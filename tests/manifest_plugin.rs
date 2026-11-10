use std::path::PathBuf;

use xidl_codemode::manifest;

static PATH_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn plugin_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_xidl-codemode-manifest"))
        .parent()
        .unwrap()
        .to_path_buf()
}

fn with_plugin_on_path<T>(f: impl FnOnce() -> T) -> T {
    let _guard = PATH_LOCK.lock().expect("PATH test lock poisoned");
    let original = std::env::var_os("PATH");
    let mut paths: Vec<PathBuf> = original
        .as_deref()
        .map(std::env::split_paths)
        .into_iter()
        .flatten()
        .collect();
    paths.insert(0, plugin_dir());
    std::env::set_var("PATH", std::env::join_paths(paths).unwrap());

    let result = f();

    match original {
        Some(value) => std::env::set_var("PATH", value),
        None => std::env::remove_var("PATH"),
    }
    result
}

const DEMO_IDL: &str = r#"
#pragma xidlc package demo
#pragma xidlc version 1.0.0

struct McpToolResult {
    boolean isError;
    sequence<any> content;
    any structuredContent;
};

@mcp_server("demo")
@mcp_namespace("default")
interface DemoDefaultApi {
    @mcp_tool("echo")
    @mcp_read_only
    McpToolResult echo(
        string message,
        @optional @rename("tag") string label
    );
};
"#;

#[test]
fn external_plugin_generates_manifest_from_idl() {
    let manifest = with_plugin_on_path(|| manifest::generate("demo", DEMO_IDL)).unwrap();
    assert_eq!(manifest.server, "demo");
    let namespace = manifest.namespaces.get("default").unwrap();
    assert_eq!(namespace.idl, "/demo/default.idl");
    assert_eq!(namespace.interface, "DemoDefaultApi");
    assert_eq!(namespace.tools.len(), 1);
    assert_eq!(namespace.tools[0].js, "echo");
    assert_eq!(namespace.tools[0].mcp, "echo");
    assert_eq!(namespace.tools[0].output, "McpToolResult");
    assert_eq!(namespace.tools[0].params.len(), 2);
    assert_eq!(namespace.tools[0].params[0].name, "message");
    assert_eq!(namespace.tools[0].params[0].wire, "message");
    assert!(namespace.tools[0].params[0].required);
    assert_eq!(namespace.tools[0].params[1].name, "label");
    assert_eq!(namespace.tools[0].params[1].wire, "tag");
    assert!(!namespace.tools[0].params[1].required);
    assert!(namespace.tools[0].read_only);
    assert!(!namespace.tools[0].destructive);
    assert!(!namespace.tools[0].idempotent);
    assert!(!namespace.tools[0].open_world);
}

#[test]
fn plugin_rejects_missing_server_annotation() {
    let idl = r#"
#pragma xidlc package demo
#pragma xidlc version 1.0.0
struct McpToolResult { boolean isError; sequence<any> content; any structuredContent; };
interface DemoDefaultApi {
    @mcp_tool("echo")
    McpToolResult echo(string message);
};
"#;
    let err = with_plugin_on_path(|| manifest::generate("demo", idl)).unwrap_err();
    assert!(err.to_string().contains("missing @mcp_server"), "{err}");
}

#[test]
fn validate_rejects_unknown_tool() {
    use std::collections::BTreeMap;

    use xidl_codemode::catalog::ServerCatalog;
    use xidl_codemode::manifest_types::{Manifest, ManifestNamespace, ManifestParam, ManifestTool};

    let manifest = Manifest {
        version: 1,
        server: "demo".into(),
        namespaces: BTreeMap::from([(
            "default".into(),
            ManifestNamespace {
                idl: "/demo/default.idl".into(),
                interface: "DemoDefaultApi".into(),
                tools: vec![ManifestTool {
                    js: "echo".into(),
                    mcp: "missing".into(),
                    output: "McpToolResult".into(),
                    params: vec![ManifestParam {
                        name: "message".into(),
                        wire: "message".into(),
                        required: true,
                    }],
                    read_only: true,
                    destructive: false,
                    idempotent: false,
                    open_world: false,
                }],
            },
        )]),
    };
    let catalog = ServerCatalog {
        name: "demo".into(),
        js_name: "demo".into(),
        instructions: None,
        tools: Vec::new(),
        namespace_map: Default::default(),
    };
    let err = manifest::validate(&manifest, &catalog).unwrap_err();
    assert!(err.to_string().contains("unknown MCP tool"), "{err}");
}
