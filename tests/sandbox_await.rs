use std::sync::Arc;
use std::time::Duration;

use xidl_codemode::bridge::UpstreamManager;
use xidl_codemode::sandbox;

fn bridge() -> Arc<dyn xidl_codemode::sandbox::ToolCaller> {
    Arc::new(UpstreamManager::start().unwrap())
}

#[test]
fn awaits_resolved_promise_value() {
    let outcome = sandbox::execute(
        &bridge(),
        &[],
        "return await Promise.resolve(42);",
        Duration::from_secs(5),
    );
    assert!(outcome.ok, "{:?}", outcome.error);
    assert_eq!(outcome.value, serde_json::json!(42));
}

#[test]
fn awaits_in_user_code() {
    let outcome = sandbox::execute(
        &bridge(),
        &[],
        "const value = await Promise.resolve(1); return value + 1;",
        Duration::from_secs(5),
    );
    assert!(outcome.ok, "{:?}", outcome.error);
    assert_eq!(outcome.value, serde_json::json!(2));
}

#[test]
fn awaits_pure_js_promise_all() {
    let outcome = sandbox::execute(
        &bridge(),
        &[],
        "return await Promise.all([Promise.resolve(1), Promise.resolve(2)]);",
        Duration::from_secs(5),
    );
    assert!(outcome.ok, "{:?}", outcome.error);
    assert_eq!(outcome.value, serde_json::json!([1, 2]));
}

#[test]
fn async_throw_is_reported() {
    let outcome = sandbox::execute(
        &bridge(),
        &[],
        "await Promise.resolve(); throw new Error('x');",
        Duration::from_secs(5),
    );
    assert!(!outcome.ok);
    let error = outcome.error.unwrap();
    assert_eq!(error.code, "js_error");
    assert_eq!(error.message, "x");
}

#[test]
fn blocked_promise_reports_would_block() {
    let outcome = sandbox::execute(
        &bridge(),
        &[],
        "await new Promise(() => {});",
        Duration::from_secs(5),
    );
    assert!(!outcome.ok);
    let error = outcome.error.unwrap();
    assert_eq!(error.code, "js_error");
    assert!(
        error
            .message
            .contains("await cannot make progress: no timers or async host calls are available"),
        "{error:?}"
    );
}
