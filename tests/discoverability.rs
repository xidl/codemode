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
        "return {servers: JSON.parse(catalog.servers()), issue: catalog.search('issues get', '', '').length - 119};",
        std::time::Duration::from_secs(5),
    );
    assert!(outcome.ok, "{:?}", outcome.error);
    assert_eq!(
        outcome.value,
        serde_json::json!({"servers": {"github": 2}, "issue": 1})
    );
}

#[test]
fn instructions_expose_server_catalog_and_execute_catalog() {
    let root = tempfile::tempdir().unwrap();
    let docs = workspace::prepare_in(root.path(), None).unwrap();
    let demo = catalog("demo");
    let generated = idl::generate(&demo);
    workspace::write_server(&docs.path, &generated).unwrap();
    let docs_list = vec![("demo".to_string(), generated.agents_md.clone())];
    let namespaces: Vec<String> = generated
        .namespaces
        .iter()
        .map(|namespace| namespace.name.clone())
        .collect();
    let server_catalog = vec![format!("- `{}`: {} tools", demo.js_name, demo.tools.len())];
    let execute_catalog = vec![format!("mcp.{}.tool_name(arg1, arg2);", demo.js_name)];
    let instructions = xidl_codemode::build_instructions(
        &docs.path,
        &docs_list,
        &server_catalog,
        &[namespaces],
        &execute_catalog,
        std::slice::from_ref(&demo),
        &[],
    );
    assert!(instructions.contains("Connected MCP servers (demo)."));
    assert!(instructions.contains("## Execute catalog"));
    assert!(instructions.contains("mcp.demo.tool_name(arg1, arg2);"));
    assert!(instructions.contains("namespaces: issues"));
}
