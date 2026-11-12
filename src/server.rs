use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer, ServerHandler};
use serde::Deserialize;
use serde_json::Value;

use crate::catalog::ServerCatalog;
use crate::sandbox::{self, RuntimeServer, ToolCaller};

pub struct AppState {
    pub bridge: Arc<dyn ToolCaller>,
    pub servers: Vec<RuntimeServer>,
    pub docs_dir: std::path::PathBuf,
    pub connected_servers: Vec<String>,
    pub instructions: String,
    pub errors: Vec<String>,
    pub execute_timeout_hard_ms: u64,
    pub execute_file_roots: Vec<PathBuf>,
    pub cwd: PathBuf,
    pub tool_index: Vec<ServerCatalog>,
}

#[derive(Clone)]
pub struct CodemodeServer {
    state: Arc<AppState>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecuteArgs {
    /// Source mode: inline `code` (default) or a `path` on disk.
    #[serde(default)]
    pub r#type: Option<String>,
    /// JavaScript source. Required when `type` is `code` (the default).
    pub code: Option<String>,
    /// Path to a JavaScript file. Required when `type` is `file`.
    pub path: Option<String>,
    /// Execution timeout in milliseconds (default 10000, clamped to the
    /// configured hard limit).
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchToolsArgs {
    pub query: Option<String>,
    pub server: Option<String>,
    pub namespace: Option<String>,
    #[serde(default = "default_search_limit")]
    pub limit: usize,
}

fn default_search_limit() -> usize {
    8
}

const DEFAULT_EXECUTE_TIMEOUT_MS: u64 = 10_000;
const MAX_CODE_BYTES: usize = sandbox::MAX_CODE_BYTES;

#[derive(Debug)]
pub struct FileReadError {
    pub code: &'static str,
    pub message: String,
}

impl std::fmt::Display for FileReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// Validate the code/file split. Schema-shape problems return `Err` and map
/// to MCP `invalid_params`; IO problems come back from `read_execute_source`
/// and surface as `ExecuteOutcome` errors instead.
pub fn validate_execute_args(args: &ExecuteArgs) -> Result<&'static str, String> {
    match args.r#type.as_deref() {
        None | Some("code") => {
            if args.path.is_some() {
                return Err("`path` is only valid when type is \"file\"".into());
            }
            match &args.code {
                Some(code) if code.is_empty() => Err("`code` must not be empty".into()),
                Some(_) => Ok("code"),
                None => Err("`code` is required when type is \"code\"".into()),
            }
        }
        Some("file") => {
            if args.code.is_some() {
                return Err("`code` is only valid for type \"code\"; use `path` instead".into());
            }
            match &args.path {
                Some(path) if path.is_empty() => Err("`path` must not be empty".into()),
                Some(_) => Ok("file"),
                None => Err("`path` is required when type is \"file\"".into()),
            }
        }
        Some(other) => Err(format!(
            "invalid type {:?}: expected \"code\" or \"file\"",
            other
        )),
    }
}

fn path_within(path: &Path, root: &Path) -> bool {
    let normalize = |value: &Path| -> String {
        let mut text = value
            .to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "/");
        if cfg!(windows) {
            text = text.to_ascii_lowercase();
        }
        text
    };
    let file = normalize(path);
    let root = normalize(root);
    file == root || file.starts_with(&format!("{root}/"))
}

/// Read the file-mode source. Relative paths resolve against `cwd`; the file
/// must live inside one of `roots` after canonicalization (symlink escapes
/// are rejected), be a regular file, and decode as UTF-8. A leading BOM and
/// shebang line are stripped while preserving line numbers.
pub fn read_execute_source(
    raw: &str,
    roots: &[PathBuf],
    cwd: &Path,
    max_bytes: usize,
) -> std::result::Result<String, FileReadError> {
    if roots.is_empty() {
        return Err(FileReadError {
            code: "file_forbidden",
            message: "file mode is disabled: executeFileRoots is empty".into(),
        });
    }
    let path = Path::new(raw);
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    let canonical = absolute.canonicalize().map_err(|err| {
        let code = if err.kind() == std::io::ErrorKind::NotFound {
            "file_not_found"
        } else if err.kind() == std::io::ErrorKind::PermissionDenied {
            "file_forbidden"
        } else {
            "file_read_error"
        };
        FileReadError {
            code,
            message: format!("cannot resolve {}: {err}", absolute.display()),
        }
    })?;
    if !roots.iter().any(|root| {
        root.canonicalize()
            .map(|canonical_root| path_within(&canonical, &canonical_root))
            .unwrap_or(false)
    }) {
        return Err(FileReadError {
            code: "file_forbidden",
            message: format!(
                "{} is outside the allowed execute roots",
                canonical.display()
            ),
        });
    }
    let metadata = std::fs::metadata(&canonical).map_err(|err| FileReadError {
        code: "file_read_error",
        message: format!("cannot stat {}: {err}", canonical.display()),
    })?;
    if !metadata.is_file() {
        return Err(FileReadError {
            code: "file_not_regular",
            message: format!("{} is not a regular file", canonical.display()),
        });
    }
    if metadata.len() > max_bytes as u64 {
        return Err(FileReadError {
            code: "file_too_large",
            message: format!(
                "{} is {} bytes; limit is {max_bytes}",
                canonical.display(),
                metadata.len()
            ),
        });
    }
    let file = std::fs::File::open(&canonical).map_err(|err| FileReadError {
        code: "file_read_error",
        message: format!("cannot open {}: {err}", canonical.display()),
    })?;
    let mut bytes = Vec::new();
    std::io::Read::take(file, max_bytes as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|err| FileReadError {
            code: "file_read_error",
            message: format!("cannot read {}: {err}", canonical.display()),
        })?;
    if bytes.len() > max_bytes {
        return Err(FileReadError {
            code: "file_too_large",
            message: format!(
                "{} exceeded the {max_bytes} byte limit while reading",
                canonical.display()
            ),
        });
    }
    let mut text = String::from_utf8(bytes).map_err(|_| FileReadError {
        code: "file_invalid_utf8",
        message: format!("{} is not valid UTF-8", canonical.display()),
    })?;
    if let Some(rest) = text.strip_prefix('\u{feff}') {
        text = rest.to_string();
    }
    if text.starts_with("#!") {
        if let Some(line_end) = text.find('\n') {
            text = text[line_end..].to_string();
        } else {
            text = String::new();
        }
    }
    Ok(text)
}

impl CodemodeServer {
    pub fn new(state: Arc<AppState>) -> Self {
        Self { state }
    }
}

impl ServerHandler for CodemodeServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(
                "xidl-codemode",
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(self.state.instructions.clone())
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items(vec![
            execute_tool_with_limit(&self.state),
            search_tools_tool(),
        ]))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        if request.name == "search_tools" {
            let arguments = request.arguments.unwrap_or_default();
            let args: SearchToolsArgs =
                serde_json::from_value(Value::Object(arguments)).map_err(|err| {
                    McpError::invalid_params(format!("invalid search_tools arguments: {err}"), None)
                })?;
            let mut tools = sandbox::search_catalog(
                &self.state.tool_index,
                args.query.as_deref(),
                args.server.as_deref(),
                args.namespace.as_deref(),
            );
            let total = tools.len();
            tools.truncate(args.limit.clamp(1, 20));
            let text = serde_json::to_string(&serde_json::json!({
                "total": total,
                "returned": tools.len(),
                "hasMore": total > tools.len(),
                "tools": tools,
            }))
            .map_err(|err| McpError::internal_error(format!("serialize search: {err}"), None))?;
            return Ok(CallToolResult::success(vec![ContentBlock::text(text)]).into());
        }
        if request.name != "execute" {
            return Err(McpError::invalid_params(
                format!(
                    "unknown tool {:?}; only `execute` is available",
                    request.name
                ),
                None,
            ));
        }
        let arguments = request.arguments.unwrap_or_default();
        if arguments.is_empty() {
            return Ok(CallToolResult::success(vec![ContentBlock::text(
                serde_json::to_string(&serde_json::json!({
                    "connectedServers": self.state.connected_servers,
                    "docsRoot": self.state.docs_dir,
                    "callShape": "tools.<server>.<tool>({ param: value, ... })",
                    "examples": [
                        "return tools.<server>.<tool>({ param: value });"
                    ],
                    "hint": "Read the API docs directory for signatures; server, tool, and parameter names are snake_case and returns are plain JSON."
                }))
                .map_err(|err| {
                    McpError::internal_error(format!("serialize catalog: {err}"), None)
                })?,
            )])
            .into());
        }
        let args: ExecuteArgs =
            serde_json::from_value(Value::Object(arguments)).map_err(|err| {
                McpError::invalid_params(format!("invalid execute arguments: {err}"), None)
            })?;
        let mode = validate_execute_args(&args).map_err(|message| {
            McpError::invalid_params(format!("invalid execute arguments: {message}"), None)
        })?;
        let hard = self.state.execute_timeout_hard_ms.max(100);
        let timeout_ms = args
            .timeout_ms
            .unwrap_or(DEFAULT_EXECUTE_TIMEOUT_MS)
            .min(hard)
            .max(100);
        let timeout = Duration::from_millis(timeout_ms);
        let bridge = self.state.bridge.clone();
        let servers = self.state.servers.clone();
        let catalogs = self.state.tool_index.clone();
        let outcome = match mode {
            "code" => {
                let code = args.code.unwrap_or_default();
                tokio::task::spawn_blocking(move || {
                    sandbox::execute(&bridge, &servers, &catalogs, &code, timeout)
                })
                .await
            }
            _ => {
                let path = args.path.unwrap_or_default();
                let roots = self.state.execute_file_roots.clone();
                let cwd = self.state.cwd.clone();
                tokio::task::spawn_blocking(move || {
                    let started = Instant::now();
                    match read_execute_source(&path, &roots, &cwd, MAX_CODE_BYTES) {
                        Ok(code) => {
                            let remaining = timeout.saturating_sub(started.elapsed());
                            sandbox::execute(
                                &bridge,
                                &servers,
                                &catalogs,
                                &code,
                                remaining.max(Duration::from_millis(100)),
                            )
                        }
                        Err(err) => sandbox::ExecuteOutcome {
                            ok: false,
                            value: Value::Null,
                            logs: Vec::new(),
                            tool_calls: Vec::new(),
                            duration_ms: started.elapsed().as_millis() as u64,
                            timeout_ms: timeout.as_millis() as u64,
                            truncated: false,
                            error: Some(sandbox::ExecuteError {
                                code: err.code.to_string(),
                                message: err.message,
                                stack: None,
                            }),
                        },
                    }
                })
                .await
            }
        };
        let outcome = outcome
            .map_err(|err| McpError::internal_error(format!("sandbox task failed: {err}"), None))?;
        Ok(execute_call_result(&outcome).into())
    }
}

/// Wrap the sandbox outcome as an MCP result: the full outcome rides in
/// `structuredContent` (no double-encoded JSON), while the text content is a
/// short summary for clients that only read text blocks.
pub fn execute_call_result(outcome: &sandbox::ExecuteOutcome) -> CallToolResult {
    let summary = match &outcome.error {
        Some(error) => {
            let message: String = error.message.chars().take(500).collect();
            format!(
                "execute failed: {} ({}, {} ms); full result is in structuredContent",
                message, error.code, outcome.duration_ms
            )
        }
        None => format!(
            "execute ok ({} ms, {} tool calls); full result is in structuredContent",
            outcome.duration_ms,
            outcome.tool_calls.len()
        ),
    };
    let mut result = CallToolResult::success(vec![ContentBlock::text(summary)]);
    result.structured_content = Some(serde_json::to_value(outcome).unwrap_or(Value::Null));
    result
}

fn execute_tool_with_limit(state: &AppState) -> Tool {
    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "type": {
                "type": "string",
                "enum": ["code", "file"],
                "default": "code",
                "description": "\"code\" (default) runs inline JavaScript source; \"file\" reads the script from disk. Use \"code\" for short, one-off, generated JavaScript, and \"file\" when the user points to an existing script or you are iterating on a long file. File mode only loads the source; the sandbox still has no filesystem access."
            },
            "code": {
                "type": "string",
                "description": "JavaScript source; required when type=code. Use the injected global `tools` object with a single object argument, e.g. `return tools.server.tool({ param: value });`"
            },
            "path": {
                "type": "string",
                "description": "Path to a JavaScript file; required when type=file. Relative paths resolve against the workspace root; paths must stay inside the allowed execute roots. Do not use file mode to inspect a file: it executes the file as JavaScript."
            },
            "timeout_ms": {
                "type": "integer",
                "minimum": 100,
                "maximum": state.execute_timeout_hard_ms,
                "default": 10000
            }
        },
        "additionalProperties": false,
        "oneOf": [
            {"properties": {"type": {"const": "code"}}, "required": ["code"]},
            {"properties": {"type": {"const": "file"}}, "required": ["path"]}
        ]
    });
    Tool::new(
        "execute",
        format!(
            "Execute sandboxed JavaScript. Connected tools are injected as \
             `tools.<server>.<tool>({{ ... }})` with a single object argument. \
             Server, tool, and parameter names are snake_case; omit optional \
             fields instead of passing null placeholders, unknown fields are \
             rejected, and explicit null values pass through. Returns are plain \
             JSON values and tool failures throw. `timeout_ms` defaults to \
             {DEFAULT_EXECUTE_TIMEOUT_MS} ms and cannot exceed {} ms.",
            state.execute_timeout_hard_ms
        ),
        schema
            .as_object()
            .cloned()
            .expect("execute tool schema must be a JSON object"),
    )
    .with_output_schema::<sandbox::ExecuteOutcome>()
}

fn search_tools_tool() -> Tool {
    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "query": {"type": "string"},
            "server": {"type": "string"},
            "namespace": {"type": "string"},
            "limit": {"type": "integer", "minimum": 1, "maximum": 20, "default": 8}
        },
        "additionalProperties": false
    });
    let mut tool = Tool::new(
        "search_tools",
        "Search connected MCP tools by name, namespace, server, description, or argument. Read-only discovery; no upstream tool is called.",
        schema.as_object().cloned().expect("search schema object"),
    );
    tool.annotations = Some(rmcp::model::ToolAnnotations::new().read_only(true));
    tool
}
use std::io::Read as _;
