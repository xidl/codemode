use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rquickjs::{Context, Function, Object, Runtime, Value as JsValue};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub trait ToolCaller: Send + Sync {
    fn call_tool(
        &self,
        server: &str,
        tool: &str,
        arguments: Value,
        timeout: Duration,
    ) -> std::result::Result<Value, ToolCallFailure>;
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

const MAX_CODE_BYTES: usize = 256 * 1024;
const MAX_RESULT_BYTES: usize = 1024 * 1024;
const MAX_TOOL_CALLS: usize = 100;
const MAX_LOGS: usize = 100;
const MAX_LOG_BYTES: usize = 64 * 1024;
const TOOL_CALL_TIMEOUT: Duration = Duration::from_secs(10);

pub fn execute(
    bridge: &Arc<dyn ToolCaller>,
    servers: &[RuntimeServer],
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
                    Ok(value) => {
                        let json = serde_json::to_string(&value)
                            .map_err(|err| host_error(format!("serialize tool result: {err}")))?;
                        Ok(json)
                    }
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

        let mcp = Object::new(ctx.clone()).map_err(|err| (format!("create mcp: {err}"), None))?;
        globals
            .set("mcp", mcp.clone())
            .map_err(|err| (format!("inject mcp: {err}"), None))?;
        for server in servers {
            let wrapped = format!(
                "(function(){{\n{}\nreturn installXidlGlue;\n}})()",
                server.glue
            );
            let install: Function = ctx
                .eval::<Function, _>(wrapped.as_str())
                .map_err(|err| (format!("evaluate glue for {}: {err}", server.name), None))?;
            let mcp_value: JsValue = mcp.clone().into();
            install
                .call::<_, ()>((mcp_value, host_value.clone()))
                .map_err(|err| (format!("install glue for {}: {err}", server.name), None))?;
        }
        ctx.eval::<(), _>("Object.freeze(globalThis.mcp);")
            .map_err(|err| (format!("freeze mcp: {err}"), None))?;

        let code_json = serde_json::to_string(&code)
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

fn wrapper_script(code_json: &str, max_result: usize) -> String {
    format!(
        r#"(function(){{
  const __src = {code_json};
  let __fn;
  try {{ __fn = new Function(__src); }} catch (__e) {{
    return JSON.stringify({{ ok:false, error:{{ code:"js_error", message:String(__e && __e.message !== undefined ? __e.message : __e), stack:__e && __e.stack ? String(__e.stack) : null }} }});
  }}
  const __v = __fn();
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
      __xidl_log_fn(__level, args.map((value) => typeof value === "string" ? value : JSON.stringify(value)).join(" "));
    };
  }
  globalThis.console = __console;
})();
"#;
