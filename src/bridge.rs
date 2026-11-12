use std::collections::{BTreeMap, HashMap};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::time::Duration;

use rmcp::model::{CallToolRequestParams, PaginatedRequestParams, Tool};
use rmcp::service::{RunningService, ServiceExt};
use rmcp::transport::streamable_http_client::{
    StreamableHttpClientTransport, StreamableHttpClientTransportConfig,
};
use rmcp::transport::TokioChildProcess;
use serde_json::Value;

use crate::catalog::{canonical_server_name, ServerCatalog, ToolInfo};
use crate::config::{ServerSpec, Transport};
use crate::error::{Error, Result};
use crate::sandbox::{ToolCallFailure, ToolCallFailureKind};

pub struct UpstreamManager {
    tx: Option<SyncSender<BridgeRequest>>,
    join: Option<std::thread::JoinHandle<()>>,
}

enum BridgeRequest {
    Discover {
        spec: ServerSpec,
        reply: SyncSender<std::result::Result<ServerCatalog, String>>,
    },
    Call {
        server: String,
        tool: String,
        arguments: Value,
        timeout: Duration,
        reply: SyncSender<std::result::Result<Value, ToolCallFailure>>,
    },
}

impl UpstreamManager {
    pub fn start() -> Result<Self> {
        let (tx, rx) = mpsc::sync_channel(16);
        let join = std::thread::Builder::new()
            .name("xidl-codemode-bridge".into())
            .spawn(move || bridge_thread(rx))
            .map_err(Error::Io)?;
        Ok(Self {
            tx: Some(tx),
            join: Some(join),
        })
    }

    pub fn discover(&self, spec: ServerSpec) -> Result<ServerCatalog> {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        self.tx
            .as_ref()
            .ok_or_else(|| Error::Upstream("bridge is shut down".into()))?
            .send(BridgeRequest::Discover {
                spec,
                reply: reply_tx,
            })
            .map_err(|_| Error::Upstream("bridge thread stopped".into()))?;
        reply_rx
            .recv()
            .map_err(|_| Error::Upstream("bridge reply dropped".into()))?
            .map_err(Error::Upstream)
    }

    pub fn call(
        &self,
        server: &str,
        tool: &str,
        arguments: Value,
        timeout: Duration,
    ) -> std::result::Result<Value, ToolCallFailure> {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        self.tx
            .as_ref()
            .ok_or_else(|| ToolCallFailure {
                kind: ToolCallFailureKind::Failed,
                message: "bridge is shut down".into(),
            })?
            .send(BridgeRequest::Call {
                server: server.to_string(),
                tool: tool.to_string(),
                arguments,
                timeout,
                reply: reply_tx,
            })
            .map_err(|_| ToolCallFailure {
                kind: ToolCallFailureKind::Failed,
                message: "bridge thread stopped".into(),
            })?;
        reply_rx
            .recv_timeout(timeout)
            .map_err(|err| ToolCallFailure {
                kind: ToolCallFailureKind::ToolTimeout,
                message: format!("tool call reply timeout: {err}"),
            })?
    }
}

impl Drop for UpstreamManager {
    fn drop(&mut self) {
        self.tx.take();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn bridge_thread(rx: Receiver<BridgeRequest>) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            tracing::error!("cannot build bridge runtime: {err}");
            return;
        }
    };
    let mut clients: HashMap<String, RunningService<rmcp::RoleClient, ()>> = HashMap::new();
    while let Ok(request) = rx.recv() {
        match request {
            BridgeRequest::Discover { spec, reply } => {
                let result = runtime.block_on(connect_and_discover(&spec));
                let result = result.map(|(catalog, client)| {
                    clients.insert(spec.name.clone(), client);
                    catalog
                });
                let _ = reply.send(result);
            }
            BridgeRequest::Call {
                server,
                tool,
                arguments,
                timeout,
                reply,
            } => {
                let result = runtime.block_on(async {
                    let client = clients.get(&server).ok_or_else(|| ToolCallFailure {
                        kind: ToolCallFailureKind::Failed,
                        message: format!("server {server} is not connected"),
                    })?;
                    let arguments = arguments.as_object().cloned().unwrap_or_default();
                    let params = CallToolRequestParams::new(tool).with_arguments(arguments);
                    match tokio::time::timeout(timeout, client.peer().call_tool(params)).await {
                        Ok(Ok(result)) => {
                            Ok(serde_json::to_value(result).map_err(|err| ToolCallFailure {
                                kind: ToolCallFailureKind::Failed,
                                message: format!("serialize tool result: {err}"),
                            })?)
                        }
                        Ok(Err(err)) => Err(ToolCallFailure {
                            kind: ToolCallFailureKind::Failed,
                            message: err.to_string(),
                        }),
                        Err(_) => Err(ToolCallFailure {
                            kind: ToolCallFailureKind::ToolTimeout,
                            message: "tool call timed out".to_string(),
                        }),
                    }
                });
                let _ = reply.send(result);
            }
        }
    }
}

async fn connect_and_discover(
    spec: &ServerSpec,
) -> std::result::Result<(ServerCatalog, RunningService<rmcp::RoleClient, ()>), String> {
    let client = match &spec.transport {
        Transport::Stdio {
            command,
            args,
            env,
            cwd,
        } => {
            let mut cmd = tokio::process::Command::new(command);
            cmd.args(args);
            cmd.env_clear();
            if let Some(path) = std::env::var_os("PATH") {
                cmd.env("PATH", path);
            }
            if let Some(home) = std::env::var_os("HOME") {
                cmd.env("HOME", home);
            }
            for (key, value) in env {
                cmd.env(key, value);
            }
            if let Some(cwd) = cwd {
                cmd.current_dir(cwd);
            }
            let transport = TokioChildProcess::new(cmd).map_err(|err| err.to_string())?;
            ().serve(transport)
                .await
                .map_err(|err| format!("initialize {}: {err}", spec.name))?
        }
        Transport::Http { url, headers } => {
            let mut config = StreamableHttpClientTransportConfig::with_uri(url.clone());
            let mut auth_header = None;
            let mut custom_headers = HashMap::new();
            for (name, value) in headers {
                if name.eq_ignore_ascii_case("authorization")
                    && value
                        .get(..7)
                        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("bearer "))
                {
                    auth_header = Some(value[7..].to_string());
                } else {
                    let name = name
                        .parse::<reqwest::header::HeaderName>()
                        .map_err(|err| format!("invalid header {name}: {err}"))?;
                    let value = value
                        .parse::<reqwest::header::HeaderValue>()
                        .map_err(|err| format!("invalid header value: {err}"))?;
                    custom_headers.insert(name, value);
                }
            }
            if let Some(token) = auth_header {
                config = config.auth_header(token);
            }
            if !custom_headers.is_empty() {
                config = config.custom_headers(custom_headers);
            }
            let transport =
                StreamableHttpClientTransport::with_client(reqwest::Client::new(), config);
            ().serve(transport)
                .await
                .map_err(|err| format!("initialize {}: {err}", spec.name))?
        }
    };
    let tools = list_all_tools(client.peer()).await?;
    let instructions = client
        .peer_info()
        .and_then(|info| info.instructions.clone());
    let catalog = ServerCatalog {
        name: spec.name.clone(),
        js_name: canonical_server_name(&spec.name),
        instructions,
        tools,
        namespace_map: BTreeMap::new(),
    };
    Ok((catalog, client))
}

async fn list_all_tools(
    peer: &rmcp::service::Peer<rmcp::RoleClient>,
) -> std::result::Result<Vec<ToolInfo>, String> {
    let mut tools = Vec::new();
    let mut cursor = None;
    loop {
        let params = cursor
            .take()
            .map(|cursor| PaginatedRequestParams::default().with_cursor(Some(cursor)));
        let page = peer
            .list_tools(params)
            .await
            .map_err(|err| format!("tools/list: {err}"))?;
        for tool in page.tools {
            tools.push(to_tool_info(tool));
        }
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    Ok(tools)
}

fn to_tool_info(tool: Tool) -> ToolInfo {
    let input_schema = serde_json::to_value(&*tool.input_schema).unwrap_or(Value::Null);
    let output_schema = tool
        .output_schema
        .as_ref()
        .and_then(|schema| serde_json::to_value(&**schema).ok());
    let annotations = tool.annotations.unwrap_or_default();
    ToolInfo {
        name: tool.name.to_string(),
        description: tool
            .description
            .map(|value| value.to_string())
            .unwrap_or_default(),
        input_schema,
        output_schema,
        read_only: annotations.read_only_hint.unwrap_or(false),
        destructive: annotations.destructive_hint.unwrap_or(false),
        idempotent: annotations.idempotent_hint.unwrap_or(false),
        open_world: annotations.open_world_hint.unwrap_or(false),
    }
}

impl crate::sandbox::ToolCaller for UpstreamManager {
    fn call_tool(
        &self,
        server: &str,
        tool: &str,
        arguments: Value,
        timeout: Duration,
    ) -> std::result::Result<Value, ToolCallFailure> {
        self.call(server, tool, arguments, timeout)
    }
}
