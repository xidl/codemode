use std::sync::Arc;
use std::time::Duration;

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
    pub instructions: String,
    pub errors: Vec<String>,
    pub execute_timeout_hard_ms: u64,
    pub execute_catalog: Vec<String>,
    pub tool_index: Vec<ServerCatalog>,
}

#[derive(Clone)]
pub struct CodemodeServer {
    state: Arc<AppState>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecuteArgs {
    /// JavaScript source. `return` at top level is supported.
    pub code: String,
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
                    "catalog": self.state.execute_catalog,
                    "examples": [
                        "return mcp.<server>.<tool>(arg1, arg2);",
                        "return mcp[\"server-name\"][\"tool.name\"](arg);"
                    ],
                    "hint": "Read the API docs directory for signatures; return values are plain JSON."
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
        let code = args.code;
        let outcome = tokio::task::spawn_blocking(move || {
            sandbox::execute(&bridge, &servers, &catalogs, &code, timeout)
        })
        .await
        .map_err(|err| McpError::internal_error(format!("sandbox task failed: {err}"), None))?;
        let text = serde_json::to_string(&outcome)
            .map_err(|err| McpError::internal_error(format!("serialize result: {err}"), None))?;
        Ok(CallToolResult::success(vec![ContentBlock::text(text)]).into())
    }
}

fn execute_tool_with_limit(state: &AppState) -> Tool {
    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "code": {
                "type": "string",
                "description": "JavaScript source. Use the injected global `mcp` object with positional arguments, e.g. `return mcp.server.tool(arg1, arg2);`"
            },
            "timeout_ms": {
                "type": "integer",
                "minimum": 100,
                "maximum": state.execute_timeout_hard_ms,
                "default": 10000
            }
        },
        "required": ["code"],
        "additionalProperties": false
    });
    Tool::new(
        "execute",
        format!(
            "Execute JavaScript in a QuickJS sandbox. MCP tools are injected as \
             `mcp.<server>.<tool>(arg1, arg2, ...)` with positional arguments. \
             Skip an optional argument before a required one with `null`, and use \
             bracket access for invalid identifiers, for example \
             `mcp[\"server-name\"][\"tool.name\"](arg)`. Returns are plain JSON \
             values and tool failures throw. `timeout_ms` defaults to \
             {DEFAULT_EXECUTE_TIMEOUT_MS} ms and cannot exceed {} ms.",
            state.execute_timeout_hard_ms
        ),
        schema
            .as_object()
            .cloned()
            .expect("execute tool schema must be a JSON object"),
    )
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
