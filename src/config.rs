use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::{Error, Result};

#[derive(Debug, Clone, Deserialize, Default)]
struct ConfigFile {
    #[serde(rename = "mcpServers", alias = "servers", default)]
    servers: BTreeMap<String, RawServer>,
    #[serde(
        rename = "executeTimeoutHardMs",
        alias = "execute_timeout_hard_ms",
        default
    )]
    execute_timeout_hard_ms: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct RawServer {
    #[serde(rename = "type")]
    transport_type: Option<String>,
    command: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    cwd: Option<String>,
    url: Option<String>,
    #[serde(default)]
    headers: BTreeMap<String, String>,
    disabled: Option<bool>,
    enabled: Option<bool>,
}

#[derive(Debug, Clone)]
pub enum Transport {
    Stdio {
        command: String,
        args: Vec<String>,
        env: BTreeMap<String, String>,
        cwd: Option<PathBuf>,
    },
    Http {
        url: String,
        headers: BTreeMap<String, String>,
    },
}

#[derive(Debug, Clone)]
pub struct ServerSpec {
    pub name: String,
    pub transport: Transport,
}

#[derive(Debug, Clone)]
pub struct LoadedConfig {
    pub servers: Vec<ServerSpec>,
    pub execute_timeout_hard_ms: u64,
}

const DEFAULT_EXECUTE_TIMEOUT_HARD_MS: u64 = 60_000;

pub fn default_config_path() -> Option<PathBuf> {
    let config_home = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    let dir = config_home.join("mcp");
    for name in ["config.toml", "mcp.json"] {
        let candidate = dir.join(name);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

pub fn load(path: Option<&Path>) -> Result<LoadedConfig> {
    let path = match path {
        Some(path) => path.to_path_buf(),
        None => default_config_path()
            .ok_or_else(|| Error::Config("no MCP config found; pass --config".into()))?,
    };
    let raw = std::fs::read_to_string(&path)
        .map_err(|err| Error::Config(format!("cannot read {}: {err}", path.display())))?;
    let config: ConfigFile = parse_config(&path, &raw)?;
    let current_exe = std::env::current_exe().ok();
    let mut servers = Vec::new();
    for (name, server) in config.servers {
        if server.disabled == Some(true) || server.enabled == Some(false) {
            continue;
        }
        if name == "xidl-codemode" {
            continue;
        }
        let transport = normalize_transport(&name, server)?;
        if let Transport::Stdio { command, .. } = &transport {
            if current_exe
                .as_ref()
                .and_then(|exe| exe.to_str())
                .is_some_and(|exe| exe == command)
            {
                continue;
            }
        }
        servers.push(ServerSpec { name, transport });
    }
    if servers.is_empty() {
        return Err(Error::Config("no enabled MCP servers in config".into()));
    }
    Ok(LoadedConfig {
        servers,
        execute_timeout_hard_ms: config
            .execute_timeout_hard_ms
            .unwrap_or(DEFAULT_EXECUTE_TIMEOUT_HARD_MS)
            .max(100),
    })
}

fn parse_config(path: &Path, raw: &str) -> Result<ConfigFile> {
    let is_toml = path.extension().is_some_and(|ext| ext == "toml");
    if is_toml {
        return toml::from_str(raw)
            .map_err(|err| Error::Config(format!("invalid TOML {}: {err}", path.display())));
    }
    match serde_json::from_str(raw) {
        Ok(config) => Ok(config),
        Err(json_err) => toml::from_str(raw).map_err(|toml_err| {
            Error::Config(format!(
                "invalid JSON ({}), and invalid TOML ({})",
                json_err, toml_err
            ))
        }),
    }
}

fn normalize_transport(name: &str, server: RawServer) -> Result<Transport> {
    let kind = server
        .transport_type
        .as_deref()
        .unwrap_or(if server.command.is_some() {
            "stdio"
        } else {
            "http"
        });
    match kind {
        "stdio" => {
            let command = server
                .command
                .ok_or_else(|| Error::Config(format!("server {name}: stdio requires command")))?;
            Ok(Transport::Stdio {
                command,
                args: server.args,
                env: server.env,
                cwd: server.cwd.map(PathBuf::from),
            })
        }
        "http" | "sse" | "streamable-http" | "streamable_http" => {
            let url = server
                .url
                .ok_or_else(|| Error::Config(format!("server {name}: http requires url")))?;
            Ok(Transport::Http {
                url,
                headers: server.headers,
            })
        }
        other => Err(Error::Config(format!(
            "server {name}: unsupported transport type {other}"
        ))),
    }
}

pub fn sample_config() -> &'static str {
    r#"{
  "mcpServers": {
    "codegraph": {
      "type": "stdio",
      "command": "codegraph",
      "args": ["serve", "--mcp"]
    }
  }
}"#
}
