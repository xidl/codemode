use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::catalog::ServerCatalog;
use crate::error::{Error, Result};
use crate::manifest_types::Manifest;

pub fn generate(server: &str, bundle: &str) -> Result<Manifest> {
    let path = Path::new(server).with_extension("bundle.idl");
    let mut generator = xidlc::driver::Generator::new("codemode-manifest".to_string());
    let files = generator
        .generate_from_idl(bundle, &path, HashMap::new())
        .map_err(|err| Error::Manifest(format!("xidlc codemode-manifest failed: {err}")))?;
    let content = files
        .iter()
        .find(|file| file.path().ends_with("manifest.json"))
        .map(|file| file.content())
        .ok_or_else(|| Error::Manifest("plugin did not emit manifest.json".into()))?;
    serde_json::from_str(content).map_err(|err| {
        Error::Manifest(format!(
            "plugin emitted invalid manifest.json: {err}\n---\n{content}"
        ))
    })
}

pub fn validate(manifest: &Manifest, catalog: &ServerCatalog) -> Result<()> {
    if manifest.version != 1 {
        return Err(Error::Manifest(format!(
            "unsupported manifest version {}",
            manifest.version
        )));
    }
    if manifest.server != catalog.name {
        return Err(Error::Manifest(format!(
            "manifest server {:?} does not match catalog {:?}",
            manifest.server, catalog.name
        )));
    }
    let mut seen = HashSet::new();
    for (namespace, entry) in &manifest.namespaces {
        if namespace.is_empty() || entry.tools.is_empty() {
            return Err(Error::Manifest(format!(
                "empty namespace {namespace:?} in manifest"
            )));
        }
        for tool in &entry.tools {
            if !catalog
                .tools
                .iter()
                .any(|candidate| candidate.name == tool.mcp)
            {
                return Err(Error::Manifest(format!(
                    "manifest references unknown MCP tool {:?}",
                    tool.mcp
                )));
            }
            if !seen.insert(tool.mcp.clone()) {
                return Err(Error::Manifest(format!(
                    "duplicate MCP tool {:?} in manifest",
                    tool.mcp
                )));
            }
            if tool.js.is_empty() {
                return Err(Error::Manifest(format!(
                    "tool {:?} has empty JS name",
                    tool.mcp
                )));
            }
        }
    }
    let mut js_names = HashSet::new();
    for entry in manifest.namespaces.values() {
        for tool in &entry.tools {
            if !js_names.insert(tool.js.clone()) {
                return Err(Error::Manifest(format!(
                    "duplicate JS tool name {:?} in manifest",
                    tool.js
                )));
            }
        }
    }
    if seen.len() != catalog.tools.len() {
        let missing: Vec<&str> = catalog
            .tools
            .iter()
            .filter(|tool| !seen.contains(&tool.name))
            .map(|tool| tool.name.as_str())
            .collect();
        return Err(Error::Manifest(format!(
            "manifest covers {} of {} tools; missing: {:?}",
            seen.len(),
            catalog.tools.len(),
            missing
        )));
    }
    Ok(())
}
