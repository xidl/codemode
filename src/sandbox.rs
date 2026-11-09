use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rquickjs::{Context, Function, Runtime};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::bridge::UpstreamManager;
use crate::manifest_types::Manifest;

#[derive(Debug, Clone)]
pub struct RuntimeServer {
    pub name: String,
    pub js_name: String,
    pub manifest: Manifest,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub level: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallTrace {
    pub server: String,
    pub tool: String,
    pub duration_ms: u64,
    pub ok: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecuteError {
    pub code: String,
    pub message: String,
    pub stack: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecuteOutcome {
    pub ok: bool,
    pub value: Value,
    pub logs: Vec<LogEntry>,
    pub tool_calls: Vec<ToolCallTrace>,
    pub duration_ms: u64,
    pub truncated: bool,
    pub error: Option<ExecuteError>,
}

#[derive(Debug, Clone, Serialize)]
struct JsManifest {
    servers: Vec<JsServer>,
}

#[derive(Debug, Clone, Serialize)]
struct JsServer {
    name: String,
    js_name: String,
    tools: Vec<JsTool>,
}

#[derive(Debug, Clone, Serialize)]
struct JsTool {
    js: String,
    mcp: String,
}

#[derive(Debug, Deserialize)]
struct JsOutcome {
    ok: bool,
    #[serde(default)]
    value: Value,
    #[serde(default)]
    error: Option<ExecuteError>,
}

const MAX_CODE_BYTES: usize = 256 * 1024;
const MAX_RESULT_BYTES: usize = 1024 * 1024;
const MAX_TOOL_CALLS: usize = 100;
const MAX_LOGS: usize = 100;
const MAX_LOG_BYTES: usize = 64 * 1024;

pub fn execute(
    bridge: &Arc<UpstreamManager>,
    servers: &[RuntimeServer],
    code: &str,
    timeout: Duration,
) -> ExecuteOutcome {
    let started = Instant::now();
    let deadline = started + timeout;
    if code.len() > MAX_CODE_BYTES {
        return failure(
            "limit_exceeded",
            "code exceeds 256 KiB",
            started,
            Vec::new(),
            Vec::new(),
        );
    }

    let logs = Arc::new(Mutex::new(Vec::<LogEntry>::new()));
    let traces = Arc::new(Mutex::new(Vec::<ToolCallTrace>::new()));
    let calls = Arc::new(AtomicUsize::new(0));
    let manifest_json = match serde_json::to_string(&js_manifest(servers)) {
        Ok(json) => json,
        Err(err) => {
            return failure(
                "internal",
                &format!("serialize runtime manifest: {err}"),
                started,
                Vec::new(),
                Vec::new(),
            )
        }
    };

    let result = run_js(
        bridge.clone(),
        code,
        &manifest_json,
        deadline,
        logs.clone(),
        traces.clone(),
        calls.clone(),
    );
    let duration_ms = started.elapsed().as_millis() as u64;
    let mut logs = logs.lock().map(|logs| logs.clone()).unwrap_or_default();
    let traces = traces
        .lock()
        .map(|traces| traces.clone())
        .unwrap_or_default();
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
            truncated: false,
            error: outcome.error,
        },
        Err(error) => {
            let code = if Instant::now() >= deadline {
                "timeout"
            } else {
                "js_error"
            };
            ExecuteOutcome {
                ok: false,
                value: Value::Null,
                logs,
                tool_calls: traces,
                duration_ms,
                truncated: false,
                error: Some(ExecuteError {
                    code: code.to_string(),
                    message: error.0,
                    stack: error.1,
                }),
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_js(
    bridge: Arc<UpstreamManager>,
    code: &str,
    manifest_json: &str,
    deadline: Instant,
    logs: Arc<Mutex<Vec<LogEntry>>>,
    traces: Arc<Mutex<Vec<ToolCallTrace>>>,
    calls: Arc<AtomicUsize>,
) -> std::result::Result<JsOutcome, (String, Option<String>)> {
    let runtime = Runtime::new().map_err(|err| (format!("create quickjs runtime: {err}"), None))?;
    runtime.set_memory_limit(128 * 1024 * 1024);
    runtime.set_max_stack_size(1024 * 1024);
    runtime.set_interrupt_handler(Some(Box::new(move || Instant::now() >= deadline)));
    let context =
        Context::full(&runtime).map_err(|err| (format!("create quickjs context: {err}"), None))?;

    context.with(|ctx| {
        let globals = ctx.globals();
        globals
            .set("__xidl_manifest", manifest_json)
            .map_err(|err| (format!("inject manifest: {err}"), None))?;

        let call_bridge = bridge.clone();
        let call_fn = Function::new(
            ctx.clone(),
            move |server: String,
                  tool: String,
                  args: String|
                  -> std::result::Result<String, rquickjs::Error> {
                let used = calls.fetch_add(1, Ordering::SeqCst);
                if used >= MAX_TOOL_CALLS {
                    return Err(rquickjs::Error::new_from_js_message(
                        "host",
                        "xidl_call",
                        "tool call limit exceeded (100)",
                    ));
                }
                let now = Instant::now();
                if now >= deadline {
                    return Err(rquickjs::Error::new_from_js_message(
                        "host",
                        "xidl_call",
                        "execution deadline exceeded",
                    ));
                }
                let arguments: Value =
                    serde_json::from_str(if args.is_empty() { "{}" } else { &args }).map_err(
                        |err| {
                            rquickjs::Error::new_from_js_message(
                                "host",
                                "xidl_call",
                                format!("invalid arguments JSON: {err}"),
                            )
                        },
                    )?;
                let remaining = deadline.saturating_duration_since(now);
                let call_started = Instant::now();
                let result = call_bridge.call(&server, &tool, arguments, remaining);
                let duration_ms = call_started.elapsed().as_millis() as u64;
                if let Ok(mut traces) = traces.lock() {
                    traces.push(ToolCallTrace {
                        server: server.clone(),
                        tool: tool.clone(),
                        duration_ms,
                        ok: result.is_ok(),
                        error: result.as_ref().err().map(|err| err.to_string()),
                    });
                }
                match result {
                    Ok(value) => serde_json::to_string(&value).map_err(|err| {
                        rquickjs::Error::new_from_js_message(
                            "host",
                            "xidl_call",
                            format!("serialize tool result: {err}"),
                        )
                    }),
                    Err(err) => Err(rquickjs::Error::new_from_js_message(
                        "host",
                        "xidl_call",
                        err.to_string(),
                    )),
                }
            },
        )
        .map_err(|err| (format!("create __xidl_call: {err}"), None))?;
        globals
            .set("__xidl_call", call_fn)
            .map_err(|err| (format!("inject __xidl_call: {err}"), None))?;

        let log_sink = logs.clone();
        let log_fn = Function::new(ctx.clone(), move |level: String, message: String| {
            if let Ok(mut logs) = log_sink.lock() {
                if logs.len() >= MAX_LOGS {
                    return;
                }
                let used: usize = logs.iter().map(|entry| entry.message.len()).sum();
                if used + message.len() > MAX_LOG_BYTES {
                    return;
                }
                logs.push(LogEntry { level, message });
            }
        })
        .map_err(|err| (format!("create __xidl_log: {err}"), None))?;
        globals
            .set("__xidl_log", log_fn)
            .map_err(|err| (format!("inject __xidl_log: {err}"), None))?;

        ctx.eval::<(), _>(BOOTSTRAP)
            .map_err(|err| (format!("bootstrap JS: {err}"), None))?;

        let code_json = serde_json::to_string(code)
            .map_err(|err| (format!("encode user code: {err}"), None))?;
        let wrapper = wrapper_script(&code_json, MAX_RESULT_BYTES);
        match ctx.eval::<String, _>(wrapper.as_str()) {
            Ok(raw) => serde_json::from_str(&raw)
                .map_err(|err| (format!("decode execution outcome: {err}: {raw}"), None)),
            Err(err) => {
                let caught = rquickjs::CaughtError::from_error(&ctx, err);
                let (message, stack) = match &caught {
                    rquickjs::CaughtError::Exception(exception) => (
                        exception.message().unwrap_or_else(|| exception.to_string()),
                        exception.stack(),
                    ),
                    other => (other.to_string(), None),
                };
                Err((message, stack))
            }
        }
    })
}

fn js_manifest(servers: &[RuntimeServer]) -> JsManifest {
    let servers = servers
        .iter()
        .map(|server| JsServer {
            name: server.name.clone(),
            js_name: server.js_name.clone(),
            tools: server
                .manifest
                .namespaces
                .values()
                .flat_map(|namespace| namespace.tools.iter())
                .map(|tool| JsTool {
                    js: tool.js.clone(),
                    mcp: tool.mcp.clone(),
                })
                .collect(),
        })
        .collect();
    JsManifest { servers }
}

fn wrapper_script(code_json: &str, max_result: usize) -> String {
    format!(
        r#"(function(){{
  const __src = {code_json};
  let __fn;
  try {{ __fn = new Function(__src); }} catch (__e) {{
    return JSON.stringify({{ ok:false, error:{{ code:"js_error", message:String(__e && __e.message !== undefined ? __e.message : __e), stack:__e && __e.stack ? String(__e.stack) : null }} }});
  }}
  try {{
    const __v = __fn();
    if (__v === undefined) return JSON.stringify({{ ok:true, value:null }});
    const __s = JSON.stringify(__v);
    if (__s === undefined) return JSON.stringify({{ ok:true, value:null }});
    if (__s.length > {max_result}) return JSON.stringify({{ ok:false, error:{{ code:"limit_exceeded", message:"result exceeds 1 MiB" }} }});
    return JSON.stringify({{ ok:true, value:JSON.parse(__s) }});
  }} catch (__e) {{
    return JSON.stringify({{ ok:false, error:{{ code:"js_error", message:String(__e && __e.message !== undefined ? __e.message : __e), stack:__e && __e.stack ? String(__e.stack) : null }} }});
  }}
}})()"#
    )
}

fn failure(
    code: &str,
    message: &str,
    started: Instant,
    logs: Vec<LogEntry>,
    traces: Vec<ToolCallTrace>,
) -> ExecuteOutcome {
    ExecuteOutcome {
        ok: false,
        value: Value::Null,
        logs,
        tool_calls: traces,
        duration_ms: started.elapsed().as_millis() as u64,
        truncated: false,
        error: Some(ExecuteError {
            code: code.to_string(),
            message: message.to_string(),
            stack: None,
        }),
    }
}

const BOOTSTRAP: &str = r#"
const __M = JSON.parse(__xidl_manifest);
const __mcp = Object.create(null);
for (const __server of __M.servers) {
  const __ns = Object.create(null);
  for (const __tool of __server.tools) {
    const __fn = function(args) {
      const __args = args === undefined ? "{}" : JSON.stringify(args);
      return JSON.parse(__xidl_call(__server.name, __tool.mcp, __args));
    };
    __ns[__tool.js] = __fn;
    if (__tool.mcp !== __tool.js) __ns[__tool.mcp] = __fn;
  }
  Object.freeze(__ns);
  __mcp[__server.name] = __ns;
  if (__server.js_name !== __server.name) __mcp[__server.js_name] = __ns;
}
Object.freeze(__mcp);
globalThis.mcp = __mcp;
globalThis.unwrap = function(result) {
  if (result && result.isError) {
    const text = (result.content || []).filter((item) => item.type === "text").map((item) => item.text).join("\n");
    throw new Error(text || "tool returned isError");
  }
  if (result && Object.prototype.hasOwnProperty.call(result, "structuredContent") && result.structuredContent !== undefined) {
    return result.structuredContent;
  }
  return result && result.content;
};
const __console = Object.create(null);
for (const __level of ["log", "info", "warn", "error"]) {
  __console[__level] = function(...args) {
    __xidl_log(__level, args.map((value) => typeof value === "string" ? value : JSON.stringify(value)).join(" "));
  };
}
globalThis.console = __console;
"#;
