use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use xidl_codemode::bridge::UpstreamManager;
use xidl_codemode::manifest_types::{Manifest, ManifestNamespace, ManifestParam, ManifestTool};
use xidl_codemode::sandbox::{self, RuntimeServer, ToolCaller};

fn bridge() -> Arc<dyn ToolCaller> {
    Arc::new(UpstreamManager::start().unwrap())
}

#[derive(Default)]
struct FakeCaller {
    calls: Mutex<Vec<(String, String, Value)>>,
}

impl ToolCaller for FakeCaller {
    fn call_tool(
        &self,
        server: &str,
        tool: &str,
        arguments: Value,
        _timeout: Duration,
    ) -> xidl_codemode::error::Result<Value> {
        self.calls
            .lock()
            .expect("fake caller mutex poisoned")
            .push((server.to_string(), tool.to_string(), arguments));
        Ok(json!({
            "content": [{ "type": "text", "text": "ok" }],
            "isError": false
        }))
    }
}

fn fake_server() -> RuntimeServer {
    RuntimeServer {
        name: "fake".into(),
        js_name: "fake".into(),
        manifest: Manifest {
            version: 1,
            server: "fake".into(),
            namespaces: BTreeMap::from([(
                "default".into(),
                ManifestNamespace {
                    idl: "/fake/default.idl".into(),
                    interface: "FakeDefaultApi".into(),
                    tools: vec![ManifestTool {
                        js: "echo".into(),
                        mcp: "echo".into(),
                        output: "McpToolResult".into(),
                        params: vec![
                            ManifestParam {
                                name: "first".into(),
                                wire: "first".into(),
                                required: true,
                            },
                            ManifestParam {
                                name: "second".into(),
                                wire: "second".into(),
                                required: false,
                            },
                        ],
                        read_only: true,
                        destructive: false,
                        idempotent: false,
                        open_world: false,
                    }],
                },
            )]),
        },
    }
}

#[test]
fn executes_plain_javascript() {
    let outcome = sandbox::execute(
        &bridge(),
        &[],
        "const x = 40; return x + 2;",
        Duration::from_secs(5),
    );
    assert!(outcome.ok, "{:?}", outcome.error);
    assert_eq!(outcome.value, serde_json::json!(42));
}

#[test]
fn positional_arguments_map_to_wire_object() {
    let fake = Arc::new(FakeCaller::default());
    let caller: Arc<dyn ToolCaller> = fake.clone();
    let outcome = sandbox::execute(
        &caller,
        &[fake_server()],
        "return mcp.fake.echo('hello', 7);",
        Duration::from_secs(5),
    );
    assert!(outcome.ok, "{:?}", outcome.error);
    let calls = fake.calls.lock().expect("fake caller mutex poisoned");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "fake");
    assert_eq!(calls[0].1, "echo");
    assert_eq!(calls[0].2, json!({ "first": "hello", "second": 7 }));
}

#[test]
fn optional_positional_argument_can_be_omitted() {
    let fake = Arc::new(FakeCaller::default());
    let caller: Arc<dyn ToolCaller> = fake.clone();
    let outcome = sandbox::execute(
        &caller,
        &[fake_server()],
        "return mcp.fake.echo('hello');",
        Duration::from_secs(5),
    );
    assert!(outcome.ok, "{:?}", outcome.error);
    let calls = fake.calls.lock().expect("fake caller mutex poisoned");
    assert_eq!(calls[0].2, json!({ "first": "hello" }));
}

#[test]
fn missing_required_positional_argument_fails() {
    let outcome = sandbox::execute(
        &bridge(),
        &[fake_server()],
        "return mcp.fake.echo();",
        Duration::from_secs(5),
    );
    assert!(!outcome.ok);
    let error = outcome.error.unwrap();
    assert_eq!(error.code, "js_error");
    assert!(
        error.message.contains("missing required argument first"),
        "{error:?}"
    );
}

#[test]
fn extra_positional_argument_fails() {
    let outcome = sandbox::execute(
        &bridge(),
        &[fake_server()],
        "return mcp.fake.echo('a', 1, 2);",
        Duration::from_secs(5),
    );
    assert!(!outcome.ok);
    let error = outcome.error.unwrap();
    assert_eq!(error.code, "js_error");
    assert!(error.message.contains("too many arguments"), "{error:?}");
}

#[test]
fn interrupts_infinite_loop() {
    let outcome = sandbox::execute(
        &bridge(),
        &[],
        "console.log('started'); while (true) {}",
        Duration::from_secs(1),
    );
    assert_eq!(
        outcome.logs.first().map(|entry| entry.message.as_str()),
        Some("started")
    );
    assert!(!outcome.ok);
    assert_eq!(outcome.error.as_ref().unwrap().code, "timeout");
}

#[test]
fn rejects_invalid_javascript() {
    let outcome = sandbox::execute(&bridge(), &[], "return (;", Duration::from_secs(5));
    assert!(!outcome.ok);
    assert_eq!(outcome.error.as_ref().unwrap().code, "js_error");
}

#[test]
fn rejects_oversized_code() {
    let code = format!("return '{}';", "x".repeat(256 * 1024));
    let outcome = sandbox::execute(&bridge(), &[], &code, Duration::from_secs(5));
    assert!(!outcome.ok);
    assert_eq!(outcome.error.unwrap().code, "limit_exceeded");
}

#[test]
fn rejects_oversized_result() {
    let outcome = sandbox::execute(
        &bridge(),
        &[],
        "return 'x'.repeat(1024 * 1024 + 1);",
        Duration::from_secs(5),
    );
    assert!(!outcome.ok);
    assert_eq!(outcome.error.unwrap().code, "limit_exceeded");
}

#[test]
fn reports_runtime_exception() {
    let outcome = sandbox::execute(
        &bridge(),
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
    let outcome = sandbox::execute(&bridge(), &[], "return undefined;", Duration::from_secs(5));
    assert!(outcome.ok);
    assert_eq!(outcome.value, serde_json::Value::Null);
}
