use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

mod common;

use common::{bridge, fake_server, FakeCaller};
use xidl_codemode::sandbox::{self, RuntimeServer, ToolCallFailureKind, ToolCaller};

fn run_with_fake(source: &str) -> (Arc<FakeCaller>, sandbox::ExecuteOutcome) {
    let fake = FakeCaller::ok();
    let caller: Arc<dyn ToolCaller> = fake.clone();
    let outcome = sandbox::execute(
        &caller,
        &[fake_server()],
        &[],
        source,
        Duration::from_secs(5),
    );
    (fake, outcome)
}

#[test]
fn object_arguments_map_to_wire_object() {
    let (fake, outcome) = run_with_fake("return tools.fake.echo({ first: 'hello', second: 7 });");
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
    let (fake, outcome) = run_with_fake("return tools.fake.echo({ first: 'hello' });");
    assert!(outcome.ok, "{:?}", outcome.error);
    let calls = fake.calls.lock().expect("fake caller mutex poisoned");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].2, json!({ "first_wire": "hello" }));
}

#[test]
fn explicit_null_fields_pass_through() {
    let (fake, outcome) =
        run_with_fake("return tools.fake.echo({ first: 'hello', second: null });");
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
    let (fake, outcome) = run_with_fake("return tools.fake.echo({ first: null });");
    assert!(outcome.ok, "{:?}", outcome.error);
    let calls = fake.calls.lock().expect("fake caller mutex poisoned");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].2, json!({ "first_wire": Value::Null }));
}

#[test]
fn required_undefined_field_fails() {
    let (fake, outcome) = run_with_fake("return tools.fake.echo({ first: undefined });");
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
    let (fake, outcome) = run_with_fake("return tools.fake.echo({});");
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
    let (fake, outcome) = run_with_fake("return tools.fake.echo({ first: 'a', extra: 1 });");
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
    let (fake, outcome) = {
        let fake = FakeCaller::ok();
        let caller: Arc<dyn ToolCaller> = fake.clone();
        let outcome = sandbox::execute(
            &caller,
            &[server],
            &[],
            "return [typeof tools.agent_browser, typeof tools['agent-browser'], typeof tools.agent_browser.open()].join(',');",
            Duration::from_secs(5),
        );
        (fake, outcome)
    };
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
    let (fake, outcome) =
        run_with_fake("return tools.fake.echo({ first: 'a', second: 1, proto_key: 'p' });");
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
