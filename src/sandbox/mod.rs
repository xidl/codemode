use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::catalog::ServerCatalog;
use crate::runlog::{RunLogger, RunPaths};

mod catalog;
mod normalize;
mod runtime;

mod call_handler;

pub use catalog::{search_catalog, tool_catalog, CatalogArgument, CatalogTool};
pub use normalize::{normalize_tool_result, NormalizedToolValue};

use runtime::{run_js, RunContext};

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<RunPaths>,
}

pub const MAX_CODE_BYTES: usize = 256 * 1024;
const MAX_RESULT_BYTES: usize = 1024 * 1024;
const MAX_TOOL_CALLS: usize = 100;
const MAX_LOGS: usize = 100;
const MAX_LOG_BYTES: usize = 64 * 1024;
const TOOL_CALL_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
pub struct RunSource {
    pub kind: &'static str,
    pub path: Option<String>,
}

pub fn execute(
    bridge: &Arc<dyn ToolCaller>,
    servers: &[RuntimeServer],
    catalogs: &[ServerCatalog],
    code: &str,
    timeout: Duration,
) -> ExecuteOutcome {
    execute_with_run_logger(
        bridge,
        servers,
        catalogs,
        code,
        timeout,
        RunSource {
            kind: "code",
            path: None,
        },
        &Arc::new(RunLogger::disabled()),
    )
}

pub fn execute_with_run_logger(
    bridge: &Arc<dyn ToolCaller>,
    servers: &[RuntimeServer],
    catalogs: &[ServerCatalog],
    code: &str,
    timeout: Duration,
    source: RunSource,
    run_logger: &Arc<RunLogger>,
) -> ExecuteOutcome {
    let started = Instant::now();
    let deadline = started + timeout;
    let timeout_ms = timeout.as_millis() as u64;
    let run_paths = run_logger.paths().cloned();
    run_logger.log_event(serde_json::json!({
        "event": "execute.start",
        "source": source.kind,
        "path": source.path,
        "code_bytes": code.len(),
        "timeout_ms": timeout_ms,
    }));
    if code.len() > MAX_CODE_BYTES {
        let outcome = failure(
            "limit_exceeded",
            "code exceeds 256 KiB",
            started,
            timeout_ms,
            Vec::new(),
            Vec::new(),
        );
        return finish_run(run_logger, outcome, started);
    }

    let logs = Arc::new(Mutex::new(Vec::<LogEntry>::new()));
    let traces = Arc::new(Mutex::new(Vec::<ToolCallTrace>::new()));
    let calls = Arc::new(AtomicUsize::new(0));
    let last_timeout = Arc::new(Mutex::new(None::<String>));
    let pending: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));

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
        run_logger: run_logger.clone(),
        pending: pending.clone(),
    });
    let duration_ms = started.elapsed().as_millis() as u64;
    let mut logs = logs.lock().expect("sandbox log mutex poisoned").clone();
    let traces = traces.lock().expect("sandbox trace mutex poisoned").clone();
    if logs.len() > MAX_LOGS {
        logs.truncate(MAX_LOGS);
    }
    let outcome = match result {
        Ok(outcome) => ExecuteOutcome {
            ok: outcome.ok,
            value: outcome.value,
            logs,
            tool_calls: traces,
            duration_ms,
            timeout_ms,
            truncated: false,
            error: outcome.error,
            run: run_paths.clone(),
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
            let phase = match code.as_str() {
                "tool_timeout" => "tool_call",
                _ => "javascript",
            };
            if matches!(code.as_str(), "execute_timeout" | "tool_timeout") {
                run_logger.log_event(serde_json::json!({
                    "event": "timeout",
                    "phase": phase,
                    "pending_call": pending.lock().expect("sandbox pending mutex poisoned").clone(),
                    "last_log": logs.last(),
                    "error_stack": error.1,
                }));
            }
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
                run: run_paths.clone(),
            }
        }
    };
    finish_run(run_logger, outcome, started)
}

fn finish_run(
    run_logger: &Arc<RunLogger>,
    outcome: ExecuteOutcome,
    started: Instant,
) -> ExecuteOutcome {
    let duration_ms = outcome.duration_ms;
    let error_code = outcome.error.as_ref().map(|error| error.code.clone());
    let tool_call_ms: u64 = outcome.tool_calls.iter().map(|call| call.duration_ms).sum();
    let call_count = outcome.tool_calls.len();
    let error_count = outcome.tool_calls.iter().filter(|call| !call.ok).count();
    run_logger.log_event(serde_json::json!({
        "event": "execute.end",
        "ok": outcome.ok,
        "duration_ms": duration_ms,
        "error_code": error_code,
        "stack": outcome.error.as_ref().and_then(|error| error.stack.clone()),
    }));
    run_logger.log_metric(serde_json::json!({
        "metric": "execute",
        "duration_ms": duration_ms,
        "timeout_ms": outcome.timeout_ms,
        "tool_call_ms": tool_call_ms,
        "call_count": call_count,
        "error_count": error_count,
        "ok": outcome.ok,
        "error_code": error_code,
    }));
    run_logger.write_summary(&serde_json::json!({
        "ok": outcome.ok,
        "error_code": error_code,
        "duration_ms": duration_ms,
        "tool_call_count": call_count,
        "elapsed_until_summary_ms": started.elapsed().as_millis() as u64,
    }));
    outcome
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
        run: None,
    }
}
