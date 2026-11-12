use serde_json::json;
use xidl_codemode::sandbox::{ExecuteError, ExecuteOutcome};
use xidl_codemode::server::execute_call_result;

fn outcome(ok: bool, error: Option<ExecuteError>) -> ExecuteOutcome {
    ExecuteOutcome {
        ok,
        value: if ok {
            json!({"answer": 42})
        } else {
            json!(null)
        },
        logs: vec![],
        tool_calls: vec![],
        duration_ms: 12,
        timeout_ms: 5000,
        truncated: false,
        error,
        run: None,
    }
}

#[test]
fn success_carries_structured_content_and_summary() {
    let result = execute_call_result(&outcome(true, None));
    assert_eq!(result.is_error, Some(false));
    assert_eq!(
        result.structured_content,
        Some(json!({
            "ok": true,
            "value": {"answer": 42},
            "logs": [],
            "tool_calls": [],
            "duration_ms": 12,
            "timeout_ms": 5000,
            "truncated": false,
            "error": null
        }))
    );
    let text = serde_json::to_value(&result.content).unwrap();
    assert_eq!(text[0]["type"], "text");
    assert!(text[0]["text"]
        .as_str()
        .unwrap()
        .contains("execute ok (12 ms, 0 tool calls)"));
    assert!(text[0]["text"]
        .as_str()
        .unwrap()
        .contains("structuredContent"));
}

#[test]
fn failure_summary_includes_code_and_message() {
    let result = execute_call_result(&outcome(
        false,
        Some(ExecuteError {
            code: "js_error".into(),
            message: "boom".into(),
            stack: None,
        }),
    ));
    assert_eq!(result.is_error, Some(false));
    let text = serde_json::to_value(&result.content).unwrap();
    assert!(text[0]["text"]
        .as_str()
        .unwrap()
        .contains("execute failed: boom (js_error, 12 ms)"));
}
