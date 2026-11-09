use std::collections::BTreeMap;

use xidl_codemode::catalog::namespace_for;
use xidl_codemode::config;

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
    let servers = config::load(Some(&path)).unwrap();
    assert_eq!(servers.len(), 2);
    assert!(servers.iter().any(|server| server.name == "demo"));
    assert!(servers.iter().any(|server| server.name == "remote"));
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
    let servers = config::load(Some(&path)).unwrap();
    assert_eq!(servers.len(), 1);
    assert_eq!(servers[0].name, "demo");
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
