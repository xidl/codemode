use std::path::PathBuf;

use xidl_codemode::manifest;

fn plugin_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_xidl-codemode-manifest"))
        .parent()
        .unwrap()
        .to_path_buf()
}

#[test]
fn external_plugin_generates_manifest_from_idl() {
    let dir = plugin_dir();
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut paths: Vec<PathBuf> = std::env::split_paths(&path).collect();
    paths.insert(0, dir);
    std::env::set_var("PATH", std::env::join_paths(paths).unwrap());

    let idl = r#"
#pragma xidlc package demo
#pragma xidlc version 1.0.0

struct McpToolResult {
    boolean isError;
    sequence<any> content;
    any structuredContent;
};

struct DemoDefaultEchoInput {
    string message;
};

@mcp_server("demo")
@mcp_namespace("default")
interface DemoDefaultApi {
    @mcp_tool("echo")
    @mcp_read_only
    McpToolResult echo(in DemoDefaultEchoInput input);
};
"#;
    let manifest = manifest::generate("demo", idl).unwrap();
    assert_eq!(manifest.server, "demo");
    let namespace = manifest.namespaces.get("default").unwrap();
    assert_eq!(namespace.tools.len(), 1);
    assert_eq!(namespace.tools[0].js, "echo");
    assert_eq!(namespace.tools[0].mcp, "echo");
    assert_eq!(namespace.tools[0].input, "DemoDefaultEchoInput");
    assert!(namespace.tools[0].read_only);
}
