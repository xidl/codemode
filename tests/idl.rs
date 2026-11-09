use serde_json::json;
use xidl_codemode::catalog::{ServerCatalog, ToolInfo};
use xidl_codemode::idl;

#[test]
fn generates_namespace_idl_and_agents_md() {
    let catalog = ServerCatalog {
        name: "demo".into(),
        js_name: "demo".into(),
        instructions: None,
        namespace_map: Default::default(),
        tools: vec![
            ToolInfo {
                name: "issues.get".into(),
                description: "Get an issue".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "id": { "type": "integer" },
                        "title": { "type": "string" }
                    },
                    "required": ["id"]
                }),
                output_schema: None,
                read_only: true,
                destructive: false,
                idempotent: true,
                open_world: false,
            },
            ToolInfo {
                name: "issues.list".into(),
                description: "List issues".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "labels": { "type": "array", "items": { "type": "string" } }
                    }
                }),
                output_schema: None,
                read_only: true,
                destructive: false,
                idempotent: true,
                open_world: false,
            },
        ],
    };
    let generated = idl::generate(&catalog);
    assert_eq!(generated.namespaces.len(), 1);
    assert_eq!(generated.namespaces[0].name, "issues");
    let idl = &generated.namespaces[0].idl;
    assert!(idl.contains("@mcp_server(\"demo\")"));
    assert!(idl.contains("@mcp_namespace(\"issues\")"));
    assert!(idl.contains("@mcp_tool(\"issues.get\")"));
    assert!(idl.contains("McpToolResult issues_get("));
    assert!(idl.contains("int64 id,"));
    assert!(idl.contains("@optional\n        string title\n"));
    assert!(idl.contains("@optional\n        sequence<string> labels\n"));
    assert!(!idl.contains("in DemoIssuesIssuesGetInput input"));
    assert!(idl.contains("@mcp_read_only"));
    assert!(idl.contains("@mcp_idempotent"));
    assert!(generated
        .agents_md
        .contains("mcp.demo.issues_get(id: number"));
    assert!(generated.bundle.contains("interface DemoIssuesApi"));
}
