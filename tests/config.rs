use std::collections::BTreeMap;

use xidl_codemode::catalog::namespace_for;
use xidl_codemode::config::{self, Transport};

#[test]
fn parses_json_config_and_infers_stdio() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mcp.json");
    std::fs::write(
        &path,
        r#"{
          "mcpServers": {
            "demo": { "command": "demo-server", "args": ["stdio"] },
            "remote": { "type": "http", "url": "https://example.com/mcp" },
            "disabled": { "command": "x", "disabled": true }
          }
        }"#,
    )
    .unwrap();
    let loaded = config::load(Some(&path)).unwrap();
    assert_eq!(loaded.servers.len(), 2);
    let demo = loaded
        .servers
        .iter()
        .find(|server| server.name == "demo")
        .unwrap();
    match &demo.transport {
        Transport::Stdio {
            command,
            args,
            env,
            cwd,
        } => {
            assert_eq!(command, "demo-server");
            assert_eq!(args, &["stdio".to_string()]);
            assert!(env.is_empty());
            assert!(cwd.is_none());
        }
        other => panic!("expected stdio transport, got {other:?}"),
    }
    let remote = loaded
        .servers
        .iter()
        .find(|server| server.name == "remote")
        .unwrap();
    match &remote.transport {
        Transport::Http { url, headers } => {
            assert_eq!(url, "https://example.com/mcp");
            assert!(headers.is_empty());
        }
        other => panic!("expected http transport, got {other:?}"),
    }
}

#[test]
fn parses_toml_config() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        r#"
[mcpServers.demo]
command = "demo-server"
args = ["stdio"]

[mcpServers.demo.env]
DEMO = "1"
"#,
    )
    .unwrap();
    let loaded = config::load(Some(&path)).unwrap();
    assert_eq!(loaded.servers.len(), 1);
    match &loaded.servers[0].transport {
        Transport::Stdio {
            command, args, env, ..
        } => {
            assert_eq!(command, "demo-server");
            assert_eq!(args, &["stdio".to_string()]);
            assert_eq!(env.get("DEMO").map(String::as_str), Some("1"));
        }
        other => panic!("expected stdio transport, got {other:?}"),
    }
}

#[test]
fn groups_tools_by_namespace() {
    let map = BTreeMap::new();
    let all = vec![
        "issues.get".to_string(),
        "issues.list".to_string(),
        "search_issues".to_string(),
        "search_prs".to_string(),
        "misc".to_string(),
    ];
    assert_eq!(namespace_for("github", "issues.get", &all, &map), "issues");
    assert_eq!(
        namespace_for("github", "search_issues", &all, &map),
        "search"
    );
    assert_eq!(namespace_for("github", "misc", &all, &map), "default");
}

#[test]
fn toml_extension_uses_toml_parser() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "[mcpServers.demo\ncommand =").unwrap();
    let err = config::load(Some(&path)).unwrap_err();
    assert!(err.to_string().contains("invalid TOML"), "{err}");
}

#[test]
fn rejects_invalid_json() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mcp.json");
    std::fs::write(&path, "{ not valid").unwrap();
    let err = config::load(Some(&path)).unwrap_err();
    assert!(err.to_string().contains("invalid JSON"), "{err}");
}

#[test]
fn rejects_config_with_no_enabled_servers() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mcp.json");
    std::fs::write(
        &path,
        r#"{"mcpServers":{"off":{"command":"x","enabled":false}}}"#,
    )
    .unwrap();
    let err = config::load(Some(&path)).unwrap_err();
    assert!(err.to_string().contains("no enabled MCP servers"), "{err}");
}

#[test]
fn rejects_unsupported_transport() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mcp.json");
    std::fs::write(
        &path,
        r#"{"mcpServers":{"bad":{"type":"grpc","url":"https://example.com"}}}"#,
    )
    .unwrap();
    let err = config::load(Some(&path)).unwrap_err();
    assert!(
        err.to_string().contains("unsupported transport type grpc"),
        "{err}"
    );
}

#[test]
fn ignores_self_entry() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mcp.json");
    std::fs::write(
        &path,
        r#"{"mcpServers":{"xidl-codemode":{"command":"x"},"demo":{"command":"demo-server"}}}"#,
    )
    .unwrap();
    let loaded = config::load(Some(&path)).unwrap();
    assert_eq!(loaded.servers.len(), 1);
    assert_eq!(loaded.servers[0].name, "demo");
}

#[test]
fn parses_execute_timeout_hard_limit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mcp.json");
    std::fs::write(
        &path,
        r#"{"mcpServers":{"demo":{"command":"demo-server"}},"executeTimeoutHardMs":30000}"#,
    )
    .unwrap();
    let loaded = config::load(Some(&path)).unwrap();
    assert_eq!(loaded.execute_timeout_hard_ms, 30000);

    let path = dir.path().join("snake.json");
    std::fs::write(
        &path,
        r#"{"mcpServers":{"demo":{"command":"demo-server"}},"execute_timeout_hard_ms":45000}"#,
    )
    .unwrap();
    let loaded = config::load(Some(&path)).unwrap();
    assert_eq!(loaded.execute_timeout_hard_ms, 45000);

    let path = dir.path().join("default.json");
    std::fs::write(
        &path,
        r#"{"mcpServers":{"demo":{"command":"demo-server"}}}"#,
    )
    .unwrap();
    let loaded = config::load(Some(&path)).unwrap();
    assert_eq!(loaded.execute_timeout_hard_ms, 60000);
}

#[test]
fn namespace_map_override_wins() {
    let mut map = BTreeMap::new();
    map.insert("misc".to_string(), "custom".to_string());
    let all = vec!["misc".to_string()];
    assert_eq!(namespace_for("github", "misc", &all, &map), "custom");
}
