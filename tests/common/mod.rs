#![allow(dead_code)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use xidl_codemode::bridge::UpstreamManager;
use xidl_codemode::sandbox::{RuntimeServer, ToolCallFailure, ToolCallFailureKind, ToolCaller};

pub fn bridge() -> Arc<dyn ToolCaller> {
    Arc::new(UpstreamManager::start().unwrap())
}

#[derive(Default)]
pub struct FakeCaller {
    pub calls: Mutex<Vec<(String, String, Value)>>,
    failure: Option<ToolCallFailureKind>,
}

impl FakeCaller {
    pub fn ok() -> Arc<FakeCaller> {
        Arc::new(FakeCaller::default())
    }

    pub fn failing(kind: ToolCallFailureKind) -> Arc<FakeCaller> {
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

pub fn fake_server() -> RuntimeServer {
    RuntimeServer {
        name: "fake".into(),
        js_name: "fake".into(),
        glue: FAKE_GLUE.into(),
    }
}
