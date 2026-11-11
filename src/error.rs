use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("config error: {0}")]
    Config(String),
    #[error("upstream MCP error: {0}")]
    Upstream(String),
    #[error("idl error: {0}")]
    Idl(String),
    #[error("glue error: {0}")]
    Glue(String),
    #[error("workspace error: {0}")]
    Workspace(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
