use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use xidl_codemode::sandbox::{self, RuntimeServer, ToolCallFailure, ToolCaller};

#[derive(Clone)]
struct FixedCaller(Value);

impl ToolCaller for FixedCaller {
    fn call_tool(
        &self,
        _server: &str,
        _tool: &str,
        _arguments: Value,
        _timeout: Duration,
    ) -> Result<Value, ToolCallFailure> {
        Ok(self.0.clone())
    }
}

const GLUE: &str = r#"
function installXidlGlue(target, host) {
  const ns = Object.create(null);
  ns.echo = function(input) {
    return JSON.parse(host.callTool(JSON.stringify({server:'fake', tool:'echo', params:{}})));
  };
  const server = Object.create(null);
  server.fake = ns;
  Object.defineProperty(target, 'fake', { value: server.fake, enumerable: true });
}
"#;

fn server() -> RuntimeServer {
    RuntimeServer {
        name: "fake".into(),
        js_name: "fake".into(),
        glue: GLUE.into(),
    }
}

fn execute(result: Value) -> xidl_codemode::sandbox::ExecuteOutcome {
    let caller: Arc<dyn ToolCaller> = Arc::new(FixedCaller(result));
    sandbox::execute(
        &caller,
        &[server()],
        &[],
        "return tools.fake.echo();",
        Duration::from_secs(5),
    )
}

#[test]
fn structured_content_is_returned_directly() {
    let outcome = execute(json!({
        "content": [{"type": "text", "text": "raw"}],
        "structuredContent": {"value": 42}
    }));
    assert!(outcome.ok, "{:?}", outcome.error);
    assert_eq!(outcome.value, json!({"value": 42}));
}

#[test]
fn single_text_block_is_returned_as_string() {
    let outcome = execute(json!({"content": [{"type": "text", "text": "ok"}]}));
    assert!(outcome.ok, "{:?}", outcome.error);
    assert_eq!(outcome.value, json!("ok"));
}

#[test]
fn json_object_text_is_unwrapped() {
    let outcome = execute(json!({
        "content": [{"type": "text", "text": "{\"number\": 361, \"title\": \"hi\"}"}]
    }));
    assert!(outcome.ok, "{:?}", outcome.error);
    assert_eq!(outcome.value, json!({"number": 361, "title": "hi"}));
}

#[test]
fn json_array_text_is_unwrapped() {
    let outcome = execute(json!({
        "content": [{"type": "text", "text": "  [1, 2, 3]  "}]
    }));
    assert!(outcome.ok, "{:?}", outcome.error);
    assert_eq!(outcome.value, json!([1, 2, 3]));
}

#[test]
fn invalid_json_text_stays_string() {
    let outcome = execute(json!({
        "content": [{"type": "text", "text": "{oops"}]
    }));
    assert!(outcome.ok, "{:?}", outcome.error);
    assert_eq!(outcome.value, json!("{oops"));
}

#[test]
fn scalar_and_string_json_stay_strings() {
    for text in ["42", "true", "\"hello\"", "-3.5"] {
        let outcome = execute(json!({
            "content": [{"type": "text", "text": text}]
        }));
        assert!(outcome.ok, "{text}");
        assert_eq!(outcome.value, json!(text));
    }
}

#[test]
fn multi_block_text_json_unwraps_per_block() {
    let outcome = execute(json!({
        "content": [
            {"type": "text", "text": "{\"a\": 1}"},
            {"type": "text", "text": "plain"},
            {"type": "text", "text": "[2]"}
        ]
    }));
    assert!(outcome.ok, "{:?}", outcome.error);
    assert_eq!(outcome.value, json!([{"a": 1}, "plain", [2]]));
}

#[test]
fn multiple_blocks_are_normalized() {
    let outcome = execute(json!({
        "content": [
            {"type": "text", "text": "one"},
            {"type": "text", "text": "two"},
            {"type": "image", "data": "abc", "mimeType": "image/png"}
        ]
    }));
    assert!(outcome.ok, "{:?}", outcome.error);
    assert_eq!(
        outcome.value,
        json!(["one", "two", {"type": "image", "data": "abc", "mimeType": "image/png"}])
    );
}

#[test]
fn empty_content_is_null() {
    let outcome = execute(json!({"content": []}));
    assert!(outcome.ok, "{:?}", outcome.error);
    assert_eq!(outcome.value, Value::Null);
}

#[test]
fn missing_content_is_null() {
    let outcome = execute(json!({}));
    assert!(outcome.ok, "{:?}", outcome.error);
    assert_eq!(outcome.value, Value::Null);
}

#[test]
fn tool_error_becomes_js_error() {
    let outcome = execute(json!({
        "content": [{"type": "text", "text": "bad"}],
        "isError": true
    }));
    assert!(!outcome.ok);
    let error = outcome.error.unwrap();
    assert_eq!(error.code, "js_error");
    assert!(error.message.contains(": bad"), "{error:?}");
}

#[test]
fn structured_error_attaches_diagnostic_data() {
    let outcome = execute(json!({
        "content": [{"type": "text", "text": "bad"}],
        "structuredContent": {"detail": "broken"},
        "isError": true
    }));
    assert!(!outcome.ok);
    let error = outcome.error.unwrap();
    assert_eq!(error.code, "js_error");
    assert!(error.message.contains(": bad"), "{error:?}");
}
