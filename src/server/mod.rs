use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer, ServerHandler};
use serde_json::Value;

mod execute_args;

pub use execute_args::{
    read_execute_source, validate_execute_args, ExecuteArgs, FileReadError, SearchToolsArgs,
};

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
    pub run_log_root: Option<PathBuf>,
    pub tool_index: Vec<ServerCatalog>,
}

#[derive(Clone)]
pub struct CodemodeServer {
    state: Arc<AppState>,
}

const DEFAULT_EXECUTE_TIMEOUT_MS: u64 = 10_000;
const MAX_CODE_BYTES: usize = sandbox::MAX_CODE_BYTES;

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
        let run_logger = self.create_run_logger(mode, &args, timeout_ms);
        let outcome = match mode {
            "code" => {
                let code = args.code.unwrap_or_default();
                tokio::task::spawn_blocking(move || {
                    sandbox::execute_with_run_logger(
                        &bridge,
                        &servers,
                        &catalogs,
                        &code,
                        timeout,
                        sandbox::RunSource {
                            kind: "code",
                            path: None,
                        },
                        &run_logger,
                    )
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
                            sandbox::execute_with_run_logger(
                                &bridge,
                                &servers,
                                &catalogs,
                                &code,
                                remaining.max(Duration::from_millis(100)),
                                sandbox::RunSource {
                                    kind: "file",
                                    path: Some(path.clone()),
                                },
                                &run_logger,
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
                            run: run_logger.paths().cloned(),
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

impl CodemodeServer {
    fn create_run_logger(
        &self,
        mode: &str,
        args: &ExecuteArgs,
        timeout_ms: u64,
    ) -> std::sync::Arc<crate::runlog::RunLogger> {
        let Some(root) = &self.state.run_log_root else {
            return std::sync::Arc::new(crate::runlog::RunLogger::disabled());
        };
        let run_json = serde_json::json!({
            "source": mode,
            "path": args.path,
            "code_bytes": args.code.as_deref().map(str::len).unwrap_or(0),
            "timeout_ms": timeout_ms,
            "version": env!("CARGO_PKG_VERSION"),
        });
        std::sync::Arc::new(crate::runlog::RunLogger::create(root, &run_json))
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
