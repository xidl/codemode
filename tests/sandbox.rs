use std::time::Duration;

mod common;

use common::bridge;
use xidl_codemode::sandbox;

#[test]
fn executes_plain_javascript() {
    let outcome = sandbox::execute(
        &bridge(),
        &[],
        &[],
        "const x = 40; return x + 2;",
        Duration::from_secs(5),
    );
    assert!(outcome.ok, "{:?}", outcome.error);
    assert_eq!(outcome.value, serde_json::json!(42));
    assert_eq!(outcome.timeout_ms, 5000);
}

#[test]
fn rejects_invalid_javascript() {
    let outcome = sandbox::execute(&bridge(), &[], &[], "return (;", Duration::from_secs(5));
    assert!(!outcome.ok);
    assert_eq!(outcome.error.as_ref().unwrap().code, "js_error");
}

#[test]
fn reports_runtime_exception() {
    let outcome = sandbox::execute(
        &bridge(),
        &[],
        &[],
        "throw new Error('boom')",
        Duration::from_secs(5),
    );
    assert!(!outcome.ok);
    let error = outcome.error.unwrap();
    assert_eq!(error.code, "js_error");
    assert!(error.message.contains("boom"));
}

#[test]
fn converts_undefined_to_null() {
    let outcome = sandbox::execute(
        &bridge(),
        &[],
        &[],
        "return undefined;",
        Duration::from_secs(5),
    );
    assert!(outcome.ok);
    assert_eq!(outcome.value, serde_json::Value::Null);
}

#[test]
fn rejects_oversized_code() {
    let code = format!("return '{}';", "x".repeat(256 * 1024));
    let outcome = sandbox::execute(&bridge(), &[], &[], &code, Duration::from_secs(5));
    assert!(!outcome.ok);
    assert_eq!(outcome.error.unwrap().code, "limit_exceeded");
}

#[test]
fn rejects_oversized_result() {
    let outcome = sandbox::execute(
        &bridge(),
        &[],
        &[],
        "return 'x'.repeat(1024 * 1024 + 1);",
        Duration::from_secs(5),
    );
    assert!(!outcome.ok);
    assert_eq!(outcome.error.unwrap().code, "limit_exceeded");
}

#[test]
fn interrupts_infinite_loop() {
    let outcome = sandbox::execute(
        &bridge(),
        &[],
        &[],
        "console.log('started'); while (true) {}",
        Duration::from_secs(1),
    );
    assert_eq!(
        outcome.logs.first().map(|entry| entry.message.as_str()),
        Some("started")
    );
    assert!(!outcome.ok);
    assert_eq!(outcome.error.as_ref().unwrap().code, "execute_timeout");
}
