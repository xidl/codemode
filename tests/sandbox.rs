use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use xidl_codemode::bridge::UpstreamManager;
use xidl_codemode::sandbox::{
    self, RuntimeServer, ToolCallFailure, ToolCallFailureKind, ToolCaller,
};

fn bridge() -> Arc<dyn ToolCaller> {
    Arc::new(UpstreamManager::start().unwrap())
}

#[derive(Default)]
struct FakeCaller {
    calls: Mutex<Vec<(String, String, Value)>>,
    failure: Option<ToolCallFailureKind>,
}

impl FakeCaller {
    fn ok() -> Arc<FakeCaller> {
        Arc::new(FakeCaller::default())
    }

    fn failing(kind: ToolCallFailureKind) -> Arc<FakeCaller> {
        Arc::new(FakeCaller {
            calls: Mutex::new(Vec::new()),
            failure: Some(kind),
        })
    }
}

impl ToolCaller for FakeCaller {
    fn call_tool(
        &self,
        server: &str,
        tool: &str,
        arguments: Value,
        _timeout: Duration,
    ) -> Result<Value, ToolCallFailure> {
        self.calls
            .lock()
            .expect("fake caller mutex poisoned")
            .push((server.to_string(), tool.to_string(), arguments));
        if let Some(kind) = self.failure {
            return Err(ToolCallFailure {
                kind,
                message: "fake failure".into(),
            });
        }
        Ok(json!({
            "content": [{ "type": "text", "text": "ok" }],
            "isError": false
        }))
    }
}

const FAKE_GLUE: &str = r#"
function installXidlGlue(target, host) {
  const __ns = Object.create(null);
  __ns["echo"] = function(input) {
    if (input !== undefined && (typeof input !== "object" || input === null || Array.isArray(input))) {
      throw new Error("invalid arguments for fake.echo: expected a single object argument");
    }
    const __params = Object.create(null);
    {
      const __value = input !== undefined && Object.prototype.hasOwnProperty.call(input, "first") ? input["first"] : undefined;
      if (__value === undefined) { throw new Error("missing required argument first for fake.echo"); }
      __params["first_wire"] = __value;
    }
    {
      const __value = input !== undefined && Object.prototype.hasOwnProperty.call(input, "second") ? input["second"] : undefined;
      if (__value !== undefined) { __params["second_wire"] = __value; }
    }
    {
      const __value = input !== undefined && Object.prototype.hasOwnProperty.call(input, "proto_key") ? input["proto_key"] : undefined;
      if (__value !== undefined) { __params["__proto__"] = __value; }
    }
    for (const __key of Object.keys(input ?? {})) {
      if (__key !== "first" && __key !== "second" && __key !== "proto_key") {
        throw new Error("unknown argument " + __key + " for fake.echo (expected: first, second, proto_key)");
      }
    }
    return JSON.parse(host.callTool(JSON.stringify({ server: "fake", tool: "echo", params: __params })));
  };
  Object.freeze(__ns);
  const __server = Object.create(null);
  __server["fake"] = __ns;
  Object.freeze(__server);
  for (const [key, value] of Object.entries(__server)) {
    Object.defineProperty(target, key, { value: value, enumerable: true });
  }
}
"#;

fn fake_server() -> RuntimeServer {
    RuntimeServer {
        name: "fake".into(),
        js_name: "fake".into(),
        glue: FAKE_GLUE.into(),
    }
}

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
fn object_arguments_map_to_wire_object() {
    let fake = FakeCaller::ok();
    let caller: Arc<dyn ToolCaller> = fake.clone();
    let outcome = sandbox::execute(
        &caller,
        &[fake_server()],
        &[],
        "return tools.fake.echo({ first: 'hello', second: 7 });",
        Duration::from_secs(5),
    );
    assert!(outcome.ok, "{:?}", outcome.error);
    let calls = fake.calls.lock().expect("fake caller mutex poisoned");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "fake");
    assert_eq!(calls[0].1, "echo");
    assert_eq!(
        calls[0].2,
        json!({ "first_wire": "hello", "second_wire": 7 })
    );
}

#[test]
fn optional_object_field_can_be_omitted() {
    let fake = FakeCaller::ok();
    let caller: Arc<dyn ToolCaller> = fake.clone();
    let outcome = sandbox::execute(
        &caller,
        &[fake_server()],
        &[],
        "return tools.fake.echo({ first: 'hello' });",
        Duration::from_secs(5),
    );
    assert!(outcome.ok, "{:?}", outcome.error);
    let calls = fake.calls.lock().expect("fake caller mutex poisoned");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].2, json!({ "first_wire": "hello" }));
}

#[test]
fn explicit_null_fields_pass_through() {
    let fake = FakeCaller::ok();
    let caller: Arc<dyn ToolCaller> = fake.clone();
    let outcome = sandbox::execute(
        &caller,
        &[fake_server()],
        &[],
        "return tools.fake.echo({ first: 'hello', second: null });",
        Duration::from_secs(5),
    );
    assert!(outcome.ok, "{:?}", outcome.error);
    let calls = fake.calls.lock().expect("fake caller mutex poisoned");
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].2,
        json!({ "first_wire": "hello", "second_wire": Value::Null })
    );
}

#[test]
fn required_null_field_is_passed() {
    let fake = FakeCaller::ok();
    let caller: Arc<dyn ToolCaller> = fake.clone();
    let outcome = sandbox::execute(
        &caller,
        &[fake_server()],
        &[],
        "return tools.fake.echo({ first: null });",
        Duration::from_secs(5),
    );
    assert!(outcome.ok, "{:?}", outcome.error);
    let calls = fake.calls.lock().expect("fake caller mutex poisoned");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].2, json!({ "first_wire": Value::Null }));
}

#[test]
fn required_undefined_field_fails() {
    let fake = FakeCaller::ok();
    let caller: Arc<dyn ToolCaller> = fake.clone();
    let outcome = sandbox::execute(
        &caller,
        &[fake_server()],
        &[],
        "return tools.fake.echo({ first: undefined });",
        Duration::from_secs(5),
    );
    assert!(!outcome.ok);
    let error = outcome.error.unwrap();
    assert!(
        error.message.contains("missing required argument first"),
        "{error:?}"
    );
    assert!(fake
        .calls
        .lock()
        .expect("fake caller mutex poisoned")
        .is_empty());
}

#[test]
fn missing_required_field_fails() {
    let fake = FakeCaller::ok();
    let caller: Arc<dyn ToolCaller> = fake.clone();
    let outcome = sandbox::execute(
        &caller,
        &[fake_server()],
        &[],
        "return tools.fake.echo({});",
        Duration::from_secs(5),
    );
    assert!(!outcome.ok);
    let error = outcome.error.unwrap();
    assert_eq!(error.code, "js_error");
    assert!(
        error.message.contains("missing required argument first"),
        "{error:?}"
    );
    assert!(fake
        .calls
        .lock()
        .expect("fake caller mutex poisoned")
        .is_empty());
}

#[test]
fn unknown_object_key_fails() {
    let fake = FakeCaller::ok();
    let caller: Arc<dyn ToolCaller> = fake.clone();
    let outcome = sandbox::execute(
        &caller,
        &[fake_server()],
        &[],
        "return tools.fake.echo({ first: 'a', extra: 1 });",
        Duration::from_secs(5),
    );
    assert!(!outcome.ok);
    let error = outcome.error.unwrap();
    assert_eq!(error.code, "js_error");
    assert!(
        error.message.contains("unknown argument extra"),
        "{error:?}"
    );
    assert!(fake
        .calls
        .lock()
        .expect("fake caller mutex poisoned")
        .is_empty());
}

#[test]
fn non_object_argument_fails() {
    for source in [
        "return tools.fake.echo(['a']);",
        "return tools.fake.echo('x');",
        "return tools.fake.echo(null);",
    ] {
        let outcome = sandbox::execute(
            &bridge(),
            &[fake_server()],
            &[],
            source,
            Duration::from_secs(5),
        );
        assert!(!outcome.ok, "{source}");
        let error = outcome.error.unwrap();
        assert!(
            error.message.contains("expected a single object argument"),
            "{error:?}"
        );
    }
}

#[test]
fn hides_host_internals_from_user_code() {
    let outcome = sandbox::execute(
        &bridge(),
        &[fake_server()],
        &[],
        "return [typeof __xidl_call, typeof __xidl_manifest, typeof __xidl_log, typeof host, typeof __xidl_log_fn, typeof __console].join(',');",
        Duration::from_secs(5),
    );
    assert!(outcome.ok, "{:?}", outcome.error);
    assert_eq!(
        outcome.value,
        serde_json::json!("undefined,undefined,undefined,undefined,undefined,undefined")
    );
}

#[test]
fn snake_case_server_namespace_routes_wire_name() {
    let glue = r#"
function installXidlGlue(target, host) {
  const ns = Object.create(null);
  ns["open"] = function(input) {
    return JSON.parse(host.callTool(JSON.stringify({server:"agent-browser", tool:"open", params:{}})));
  };
  const server = Object.create(null);
  server["agent_browser"] = ns;
  for (const [key, value] of Object.entries(server)) {
    Object.defineProperty(target, key, { value, enumerable: true });
  }
}
"#;
    let server = RuntimeServer {
        name: "agent-browser".into(),
        js_name: "agent_browser".into(),
        glue: glue.into(),
    };
    let fake = FakeCaller::ok();
    let caller: Arc<dyn ToolCaller> = fake.clone();
    let outcome = sandbox::execute(
        &caller,
        &[server],
        &[],
        "return [typeof tools.agent_browser, typeof tools['agent-browser'], typeof tools.agent_browser.open()].join(',');",
        Duration::from_secs(5),
    );
    assert!(outcome.ok, "{:?}", outcome.error);
    assert_eq!(outcome.value, serde_json::json!("object,undefined,string"));
    let calls = fake.calls.lock().expect("fake caller mutex poisoned");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "agent-browser");
}

#[test]
fn tools_object_is_frozen() {
    let outcome = sandbox::execute(
        &bridge(),
        &[fake_server()],
        &[],
        "return Object.isFrozen(tools) ? 'frozen' : 'mutable';",
        Duration::from_secs(5),
    );
    assert!(outcome.ok, "{:?}", outcome.error);
    assert_eq!(outcome.value, serde_json::json!("frozen"));
}

#[test]
fn proto_wire_name_survives_serialization() {
    let fake = FakeCaller::ok();
    let caller: Arc<dyn ToolCaller> = fake.clone();
    let outcome = sandbox::execute(
        &caller,
        &[fake_server()],
        &[],
        "return tools.fake.echo({ first: 'a', second: 1, proto_key: 'p' });",
        Duration::from_secs(5),
    );
    assert!(outcome.ok, "{:?}", outcome.error);
    let calls = fake.calls.lock().expect("fake caller mutex poisoned");
    assert_eq!(
        calls[0].2,
        json!({ "first_wire": "a", "second_wire": 1, "__proto__": "p" })
    );
}

#[test]
fn tool_timeout_is_reported_as_tool_timeout() {
    let fake = FakeCaller::failing(ToolCallFailureKind::ToolTimeout);
    let caller: Arc<dyn ToolCaller> = fake.clone();
    let outcome = sandbox::execute(
        &caller,
        &[fake_server()],
        &[],
        "return tools.fake.echo({ first: 'hello' });",
        Duration::from_secs(5),
    );
    assert!(!outcome.ok);
    let error = outcome.error.unwrap();
    assert_eq!(error.code, "tool_timeout");
    assert!(error.message.contains("fake failure"));
    let traces = &outcome.tool_calls;
    assert_eq!(traces.len(), 1);
    assert!(!traces[0].ok);
}

#[test]
fn caught_tool_timeout_does_not_contaminate_later_errors() {
    let fake = FakeCaller::failing(ToolCallFailureKind::ToolTimeout);
    let caller: Arc<dyn ToolCaller> = fake.clone();
    let outcome = sandbox::execute(
        &caller,
        &[fake_server()],
        &[],
        "try { tools.fake.echo({ first: 'hello' }); } catch (e) { throw new Error('different failure'); }",
        Duration::from_secs(5),
    );
    assert!(!outcome.ok);
    let error = outcome.error.unwrap();
    assert_eq!(error.code, "js_error");
    assert!(error.message.contains("different failure"));
}

#[test]
fn failed_tool_call_is_reported_as_js_error() {
    let fake = FakeCaller::failing(ToolCallFailureKind::Failed);
    let caller: Arc<dyn ToolCaller> = fake.clone();
    let outcome = sandbox::execute(
        &caller,
        &[fake_server()],
        &[],
        "return tools.fake.echo({ first: 'hello' });",
        Duration::from_secs(5),
    );
    assert!(!outcome.ok);
    let error = outcome.error.unwrap();
    assert_eq!(error.code, "js_error");
    assert!(error.message.contains("fake failure"));
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

#[test]
fn rejects_invalid_javascript() {
    let outcome = sandbox::execute(&bridge(), &[], &[], "return (;", Duration::from_secs(5));
    assert!(!outcome.ok);
    assert_eq!(outcome.error.as_ref().unwrap().code, "js_error");
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
