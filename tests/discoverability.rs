use xidl_codemode::catalog::ServerCatalog;
use xidl_codemode::idl;
use xidl_codemode::sandbox;
use xidl_codemode::workspace;

fn catalog(name: &str) -> ServerCatalog {
    ServerCatalog {
        name: name.into(),
        js_name: name.into(),
        instructions: None,
        tools: vec![
            xidl_codemode::catalog::ToolInfo {
                name: "issues.get".into(),
                description: "Get issue".into(),
                input_schema: serde_json::json!({"type": "object"}),
                output_schema: None,
                read_only: true,
                destructive: false,
                idempotent: true,
                open_world: false,
            },
            xidl_codemode::catalog::ToolInfo {
                name: "issues.list".into(),
                description: "List issues".into(),
                input_schema: serde_json::json!({"type": "object"}),
                output_schema: None,
                read_only: true,
                destructive: false,
                idempotent: true,
                open_world: false,
            },
        ],
        namespace_map: Default::default(),
    }
}

#[test]
fn catalog_search_matches_names_and_scopes() {
    let catalogs = vec![catalog("github")];
    let tools = sandbox::search_catalog(&catalogs, Some("issues get"), None, None);
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].tool, "issues_get");
    assert_eq!(tools[0].server, "github");
    assert_eq!(tools[0].wire_server, "github");
    assert_eq!(tools[0].namespace, "issues");

    let namespace_only = sandbox::search_catalog(&catalogs, None, Some("github"), Some("issues"));
    assert_eq!(namespace_only.len(), 2);

    let no_scope = sandbox::search_catalog(&catalogs, None, None, None);
    assert_eq!(no_scope.len(), 2);
}

#[test]
fn catalog_helpers_are_frozen_and_share_index() {
    let catalogs = vec![catalog("github")];
    let caller: std::sync::Arc<dyn xidl_codemode::sandbox::ToolCaller> =
        std::sync::Arc::new(xidl_codemode::bridge::UpstreamManager::start().unwrap());
    let outcome = xidl_codemode::sandbox::execute(
        &caller,
        &[],
        &catalogs,
        "return {servers: JSON.parse(catalog.servers()), issue: JSON.parse(catalog.search('issues get', '', '')).length};",
        std::time::Duration::from_secs(5),
    );
    assert!(outcome.ok, "{:?}", outcome.error);
    assert_eq!(
        outcome.value,
        serde_json::json!({"servers": {"github": 2}, "issue": 1})
    );
}

#[test]
fn instructions_are_compact_and_reference_upstream_notes() {
    let root = tempfile::tempdir().unwrap();
    let docs = workspace::prepare_in(root.path(), None).unwrap();
    let mut demo = catalog("demo");
    demo.instructions = Some("UNTRUSTED_UPSTREAM_TEXT".to_string());
    let generated = idl::generate(&demo);
    workspace::write_server(&docs.path, &generated).unwrap();
    let connected_servers = vec![demo.js_name.clone()];
    let instructions = xidl_codemode::build_instructions(&docs.path, &connected_servers, &[]);
    assert!(instructions.contains("Connected servers (demo)."));
    assert!(instructions.contains("tools.<server>.<tool>({ param: value, ... })"));
    assert!(instructions.contains("sandbox-local JavaScript bindings"));
    assert!(instructions.contains("<server>/agents.md"));
    assert!(!instructions.contains("UNTRUSTED_UPSTREAM_TEXT"));
    assert!(!instructions.contains("Available MCP servers"));
    assert!(!instructions.contains("## Execute catalog"));
    assert!(!instructions.contains("QuickJS"));
}
