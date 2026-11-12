use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rquickjs::{Context, Function, Object, Promise, Runtime, Value as JsValue};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::catalog::{namespace_for, snake_case, ServerCatalog};

pub trait ToolCaller: Send + Sync {
    fn call_tool(
        &self,
        server: &str,
        tool: &str,
        arguments: Value,
        timeout: Duration,
    ) -> std::result::Result<Value, ToolCallFailure>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NormalizedToolValue {
    Null,
    Text(String),
    Json(Value),
    Blocks(Vec<Value>),
}

impl NormalizedToolValue {
    fn into_json(self) -> Value {
        match self {
            NormalizedToolValue::Null => Value::Null,
            NormalizedToolValue::Text(text) => Value::String(text),
            NormalizedToolValue::Json(value) => value,
            NormalizedToolValue::Blocks(blocks) => Value::Array(blocks),
        }
    }
}

pub fn normalize_tool_result(value: &Value) -> std::result::Result<NormalizedToolValue, String> {
    let is_error = value
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let structured = value
        .get("structuredContent")
        .filter(|value| !value.is_null());
    if is_error {
        let mut text = content_text(value);
        if text.trim().is_empty() {
            text = "tool returned isError".to_string();
        }
        return Err(text);
    }
    if let Some(structured) = structured {
        return Ok(NormalizedToolValue::Json(structured.clone()));
    }
    let Some(content) = value.get("content").and_then(Value::as_array) else {
        return Ok(NormalizedToolValue::Null);
    };
    if content.is_empty() {
        return Ok(NormalizedToolValue::Null);
    }
    if content.len() == 1 {
        return Ok(normalize_content_block(&content[0]));
    }
    Ok(NormalizedToolValue::Blocks(
        content
            .iter()
            .map(|block| normalize_content_block(block).into_json())
            .collect(),
    ))
}

fn normalize_content_block(block: &Value) -> NormalizedToolValue {
    match block.get("type").and_then(Value::as_str) {
        Some("text") => {
            let text = block
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let trimmed = text.trim();
            if trimmed.starts_with('{') || trimmed.starts_with('[') {
                if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
                    return NormalizedToolValue::Json(value);
                }
            }
            NormalizedToolValue::Text(text.to_string())
        }
        Some("image" | "audio" | "resource") => NormalizedToolValue::Json(block.clone()),
        _ => NormalizedToolValue::Json(block.clone()),
    }
}

fn content_text(value: &Value) -> String {
    value
        .get("content")
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolCallFailureKind {
    ToolTimeout,
    Failed,
}

#[derive(Debug, Clone)]
pub struct ToolCallFailure {
    pub kind: ToolCallFailureKind,
    pub message: String,
}

impl std::fmt::Display for ToolCallFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.kind {
            ToolCallFailureKind::ToolTimeout => write!(f, "tool call timed out: {}", self.message),
            ToolCallFailureKind::Failed => write!(f, "{}", self.message),
        }
    }
}

#[derive(Debug, Clone)]
pub struct RuntimeServer {
    pub name: String,
    pub js_name: String,
    pub glue: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct LogEntry {
    pub level: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ToolCallTrace {
    pub server: String,
    pub tool: String,
    pub duration_ms: u64,
    pub ok: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ExecuteError {
    pub code: String,
    pub message: String,
    pub stack: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ExecuteOutcome {
    pub ok: bool,
    pub value: Value,
    pub logs: Vec<LogEntry>,
    pub tool_calls: Vec<ToolCallTrace>,
    pub duration_ms: u64,
    pub timeout_ms: u64,
    pub truncated: bool,
    pub error: Option<ExecuteError>,
}

#[derive(Debug, Deserialize)]
struct JsOutcome {
    ok: bool,
    #[serde(default)]
    value: Value,
    #[serde(default)]
    error: Option<ExecuteError>,
}

pub const MAX_CODE_BYTES: usize = 256 * 1024;
const MAX_RESULT_BYTES: usize = 1024 * 1024;
const MAX_TOOL_CALLS: usize = 100;
const MAX_LOGS: usize = 100;
const MAX_LOG_BYTES: usize = 64 * 1024;
const TOOL_CALL_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Serialize)]
pub struct CatalogTool {
    pub server: String,
    pub wire_server: String,
    pub namespace: String,
    pub tool: String,
    pub description: String,
    pub read_only: bool,
    pub arguments: Vec<CatalogArgument>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CatalogArgument {
    pub name: String,
    pub wire_name: String,
    pub kind: String,
    pub optional: bool,
}

pub fn tool_catalog(catalogs: &[ServerCatalog]) -> Vec<CatalogTool> {
    let mut tools = Vec::new();
    for catalog in catalogs {
        let all_names: Vec<String> = catalog.tools.iter().map(|tool| tool.name.clone()).collect();
        for tool in &catalog.tools {
            let namespace = namespace_for(
                &catalog.name,
                &tool.name,
                &all_names,
                &catalog.namespace_map,
            );
            let required: std::collections::HashSet<&str> = tool
                .input_schema
                .get("required")
                .and_then(Value::as_array)
                .map(|values| values.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            let arguments = tool
                .input_schema
                .get("properties")
                .and_then(Value::as_object)
                .map(|properties| {
                    properties
                        .iter()
                        .map(|(name, schema)| CatalogArgument {
                            name: snake_case(name),
                            wire_name: name.clone(),
                            kind: schema
                                .get("type")
                                .and_then(Value::as_str)
                                .unwrap_or("any")
                                .to_string(),
                            optional: !required.contains(name.as_str()),
                        })
                        .collect()
                })
                .unwrap_or_default();
            tools.push(CatalogTool {
                server: catalog.js_name.clone(),
                wire_server: catalog.name.clone(),
                namespace,
                tool: snake_case(&tool.name),
                description: tool.description.clone(),
                read_only: tool.read_only,
                arguments,
            });
        }
    }
    tools
}

pub fn search_catalog(
    catalogs: &[ServerCatalog],
    query: Option<&str>,
    server: Option<&str>,
    namespace: Option<&str>,
) -> Vec<CatalogTool> {
    let terms: Vec<String> = query
        .map(|query| {
            query
                .split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_')
                .filter(|term| !term.is_empty())
                .map(str::to_ascii_lowercase)
                .collect()
        })
        .unwrap_or_default();
    let tools = tool_catalog(catalogs);
    tools
        .into_iter()
        .filter(|tool| server.is_none_or(|server| tool.server.eq_ignore_ascii_case(server)))
        .filter(|tool| {
            namespace.is_none_or(|namespace| tool.namespace.eq_ignore_ascii_case(namespace))
        })
        .filter(|tool| {
            if terms.is_empty() {
                return true;
            }
            let haystack = format!(
                "{} {} {} {} {}",
                tool.server,
                tool.namespace,
                tool.tool,
                tool.description,
                tool.arguments
                    .iter()
                    .map(|argument| argument.name.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
            )
            .to_ascii_lowercase();
            terms.iter().all(|term| haystack.contains(term))
        })
        .collect()
}

pub fn execute(
    bridge: &Arc<dyn ToolCaller>,
    servers: &[RuntimeServer],
    catalogs: &[ServerCatalog],
    code: &str,
    timeout: Duration,
) -> ExecuteOutcome {
    let started = Instant::now();
    let deadline = started + timeout;
    let timeout_ms = timeout.as_millis() as u64;
    if code.len() > MAX_CODE_BYTES {
        return failure(
            "limit_exceeded",
            "code exceeds 256 KiB",
            started,
            timeout_ms,
            Vec::new(),
            Vec::new(),
        );
    }

    let logs = Arc::new(Mutex::new(Vec::<LogEntry>::new()));
    let traces = Arc::new(Mutex::new(Vec::<ToolCallTrace>::new()));
    let calls = Arc::new(AtomicUsize::new(0));
    let last_timeout = Arc::new(Mutex::new(None::<String>));

    let result = run_js(RunContext {
        bridge: bridge.clone(),
        servers: servers.to_vec(),
        catalogs: catalogs.to_vec(),
        code: code.to_string(),
        deadline,
        logs: logs.clone(),
        traces: traces.clone(),
        calls: calls.clone(),
        last_timeout: last_timeout.clone(),
    });
    let duration_ms = started.elapsed().as_millis() as u64;
    let mut logs = logs.lock().expect("sandbox log mutex poisoned").clone();
    let traces = traces.lock().expect("sandbox trace mutex poisoned").clone();
    if logs.len() > MAX_LOGS {
        logs.truncate(MAX_LOGS);
    }
    match result {
        Ok(outcome) => ExecuteOutcome {
            ok: outcome.ok,
            value: outcome.value,
            logs,
            tool_calls: traces,
            duration_ms,
            timeout_ms,
            truncated: false,
            error: outcome.error,
        },
        Err(error) => {
            let lower = error.0.to_ascii_lowercase();
            let code = if Instant::now() >= deadline {
                "execute_timeout".to_string()
            } else if last_timeout
                .lock()
                .expect("sandbox timeout mutex poisoned")
                .as_ref()
                .is_some_and(|message| error.0.contains(message))
            {
                "tool_timeout".to_string()
            } else if lower.contains("out of memory") {
                "oom".to_string()
            } else {
                "js_error".to_string()
            };
            ExecuteOutcome {
                ok: false,
                value: Value::Null,
                logs,
                tool_calls: traces,
                duration_ms,
                timeout_ms,
                truncated: false,
                error: Some(ExecuteError {
                    code,
                    message: error.0,
                    stack: error.1,
                }),
            }
        }
    }
}

struct RunContext {
    bridge: Arc<dyn ToolCaller>,
    servers: Vec<RuntimeServer>,
    catalogs: Vec<ServerCatalog>,
    code: String,
    deadline: Instant,
    logs: Arc<Mutex<Vec<LogEntry>>>,
    traces: Arc<Mutex<Vec<ToolCallTrace>>>,
    calls: Arc<AtomicUsize>,
    last_timeout: Arc<Mutex<Option<String>>>,
}

fn host_error(message: impl Into<String>) -> rquickjs::Error {
    rquickjs::Error::new_from_js_message("host", "callTool", message.into())
}

fn run_js(context: RunContext) -> std::result::Result<JsOutcome, (String, Option<String>)> {
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
        let log_fn = Function::new(ctx.clone(), move |level: String, message: String| {
            let mut logs = log_sink.lock().expect("sandbox log mutex poisoned");
            if logs.len() >= MAX_LOGS {
                return;
            }
            let used: usize = logs.iter().map(|entry| entry.message.len()).sum();
            if used + message.len() > MAX_LOG_BYTES {
                return;
            }
            logs.push(LogEntry { level, message });
        })
        .map_err(|err| (format!("create log sink: {err}"), None))?;
        globals
            .set("__xidl_log", log_fn)
            .map_err(|err| (format!("inject log sink: {err}"), None))?;

        ctx.eval::<(), _>(BOOTSTRAP)
            .map_err(|err| (format!("bootstrap JS: {err}"), None))?;
        ctx.eval::<(), _>("delete globalThis.__xidl_log;")
            .map_err(|err| (format!("remove log sink: {err}"), None))?;

        let call_bridge = bridge.clone();
        let call_traces = traces.clone();
        let call_last_timeout = last_timeout.clone();
        let call_fn = Function::new(
            ctx.clone(),
            move |call: String| -> rquickjs::Result<String> {
                let used = calls.fetch_add(1, Ordering::SeqCst);
                if used >= MAX_TOOL_CALLS {
                    return Err(host_error("tool call limit exceeded (100)"));
                }
                let now = Instant::now();
                if now >= deadline {
                    return Err(host_error("execution deadline exceeded"));
                }
                #[derive(Deserialize)]
                struct Incoming {
                    server: String,
                    tool: String,
                    params: Value,
                }
                let incoming: Incoming = serde_json::from_str(&call)
                    .map_err(|err| host_error(format!("invalid call payload: {err}")))?;
                if !incoming.params.is_object() {
                    return Err(host_error("call params must be an object"));
                }
                let remaining = deadline.saturating_duration_since(now);
                let per_call = remaining.min(TOOL_CALL_TIMEOUT);
                let call_started = Instant::now();
                let result = call_bridge.call_tool(
                    &incoming.server,
                    &incoming.tool,
                    incoming.params,
                    per_call,
                );
                let duration_ms = call_started.elapsed().as_millis() as u64;
                call_traces
                    .lock()
                    .expect("sandbox trace mutex poisoned")
                    .push(ToolCallTrace {
                        server: incoming.server.clone(),
                        tool: incoming.tool.clone(),
                        duration_ms,
                        ok: result.is_ok(),
                        error: result.as_ref().err().map(|err| err.to_string()),
                    });
                match result {
                    Ok(value) => match normalize_tool_result(&value) {
                        Ok(value) => serde_json::to_string(&value.into_json())
                            .map_err(|err| host_error(format!("serialize tool result: {err}"))),
                        Err(message) => Err(host_error(message)),
                    },
                    Err(failure) => {
                        if failure.kind == ToolCallFailureKind::ToolTimeout {
                            let message = format!("tool_timeout: {}", failure.message);
                            *call_last_timeout
                                .lock()
                                .expect("sandbox timeout mutex poisoned") = Some(message.clone());
                            return Err(host_error(message));
                        }
                        Err(host_error(failure.message))
                    }
                }
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

fn failure(
    code: &str,
    message: &str,
    started: Instant,
    timeout_ms: u64,
    logs: Vec<LogEntry>,
    traces: Vec<ToolCallTrace>,
) -> ExecuteOutcome {
    ExecuteOutcome {
        ok: false,
        value: Value::Null,
        logs,
        tool_calls: traces,
        duration_ms: started.elapsed().as_millis() as u64,
        timeout_ms,
        truncated: false,
        error: Some(ExecuteError {
            code: code.to_string(),
            message: message.to_string(),
            stack: None,
        }),
    }
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
