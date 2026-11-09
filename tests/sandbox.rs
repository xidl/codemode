use std::sync::Arc;
use std::time::Duration;

use xidl_codemode::bridge::UpstreamManager;
use xidl_codemode::sandbox;

#[test]
fn executes_plain_javascript() {
    let bridge = Arc::new(UpstreamManager::start(Vec::new()).unwrap());
    let outcome = sandbox::execute(
        &bridge,
        &[],
        "const x = 40; return x + 2;",
        Duration::from_secs(5),
    );
    assert!(outcome.ok, "{:?}", outcome.error);
    assert_eq!(outcome.value, serde_json::json!(42));
}

#[test]
fn interrupts_infinite_loop() {
    let bridge = Arc::new(UpstreamManager::start(Vec::new()).unwrap());
    let outcome = sandbox::execute(&bridge, &[], "while (true) {}", Duration::from_millis(300));
    assert!(!outcome.ok);
    assert_eq!(outcome.error.as_ref().unwrap().code, "timeout");
}

#[test]
fn rejects_invalid_javascript() {
    let bridge = Arc::new(UpstreamManager::start(Vec::new()).unwrap());
    let outcome = sandbox::execute(&bridge, &[], "return (;", Duration::from_secs(5));
    assert!(!outcome.ok);
    assert_eq!(outcome.error.as_ref().unwrap().code, "js_error");
}
