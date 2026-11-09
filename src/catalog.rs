use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ToolInfo {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub output_schema: Option<Value>,
    pub read_only: bool,
    pub destructive: bool,
    pub idempotent: bool,
    pub open_world: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerCatalog {
    pub name: String,
    pub js_name: String,
    pub instructions: Option<String>,
    pub tools: Vec<ToolInfo>,
    #[serde(default)]
    pub namespace_map: BTreeMap<String, String>,
}

pub fn sanitize_ident(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for (index, ch) in raw.chars().enumerate() {
        let valid = ch.is_ascii_alphanumeric() || ch == '_';
        if valid {
            if index == 0 && ch.is_ascii_digit() {
                out.push('_');
            }
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        out.push_str("tool");
    }
    out
}

pub fn is_valid_ident(raw: &str) -> bool {
    let mut chars = raw.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' => {}
        _ => return false,
    }
    chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

pub fn pascal_case(raw: &str) -> String {
    let mut out = String::new();
    let mut upper = true;
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() {
            if upper {
                out.extend(ch.to_uppercase());
                upper = false;
            } else {
                out.push(ch);
            }
        } else {
            upper = true;
        }
    }
    if out.is_empty() {
        out.push_str("Generated");
    }
    if out.chars().next().is_some_and(|ch| ch.is_ascii_digit()) {
        out.insert(0, 'T');
    }
    out
}

pub fn namespace_for(
    server: &str,
    tool: &str,
    all: &[String],
    map: &BTreeMap<String, String>,
) -> String {
    if let Some(value) = map.get(tool) {
        return value.clone();
    }
    if let Some((prefix, _)) = tool.split_once('.') {
        return sanitize_ident(prefix);
    }
    if let Some((prefix, _)) = tool.split_once(':') {
        return sanitize_ident(prefix);
    }
    if let Some((prefix, _)) = tool.split_once('_') {
        let count = all
            .iter()
            .filter(|name| name.starts_with(&format!("{prefix}_")) || *name == prefix)
            .count();
        if count >= 2 && prefix != server {
            return sanitize_ident(prefix);
        }
    }
    if let Some((prefix, _)) = tool.split_once('-') {
        let count = all
            .iter()
            .filter(|name| name.starts_with(&format!("{prefix}-")) || *name == prefix)
            .count();
        if count >= 2 && prefix != server {
            return sanitize_ident(prefix);
        }
    }
    "default".to_string()
}
