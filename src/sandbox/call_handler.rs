use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::Deserialize;
use serde_json::Value;

use crate::runlog::{preview, RunLogger};

use super::normalize::normalize_tool_result;
use super::runtime::host_error;
use super::{ToolCallFailureKind, ToolCallTrace, ToolCaller, MAX_TOOL_CALLS, TOOL_CALL_TIMEOUT};

pub(super) struct CallDeps<'a> {
    pub(super) bridge: &'a Arc<dyn ToolCaller>,
    pub(super) calls: &'a Arc<AtomicUsize>,
    pub(super) deadline: Instant,
    pub(super) traces: &'a Arc<Mutex<Vec<ToolCallTrace>>>,
    pub(super) last_timeout: &'a Arc<Mutex<Option<String>>>,
    pub(super) run_logger: &'a Arc<RunLogger>,
    pub(super) pending: &'a Arc<Mutex<Option<Value>>>,
}

struct ToolCallStats<'a> {
    index: usize,
    server: &'a str,
    tool: &'a str,
    duration_ms: u64,
    request_bytes: usize,
}

fn log_call_error(
    run_logger: &RunLogger,
    stats: &ToolCallStats<'_>,
    status: &'static str,
    message: Option<&str>,
) {
    run_logger.log_event(serde_json::json!({
        "event": "tool_call.end",
        "index": stats.index,
        "server": stats.server,
        "tool": stats.tool,
        "duration_ms": stats.duration_ms,
        "ok": false,
        "status": status,
        "error": message,
        "result_bytes": 0,
    }));
    run_logger.log_metric(serde_json::json!({
        "metric": "tool_call",
        "index": stats.index,
        "server": stats.server,
        "tool": stats.tool,
        "duration_ms": stats.duration_ms,
        "ok": false,
        "status": status,
        "request_bytes": stats.request_bytes,
        "result_bytes": 0,
        "result_kind": "error",
    }));
}

fn value_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

pub(super) fn handle_tool_call(call: String, deps: &CallDeps<'_>) -> rquickjs::Result<String> {
    let CallDeps {
        bridge,
        calls,
        deadline,
        traces,
        last_timeout,
        run_logger,
        pending,
    } = *deps;
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
    let index = used;
    let args_preview = preview(&incoming.params);
    let request_bytes = serde_json::to_string(&incoming.params)
        .map(|text| text.len())
        .unwrap_or(0);
    run_logger.log_event(serde_json::json!({
        "event": "tool_call.start",
        "index": index,
        "server": incoming.server.clone(),
        "tool": incoming.tool.clone(),
        "args": args_preview,
    }));
    *pending.lock().expect("sandbox pending mutex poisoned") = Some(serde_json::json!({
        "index": index,
        "server": incoming.server.clone(),
        "tool": incoming.tool.clone(),
    }));
    let remaining = deadline.saturating_duration_since(now);
    let per_call = remaining.min(TOOL_CALL_TIMEOUT);
    let call_started = Instant::now();
    let result = bridge.call_tool(&incoming.server, &incoming.tool, incoming.params, per_call);
    let duration_ms = call_started.elapsed().as_millis() as u64;
    traces
        .lock()
        .expect("sandbox trace mutex poisoned")
        .push(ToolCallTrace {
            server: incoming.server.clone(),
            tool: incoming.tool.clone(),
            duration_ms,
            ok: result.is_ok(),
            error: result.as_ref().err().map(|err| err.to_string()),
        });
    let end_event = match &result {
        Ok(value) => match normalize_tool_result(value) {
            Ok(normalized) => {
                let value = normalized.into_json();
                let result_bytes = serde_json::to_string(&value)
                    .map(|text| text.len())
                    .unwrap_or(0);
                run_logger.log_event(serde_json::json!({
                    "event": "tool_call.end",
                    "index": index,
                    "server": incoming.server.clone(),
                    "tool": incoming.tool.clone(),
                    "duration_ms": duration_ms,
                    "ok": true,
                    "status": "ok",
                    "result": preview(&value),
                    "result_bytes": result_bytes,
                }));
                run_logger.log_metric(serde_json::json!({
                    "metric": "tool_call",
                    "index": index,
                    "server": incoming.server.clone(),
                    "tool": incoming.tool.clone(),
                    "duration_ms": duration_ms,
                    "ok": true,
                    "status": "ok",
                    "request_bytes": request_bytes,
                    "result_bytes": result_bytes,
                    "result_kind": value_kind(&value),
                }));
                serde_json::to_string(&value)
                    .map_err(|err| host_error(format!("serialize tool result: {err}")))
            }
            Err(message) => {
                log_call_error(
                    run_logger,
                    &ToolCallStats {
                        index,
                        server: &incoming.server,
                        tool: &incoming.tool,
                        duration_ms,
                        request_bytes,
                    },
                    "error",
                    Some(&message),
                );
                Err(host_error(message))
            }
        },
        Err(failure) => {
            let status = if failure.kind == ToolCallFailureKind::ToolTimeout {
                "timed_out"
            } else {
                "error"
            };
            log_call_error(
                run_logger,
                &ToolCallStats {
                    index,
                    server: &incoming.server,
                    tool: &incoming.tool,
                    duration_ms,
                    request_bytes,
                },
                status,
                Some(&failure.message),
            );
            if failure.kind == ToolCallFailureKind::ToolTimeout {
                let message = format!("tool_timeout: {}", failure.message);
                *last_timeout.lock().expect("sandbox timeout mutex poisoned") =
                    Some(message.clone());
                Err(host_error(message))
            } else {
                Err(host_error(failure.message.clone()))
            }
        }
    };
    *pending.lock().expect("sandbox pending mutex poisoned") = None;
    end_event
}
