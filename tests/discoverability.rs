use xidl_codemode::catalog::ServerCatalog;
use xidl_codemode::idl;
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
        &[],
    );
    assert!(instructions.contains("Connected MCP servers (demo)."));
    assert!(instructions.contains("## Execute catalog"));
    assert!(instructions.contains("mcp.demo.tool_name(arg1, arg2);"));
    assert!(instructions.contains("namespaces: issues"));
}
