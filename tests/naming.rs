use xidl_codemode::catalog::{snake_case, validate_server_catalog, ServerCatalog, ToolInfo};

fn catalog_with(tools: &[&str]) -> ServerCatalog {
    ServerCatalog {
        name: "demo".into(),
        js_name: "demo".into(),
        instructions: None,
        namespace_map: Default::default(),
        tools: tools
            .iter()
            .map(|name| ToolInfo {
                name: (*name).to_string(),
                description: String::new(),
                input_schema: serde_json::json!({"type": "object"}),
                output_schema: None,
                read_only: false,
                destructive: false,
                idempotent: false,
                open_world: false,
            })
            .collect(),
    }
}

#[test]
fn snake_case_normalizes_camel_pascal_and_separators() {
    assert_eq!(snake_case("getIssue"), "get_issue");
    assert_eq!(snake_case("HTTPFetch"), "http_fetch");
    assert_eq!(snake_case("HTTPSFooBar"), "https_foo_bar");
    assert_eq!(snake_case("issues.get"), "issues_get");
    assert_eq!(snake_case("agent-browser"), "agent_browser");
    assert_eq!(snake_case("X-Secure-Key"), "x_secure_key");
    assert_eq!(snake_case("issue_number"), "issue_number");
    assert_eq!(snake_case("2fa"), "_2fa");
    assert_eq!(snake_case("__proto__"), "__proto__");
}

#[test]
fn validate_accepts_unique_names() {
    validate_server_catalog(&catalog_with(&["get_issue", "issues.list", "misc"])).unwrap();
}

#[test]
fn validate_rejects_tool_name_collisions() {
    let err = validate_server_catalog(&catalog_with(&["getIssue", "get_issue"])).unwrap_err();
    assert!(err.contains("both map to \"get_issue\""), "{err}");
    assert!(err.contains("getIssue"), "{err}");
}

#[test]
fn validate_rejects_namespace_prefix_collisions() {
    let err = validate_server_catalog(&catalog_with(&["MyTools.a", "my_tools.b"])).unwrap_err();
    assert!(err.contains("conflicting origins"), "{err}");
    assert!(err.contains("MyTools"), "{err}");
    assert!(err.contains("my_tools"), "{err}");
}
