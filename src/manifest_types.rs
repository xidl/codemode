use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    pub server: String,
    pub namespaces: BTreeMap<String, ManifestNamespace>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestNamespace {
    pub idl: String,
    pub interface: String,
    pub tools: Vec<ManifestTool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestTool {
    pub js: String,
    pub mcp: String,
    pub input: String,
    pub output: String,
    #[serde(rename = "readOnly", default)]
    pub read_only: bool,
    #[serde(default)]
    pub destructive: bool,
    #[serde(default)]
    pub idempotent: bool,
    #[serde(rename = "openWorld", default)]
    pub open_world: bool,
}
