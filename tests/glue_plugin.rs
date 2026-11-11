use std::path::PathBuf;

use xidl_codemode::catalog::ServerCatalog;
use xidl_codemode::glue;

static PATH_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn plugin_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_xidl-codemode-glue"))
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

    struct PathGuard {
        original: Option<std::ffi::OsString>,
    }

    impl Drop for PathGuard {
        fn drop(&mut self) {
            match &self.original {
                Some(value) => std::env::set_var("PATH", value),
                None => std::env::remove_var("PATH"),
            }
        }
    }

    let _guard = PathGuard { original };
    f()
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

fn demo_catalog(tools: &[&str]) -> ServerCatalog {
    ServerCatalog {
        name: "demo".into(),
        js_name: "demo".into(),
        instructions: None,
        tools: tools
            .iter()
            .map(|name| xidl_codemode::catalog::ToolInfo {
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
        namespace_map: Default::default(),
    }
}

#[test]
fn external_plugin_generates_installable_glue() {
    let glue = with_plugin_on_path(|| glue::generate("demo", DEMO_IDL)).unwrap();
    assert!(glue.js.contains("function installXidlGlue(target, host)"));
    assert!(glue.js.contains("__ns[\"echo\"] = function(...args)"));
    assert!(glue.js.contains("tool: \"echo\""));
    assert!(glue.js.contains("__params[\"tag\"] = args[1];"));
    assert!(glue
        .js
        .contains("missing required argument message for demo.echo"));
    assert!(glue
        .js
        .contains("too many arguments for demo.echo (expected 2)"));
    assert!(glue.js.contains("host.callTool(JSON.stringify("));
    assert!(glue.js.contains("Object.freeze(__ns);"));
    assert!(glue.dts.contains("export declare function installXidlGlue"));
    assert!(glue
        .dts
        .contains("echo(message: string, label?: string): McpToolResult;"));
}

#[test]
fn audit_accepts_matching_catalog() {
    let glue = with_plugin_on_path(|| glue::generate("demo", DEMO_IDL)).unwrap();
    glue::audit(&glue.js, &demo_catalog(&["echo"])).unwrap();
}

#[test]
fn audit_rejects_missing_tool() {
    let glue = with_plugin_on_path(|| glue::generate("demo", DEMO_IDL)).unwrap();
    let err = glue::audit(&glue.js, &demo_catalog(&[])).unwrap_err();
    assert!(
        err.to_string().contains("glue tool keys do not match"),
        "{err}"
    );
}

#[test]
fn audit_rejects_extra_tool() {
    let glue = with_plugin_on_path(|| glue::generate("demo", DEMO_IDL)).unwrap();
    let err = glue::audit(&glue.js, &demo_catalog(&["echo", "other"])).unwrap_err();
    assert!(
        err.to_string().contains("glue tool keys do not match"),
        "{err}"
    );
}

#[test]
fn generated_glue_uses_null_proto_params() {
    let idl = r#"
#pragma xidlc package demo
#pragma xidlc version 1.0.0
struct McpToolResult { boolean isError; sequence<any> content; any structuredContent; };
@mcp_server("demo")
@mcp_namespace("default")
interface DemoDefaultApi {
    @mcp_tool("proto_tool")
    McpToolResult proto_tool(string __proto__, string normal);
};
"#;
    let glue = with_plugin_on_path(|| glue::generate("demo", idl)).unwrap();
    assert!(
        glue.js.contains("const __params = Object.create(null);"),
        "{glue:?}"
    );
    assert!(glue.js.contains("__params[\"__proto__\"]"), "{glue:?}");
    glue::audit(&glue.js, &demo_catalog(&["proto_tool"])).unwrap();
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
    let err = with_plugin_on_path(|| glue::generate("demo", idl)).unwrap_err();
    assert!(err.to_string().contains("missing @mcp_server"), "{err}");
}

#[test]
fn plugin_rejects_duplicate_js_names_across_namespaces() {
    let idl = r#"
#pragma xidlc package demo
#pragma xidlc version 1.0.0
struct McpToolResult { boolean isError; sequence<any> content; any structuredContent; };
@mcp_server("demo")
@mcp_namespace("default")
interface DemoDefaultApi {
    @mcp_tool("first.echo")
    McpToolResult echo(string message);
};
@mcp_server("demo")
@mcp_namespace("other")
interface DemoOtherApi {
    @mcp_tool("second.echo")
    McpToolResult echo(string message);
};
"#;
    let err = with_plugin_on_path(|| glue::generate("demo", idl)).unwrap_err();
    assert!(err.to_string().contains("duplicate JS tool name"), "{err}");
}
