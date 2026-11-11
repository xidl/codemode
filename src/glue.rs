use std::collections::{HashMap, HashSet};
use std::path::Path;

use rquickjs::{Context, Function, Object, Runtime};
use serde::Deserialize;

use crate::catalog::{is_valid_ident, sanitize_ident, ServerCatalog};
use crate::error::{Error, Result};

#[derive(Debug)]
pub struct Glue {
    pub js: String,
    pub dts: String,
}

pub fn generate(server: &str, bundle: &str) -> Result<Glue> {
    let path = Path::new(server).with_extension("bundle.idl");
    let mut generator = xidlc::driver::Generator::new("codemode-glue".to_string());
    let files = generator
        .generate_from_idl(bundle, &path, HashMap::new())
        .map_err(|err| Error::Glue(format!("xidlc codemode-glue failed: {err}")))?;
    let js = files
        .iter()
        .find(|file| file.path().ends_with("glue.js"))
        .map(|file| file.content().to_string())
        .ok_or_else(|| Error::Glue("plugin did not emit glue.js".into()))?;
    let dts = files
        .iter()
        .find(|file| file.path().ends_with("glue.d.ts"))
        .map(|file| file.content().to_string())
        .ok_or_else(|| Error::Glue("plugin did not emit glue.d.ts".into()))?;
    Ok(Glue { js, dts })
}

#[derive(Debug, Deserialize)]
struct AuditReport {
    name: String,
    tools: Vec<String>,
}

pub fn audit(js: &str, catalog: &ServerCatalog) -> Result<()> {
    let runtime =
        Runtime::new().map_err(|err| Error::Glue(format!("create quickjs runtime: {err}")))?;
    runtime.set_memory_limit(64 * 1024 * 1024);
    runtime.set_max_stack_size(1024 * 1024);
    let context = Context::full(&runtime)
        .map_err(|err| Error::Glue(format!("create quickjs context: {err}")))?;
    context.with(|ctx| -> Result<()> {
        let host = Object::new(ctx.clone())
            .map_err(|err| Error::Glue(format!("create audit host: {err}")))?;
        let deny = Function::new(ctx.clone(), |_: String| -> rquickjs::Result<String> {
            Err(rquickjs::Error::new_from_js_message(
                "host",
                "callTool",
                "tool call during glue audit",
            ))
        })
        .map_err(|err| Error::Glue(format!("create audit callTool: {err}")))?;
        host.set("callTool", deny)
            .map_err(|err| Error::Glue(format!("install audit callTool: {err}")))?;

        let wrapped = format!("(function(){{\n{js}\nreturn installXidlGlue;\n}})()");
        let install: Function = ctx
            .eval::<Function, _>(wrapped.as_str())
            .map_err(|err| Error::Glue(format!("glue is missing installXidlGlue: {err}")))?;
        let mcp = Object::new(ctx.clone())
            .map_err(|err| Error::Glue(format!("create probe target: {err}")))?;
        ctx.globals()
            .set("__xidl_probe", mcp.clone())
            .map_err(|err| Error::Glue(format!("install probe target: {err}")))?;
        let mcp_value: rquickjs::Value = mcp.clone().into();
        let host_value: rquickjs::Value = host.into();
        install
            .call::<_, ()>((mcp_value, host_value))
            .map_err(|err| Error::Glue(format!("installXidlGlue failed: {err}")))?;
        let raw: String = ctx
            .eval(
                "JSON.stringify(Object.entries(globalThis.__xidl_probe).map(([name, ns]) => ({ name, tools: Object.getOwnPropertyNames(ns) })))",
            )
            .map_err(|err| Error::Glue(format!("inspect probe target: {err}")))?;
        let reports: Vec<AuditReport> = serde_json::from_str(&raw)
            .map_err(|err| Error::Glue(format!("invalid probe report: {err}")))?;

        let mut expected_servers = HashSet::new();
        expected_servers.insert(catalog.name.clone());
        expected_servers.insert(catalog.js_name.clone());
        let mut expected_tools = HashSet::new();
        for tool in &catalog.tools {
            let js_name = if is_valid_ident(&tool.name) {
                tool.name.clone()
            } else {
                sanitize_ident(&tool.name)
            };
            expected_tools.insert(js_name);
            expected_tools.insert(tool.name.clone());
        }

        let mut seen_servers = HashSet::new();
        for report in reports {
            if !expected_servers.contains(&report.name) {
                return Err(Error::Glue(format!(
                    "glue registers unknown server key {:?}",
                    report.name
                )));
            }
            if !seen_servers.insert(report.name.clone()) {
                return Err(Error::Glue(format!(
                    "glue registers server key {:?} twice",
                    report.name
                )));
            }
            let keys: HashSet<String> = report.tools.into_iter().collect();
            if keys != expected_tools {
                let missing: Vec<&String> = expected_tools.difference(&keys).collect();
                let extra: Vec<&String> = keys.difference(&expected_tools).collect();
                return Err(Error::Glue(format!(
                    "glue tool keys do not match catalog; missing {missing:?}, extra {extra:?}"
                )));
            }
        }
        if seen_servers != expected_servers {
            return Err(Error::Glue(format!(
                "glue registers servers {seen_servers:?}; expected {expected_servers:?}"
            )));
        }
        Ok(())
    })
}
