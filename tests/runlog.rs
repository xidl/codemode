use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use xidl_codemode::runlog::RunLogger;
use xidl_codemode::sandbox::{self, RunSource, ToolCallFailure, ToolCaller};

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
    return JSON.parse(host.callTool(JSON.stringify({server: "fake", tool: "echo", params: {}})));
  };
  const server = Object.create(null);
  server.fake = ns;
  Object.defineProperty(target, "fake", { value: server.fake, enumerable: true });
}
"#;

fn read_lines(path: &str) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn create_writes_run_files_with_permissions() {
    let root = tempfile::tempdir().unwrap();
    let logger = RunLogger::create(root.path(), &json!({"source": "code"}));
    let paths = logger.paths().expect("enabled logger");
    let run_json: Value = serde_json::from_str(
        &std::fs::read_to_string(std::path::Path::new(&paths.dir).join("run.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(run_json["source"], "code");
    assert!(run_json["run_id"].as_str().is_some());
    assert!(run_json["run_id"] == json!(paths.id));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let dir_mode = std::fs::metadata(&paths.dir).unwrap().permissions().mode();
        let log_mode = std::fs::metadata(&paths.log).unwrap().permissions().mode();
        assert_eq!(dir_mode & 0o777, 0o700);
        assert_eq!(log_mode & 0o777, 0o600);
    }
}

#[test]
fn create_fails_closed_to_disabled_logger() {
    let root = tempfile::tempdir().unwrap();
    let blocker = root.path().join("blocker");
    std::fs::write(&blocker, b"not a dir").unwrap();
    let logger = RunLogger::create(&blocker, &json!({}));
    assert!(logger.paths().is_none());
}

#[test]
fn events_and_metrics_are_appended_with_timestamps() {
    let root = tempfile::tempdir().unwrap();
    let logger = RunLogger::create(root.path(), &json!({}));
    let paths = logger.paths().unwrap().clone();
    logger.log_event(json!({"event": "execute.start"}));
    logger.log_metric(json!({"metric": "tool_call", "ok": true}));
    let events = read_lines(&paths.log);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["event"], "execute.start");
    assert!(events[0]["ts_ms"].is_u64());
    let metrics = read_lines(&paths.metrics);
    assert_eq!(metrics[0]["metric"], "tool_call");
    logger.write_summary(&json!({"ok": true}));
    let summary: Value = serde_json::from_str(
        &std::fs::read_to_string(std::path::Path::new(&paths.dir).join("summary.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(summary["ok"], true);
    assert!(!root.path().join("summary.tmp").exists());
}

#[test]
fn previews_are_bounded_and_redacted() {
    let deep = json!({
        "authorization": "Bearer abc",
        "nested": {"api_key": "k", "token": "t", "safe": "x"}
    });
    let value = xidl_codemode::runlog::preview(&deep);
    let text = serde_json::to_string(&value).unwrap();
    assert!(text.contains("[REDACTED]"));
    assert!(!text.contains("Bearer abc"));
    assert!(!text.contains("\"k\""));
    let big = json!({"blob": "y".repeat(8192)});
    let value = xidl_codemode::runlog::preview(&big);
    let text = serde_json::to_string(&value).unwrap();
    assert!(text.len() < 4096);
    assert!(text.contains("truncated"));
}

#[test]
fn retention_keeps_recent_runs_and_never_deletes_current() {
    let root = tempfile::tempdir().unwrap();
    for index in 0..24 {
        let dir = root
            .path()
            .join(format!("2026010{}T000000Z-{index:06x}", index % 9 + 1));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("log.jsonl"), "x".repeat(16).as_bytes()).unwrap();
    }
    let logger = RunLogger::create(root.path(), &json!({}));
    let paths = logger.paths().unwrap().clone();
    let remaining: Vec<_> = std::fs::read_dir(root.path())
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .collect();
    assert!(remaining.len() <= 21, "{remaining:?}");
    assert!(remaining.contains(&paths.id));
}

#[test]
fn execute_writes_tool_call_events_and_metrics() {
    let root = tempfile::tempdir().unwrap();
    let caller: Arc<dyn ToolCaller> = Arc::new(FixedCaller(json!({
        "content": [{"type": "text", "text": "{\"answer\": 42}"}]
    })));
    let logger = Arc::new(RunLogger::create(root.path(), &json!({"source": "code"})));
    let server = sandbox::RuntimeServer {
        name: "fake".into(),
        js_name: "fake".into(),
        glue: GLUE.into(),
    };
    let outcome = sandbox::execute_with_run_logger(
        &caller,
        &[server],
        &[],
        "console.log('hi'); return tools.fake.echo({ first: 'secret-token-value' });",
        Duration::from_secs(5),
        RunSource {
            kind: "code",
            path: None,
        },
        &logger,
    );
    assert!(outcome.ok, "{:?}", outcome.error);
    let paths = logger.paths().unwrap().clone();
    assert_eq!(outcome.run.as_ref().unwrap().id, paths.id);
    let events = read_lines(&paths.log);
    let names: Vec<&str> = events
        .iter()
        .filter_map(|event| event["event"].as_str())
        .collect();
    assert!(names.contains(&"execute.start"));
    assert!(names.contains(&"log"));
    assert!(names.contains(&"tool_call.start"));
    assert!(names.contains(&"tool_call.end"));
    assert!(names.contains(&"execute.end"));
    let start = events
        .iter()
        .find(|event| event["event"] == "tool_call.start")
        .unwrap();
    assert_eq!(start["server"], "fake");
    assert_eq!(start["tool"], "echo");
    let end = events
        .iter()
        .find(|event| event["event"] == "tool_call.end")
        .unwrap();
    assert_eq!(end["status"], "ok");
    assert_eq!(end["result"], json!({"answer": 42}));
    let metrics = read_lines(&paths.metrics);
    assert!(metrics
        .iter()
        .any(|metric| metric["metric"] == "tool_call" && metric["result_kind"] == "object"));
    assert!(metrics.iter().any(|metric| metric["metric"] == "execute"));
    let summary: Value = serde_json::from_str(
        &std::fs::read_to_string(std::path::Path::new(&paths.dir).join("summary.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(summary["ok"], true);
    assert_eq!(summary["tool_call_count"], 1);
}

#[test]
fn timeout_flushes_pending_call_and_timeout_event() {
    let root = tempfile::tempdir().unwrap();
    let caller: Arc<dyn ToolCaller> = Arc::new(FixedCaller(json!({
        "content": [{"type": "text", "text": "ok"}]
    })));
    let logger = Arc::new(RunLogger::create(root.path(), &json!({})));
    let outcome = sandbox::execute_with_run_logger(
        &caller,
        &[],
        &[],
        "while (true) {}",
        Duration::from_millis(200),
        RunSource {
            kind: "code",
            path: None,
        },
        &logger,
    );
    assert!(!outcome.ok);
    assert_eq!(outcome.error.as_ref().unwrap().code, "execute_timeout");
    let paths = logger.paths().unwrap().clone();
    let events = read_lines(&paths.log);
    let timeout_event = events
        .iter()
        .find(|event| event["event"] == "timeout")
        .expect("timeout event");
    assert_eq!(timeout_event["phase"], "javascript");
    assert!(events
        .iter()
        .any(|event| event["event"] == "execute.end" && event["ok"] == false));
}
