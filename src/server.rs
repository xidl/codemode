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

use crate::sandbox::{self, RuntimeServer, ToolCaller};

pub struct AppState {
    pub bridge: Arc<dyn ToolCaller>,
    pub servers: Vec<RuntimeServer>,
    pub instructions: String,
    pub errors: Vec<String>,
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
    /// Execution timeout in milliseconds (default 10000, max 60000).
    pub timeout_ms: Option<u64>,
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
        Ok(ListToolsResult::with_all_items(vec![execute_tool()]))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
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
        let args: ExecuteArgs =
            serde_json::from_value(Value::Object(arguments)).map_err(|err| {
                McpError::invalid_params(format!("invalid execute arguments: {err}"), None)
            })?;
        let timeout = Duration::from_millis(args.timeout_ms.unwrap_or(10_000).clamp(100, 60_000));
        let bridge = self.state.bridge.clone();
        let servers = self.state.servers.clone();
        let code = args.code;
        let outcome = tokio::task::spawn_blocking(move || {
            sandbox::execute(&bridge, &servers, &code, timeout)
        })
        .await
        .map_err(|err| McpError::internal_error(format!("sandbox task failed: {err}"), None))?;
        let text = serde_json::to_string(&outcome)
            .map_err(|err| McpError::internal_error(format!("serialize result: {err}"), None))?;
        Ok(CallToolResult::success(vec![ContentBlock::text(text)]).into())
    }
}

fn execute_tool() -> Tool {
    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "code": {
                "type": "string",
                "description": "JavaScript source. Use the injected global `mcp` object, e.g. `return unwrap(mcp.server.tool({ ... }));`"
            },
            "timeout_ms": {
                "type": "integer",
                "minimum": 100,
                "maximum": 60000,
                "default": 10000
            }
        },
        "required": ["code"],
        "additionalProperties": false
    });
    Tool::new(
        "execute",
        "Execute JavaScript in a QuickJS sandbox. MCP tools are injected as `mcp.<server>.<tool>(args)`.",
        schema
            .as_object()
            .cloned()
            .expect("execute tool schema must be a JSON object"),
    )
}
