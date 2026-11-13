use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use rquickjs::{Context, Function, Object, Promise, Runtime, Value as JsValue};
use serde::Deserialize;
use serde_json::Value;

use crate::catalog::ServerCatalog;
use crate::runlog::RunLogger;

use super::catalog::{search_catalog, tool_catalog};

use super::call_handler::{handle_tool_call, CallDeps};

use super::{
    ExecuteError, LogEntry, RuntimeServer, ToolCallTrace, ToolCaller, MAX_LOGS, MAX_LOG_BYTES,
    MAX_RESULT_BYTES,
};

pub(super) struct RunContext {
    pub(super) bridge: Arc<dyn ToolCaller>,
    pub(super) servers: Vec<RuntimeServer>,
    pub(super) catalogs: Vec<ServerCatalog>,
    pub(super) code: String,
    pub(super) deadline: Instant,
    pub(super) logs: Arc<Mutex<Vec<LogEntry>>>,
    pub(super) traces: Arc<Mutex<Vec<ToolCallTrace>>>,
    pub(super) calls: Arc<AtomicUsize>,
    pub(super) last_timeout: Arc<Mutex<Option<String>>>,
    pub(super) run_logger: Arc<RunLogger>,
    pub(super) pending: Arc<Mutex<Option<Value>>>,
}

#[derive(Deserialize)]
pub(super) struct JsOutcome {
    pub(super) ok: bool,
    #[serde(default)]
    pub(super) value: Value,
    #[serde(default)]
    pub(super) error: Option<ExecuteError>,
}

pub(super) fn host_error(message: impl Into<String>) -> rquickjs::Error {
    rquickjs::Error::new_from_js_message("host", "callTool", message.into())
}

pub(super) fn run_js(
    context: RunContext,
) -> std::result::Result<JsOutcome, (String, Option<String>)> {
    let RunContext {
        bridge,
        servers,
        catalogs,
        code,
        deadline,
        logs,
        traces,
        calls,
        last_timeout,
        run_logger,
        pending,
    } = context;
    let runtime = Runtime::new().map_err(|err| (format!("create quickjs runtime: {err}"), None))?;
    runtime.set_memory_limit(128 * 1024 * 1024);
    runtime.set_max_stack_size(1024 * 1024);
    runtime.set_interrupt_handler(Some(Box::new(move || Instant::now() >= deadline)));
    let context =
        Context::full(&runtime).map_err(|err| (format!("create quickjs context: {err}"), None))?;

    context.with(|ctx| {
        let globals = ctx.globals();

        let log_sink = logs.clone();
        let log_run = run_logger.clone();
        let log_fn = Function::new(ctx.clone(), move |level: String, message: String| {
            let mut logs = log_sink.lock().expect("sandbox log mutex poisoned");
            if logs.len() >= MAX_LOGS {
                return;
            }
            let used: usize = logs.iter().map(|entry| entry.message.len()).sum();
            if used + message.len() > MAX_LOG_BYTES {
                return;
            }
            logs.push(LogEntry {
                level: level.clone(),
                message: message.clone(),
            });
            log_run.log_event(serde_json::json!({
                "event": "log",
                "level": level,
                "message": message,
            }));
        })
        .map_err(|err| (format!("create log sink: {err}"), None))?;
        globals
            .set("__xidl_log", log_fn)
            .map_err(|err| (format!("inject log sink: {err}"), None))?;

        ctx.eval::<(), _>(BOOTSTRAP)
            .map_err(|err| (format!("bootstrap JS: {err}"), None))?;
        ctx.eval::<(), _>("delete globalThis.__xidl_log;")
            .map_err(|err| (format!("remove log sink: {err}"), None))?;

        let call_fn = Function::new(
            ctx.clone(),
            move |call: String| -> rquickjs::Result<String> {
                handle_tool_call(
                    call,
                    &CallDeps {
                        bridge: &bridge,
                        calls: &calls,
                        deadline,
                        traces: &traces,
                        last_timeout: &last_timeout,
                        run_logger: &run_logger,
                        pending: &pending,
                    },
                )
            },
        )
        .map_err(|err| (format!("create callTool: {err}"), None))?;

        let host = Object::new(ctx.clone()).map_err(|err| (format!("create host: {err}"), None))?;
        host.set("callTool", call_fn)
            .map_err(|err| (format!("install callTool: {err}"), None))?;
        let host_value: JsValue = host.into();

        let tools =
            Object::new(ctx.clone()).map_err(|err| (format!("create tools: {err}"), None))?;
        globals
            .set("tools", tools.clone())
            .map_err(|err| (format!("inject tools: {err}"), None))?;
        for server in servers {
            let wrapped = format!(
                "(function(){{\n{}\nreturn installXidlGlue;\n}})()",
                server.glue
            );
            let install: Function = ctx
                .eval::<Function, _>(wrapped.as_str())
                .map_err(|err| (format!("evaluate glue for {}: {err}", server.name), None))?;
            let target: JsValue = tools.clone().into();
            install
                .call::<_, ()>((target, host_value.clone()))
                .map_err(|err| (format!("install glue for {}: {err}", server.name), None))?;
        }
        ctx.eval::<(), _>("Object.freeze(globalThis.tools);")
            .map_err(|err| (format!("freeze tools: {err}"), None))?;

        let catalog =
            Object::new(ctx.clone()).map_err(|err| (format!("create catalog: {err}"), None))?;
        let catalog_tools = tool_catalog(&catalogs);
        let server_tools = catalog_tools.clone();
        let servers_fn = Function::new(ctx.clone(), move || -> rquickjs::Result<String> {
            let servers: std::collections::BTreeMap<String, usize> =
                server_tools
                    .iter()
                    .fold(Default::default(), |mut acc, tool| {
                        *acc.entry(tool.server.clone()).or_default() += 1;
                        acc
                    });
            serde_json::to_string(&servers)
                .map_err(|err| host_error(format!("serialize catalog servers: {err}")))
        })
        .map_err(|err| (format!("create catalog.servers: {err}"), None))?;
        catalog
            .set("servers", servers_fn)
            .map_err(|err| (format!("install catalog.servers: {err}"), None))?;

        let search_catalogs = catalogs.clone();
        let search_fn = Function::new(
            ctx.clone(),
            move |query: String, server: String, namespace: String| -> rquickjs::Result<String> {
                let query = (!query.is_empty()).then_some(query.as_str());
                let server = (!server.is_empty()).then_some(server.as_str());
                let namespace = (!namespace.is_empty()).then_some(namespace.as_str());
                serde_json::to_string(&search_catalog(&search_catalogs, query, server, namespace))
                    .map_err(|err| host_error(format!("serialize catalog search: {err}")))
            },
        )
        .map_err(|err| (format!("create catalog.search: {err}"), None))?;
        catalog
            .set("search", search_fn)
            .map_err(|err| (format!("install catalog.search: {err}"), None))?;

        let tools_catalogs = catalogs.clone();
        let tools_fn = Function::new(
            ctx.clone(),
            move |server: String, namespace: String| -> rquickjs::Result<String> {
                let server = (!server.is_empty()).then_some(server.as_str());
                let namespace = (!namespace.is_empty()).then_some(namespace.as_str());
                serde_json::to_string(&search_catalog(&tools_catalogs, None, server, namespace))
                    .map_err(|err| host_error(format!("serialize catalog tools: {err}")))
            },
        )
        .map_err(|err| (format!("create catalog.tools: {err}"), None))?;
        catalog
            .set("tools", tools_fn)
            .map_err(|err| (format!("install catalog.tools: {err}"), None))?;

        let describe_tools = catalog_tools.clone();
        let describe_fn = Function::new(
            ctx.clone(),
            move |server: String, tool: String| -> rquickjs::Result<String> {
                let found = describe_tools
                    .iter()
                    .find(|entry| {
                        entry.server.eq_ignore_ascii_case(&server)
                            && (entry.tool.eq_ignore_ascii_case(&tool)
                                || entry.namespace.eq_ignore_ascii_case(&tool))
                    })
                    .cloned();
                serde_json::to_string(&found)
                    .map_err(|err| host_error(format!("serialize catalog describe: {err}")))
            },
        )
        .map_err(|err| (format!("create catalog.describe: {err}"), None))?;
        catalog
            .set("describe", describe_fn)
            .map_err(|err| (format!("install catalog.describe: {err}"), None))?;

        globals
            .set("catalog", catalog.clone())
            .map_err(|err| (format!("inject catalog: {err}"), None))?;
        ctx.eval::<(), _>("Object.freeze(globalThis.catalog);")
            .map_err(|err| (format!("freeze catalog: {err}"), None))?;

        let code_json = serde_json::to_string(&code)
            .map_err(|err| (format!("encode user code: {err}"), None))?;
        let wrapper = wrapper_script(&code_json, MAX_RESULT_BYTES);
        let promise: Promise = ctx
            .eval(wrapper.as_str())
            .map_err(|err| (format!("evaluate execution wrapper: {err}"), None))?;
        match promise.finish::<String>() {
            Ok(raw) => serde_json::from_str(&raw)
                .map_err(|err| (format!("decode execution outcome: {err}: {raw}"), None)),
            Err(err) => {
                let (message, stack) = if matches!(err, rquickjs::Error::WouldBlock) {
                    (
                        "await cannot make progress: no timers or async host calls are available"
                            .to_string(),
                        None,
                    )
                } else {
                    let caught = rquickjs::CaughtError::from_error(&ctx, err);
                    match &caught {
                        rquickjs::CaughtError::Exception(exception) => (
                            exception.message().unwrap_or_else(|| exception.to_string()),
                            exception.stack(),
                        ),
                        other => (other.to_string(), None),
                    }
                };
                Err((message, stack))
            }
        }
    })
}

fn wrapper_script(code_json: &str, max_result: usize) -> String {
    format!(
        r#"(async function(){{
  const __src = {code_json};
  let __fn;
  try {{ __fn = Object.getPrototypeOf(async function(){{}}).constructor(__src); }} catch (__e) {{
    return JSON.stringify({{ ok:false, error:{{ code:"js_error", message:String(__e && __e.message !== undefined ? __e.message : __e), stack:__e && __e.stack ? String(__e.stack) : null }} }});
  }}
  const __v = await __fn();
  if (__v === undefined) return JSON.stringify({{ ok:true, value:null }});
  const __s = JSON.stringify(__v);
  if (__s === undefined) return JSON.stringify({{ ok:true, value:null }});
  if (__s.length > {max_result}) return JSON.stringify({{ ok:false, error:{{ code:"limit_exceeded", message:"result exceeds 1 MiB" }} }});
  return JSON.stringify({{ ok:true, value:JSON.parse(__s) }});
}})()"#
    )
}

const BOOTSTRAP: &str = r#"
(function() {
  const __xidl_log_fn = globalThis.__xidl_log;
  const __console = Object.create(null);
  for (const __level of ["log", "info", "warn", "error"]) {
    __console[__level] = function(...args) {
      __xidl_log_fn(__level, args.map((value) => typeof value === "string" ? value : JSON.stringify(value)).join(" "));
    };
  }
  globalThis.console = __console;
})();
"#;
