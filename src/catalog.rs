use std::collections::{BTreeMap, HashMap};

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

/// Normalize a wire name to snake_case: camelCase, PascalCase, and
/// separator-delimited words all collapse to lowercase underscore form.
pub fn snake_case(raw: &str) -> String {
    let mut words: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut chars = raw.chars().peekable();
    let mut prev_upper = false;
    let mut prev_alnum = false;
    while let Some(ch) = chars.next() {
        if !ch.is_ascii_alphanumeric() {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            prev_alnum = false;
            prev_upper = false;
            continue;
        }
        let next_lower = chars.peek().is_some_and(|next| next.is_ascii_lowercase());
        let boundary = (prev_alnum && !prev_upper && ch.is_ascii_uppercase())
            || (prev_upper && ch.is_ascii_uppercase() && next_lower)
            || (prev_upper && ch.is_ascii_lowercase() && current.chars().count() > 1);
        if boundary && !current.is_empty() {
            words.push(std::mem::take(&mut current));
        }
        current.push(ch);
        prev_alnum = true;
        prev_upper = ch.is_ascii_uppercase();
    }
    if !current.is_empty() {
        words.push(current);
    }
    let leading = raw.chars().take_while(|ch| *ch == '_').count();
    let trailing = raw.chars().rev().take_while(|ch| *ch == '_').count();
    let mut out = words.join("_").to_ascii_lowercase();
    if out.is_empty() {
        return sanitize_ident(raw);
    }
    out = format!("{}{}{}", "_".repeat(leading), out, "_".repeat(trailing));
    if out.chars().next().is_some_and(|ch| ch.is_ascii_digit()) {
        out.insert(0, '_');
    }
    out
}

pub fn canonical_server_name(raw: &str) -> String {
    snake_case(raw)
}

/// Reject server catalogs whose snake_case names collide, so that generated
/// tool keys and namespaces stay unique instead of silently overwriting each
/// other.
pub fn validate_server_catalog(catalog: &ServerCatalog) -> std::result::Result<(), String> {
    let all: Vec<String> = catalog.tools.iter().map(|tool| tool.name.clone()).collect();
    let mut tool_names: HashMap<String, String> = HashMap::new();
    let mut namespace_origins: HashMap<String, (String, String)> = HashMap::new();
    for tool in &catalog.tools {
        let name = snake_case(&tool.name);
        if let Some(existing) = tool_names.get(&name) {
            return Err(format!(
                "server {}: tool names {:?} and {:?} both map to {:?}",
                catalog.name, existing, tool.name, name
            ));
        }
        tool_names.insert(name, tool.name.clone());
        let (namespace, origin) =
            namespace_with_origin(&catalog.name, &tool.name, &all, &catalog.namespace_map);
        match namespace_origins.get(&namespace) {
            Some((existing_origin, existing_tool)) if existing_origin != &origin => {
                return Err(format!(
                    "server {}: namespace {:?} is produced by conflicting origins {:?} (tool {:?}) and {:?} (tool {:?})",
                    catalog.name, namespace, existing_origin, existing_tool, origin, tool.name
                ));
            }
            Some(_) => {}
            None => {
                namespace_origins.insert(namespace, (origin, tool.name.clone()));
            }
        }
    }
    Ok(())
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
    namespace_with_origin(server, tool, all, map).0
}

fn namespace_with_origin(
    server: &str,
    tool: &str,
    all: &[String],
    map: &BTreeMap<String, String>,
) -> (String, String) {
    if let Some(value) = map.get(tool) {
        return (snake_case(value), format!("map override for {tool:?}"));
    }
    let canonical_server = snake_case(server);
    for separator in ['.', ':', '_', '-'] {
        if let Some((prefix, _)) = tool.split_once(separator) {
            if separator == '_' || separator == '-' {
                let count = all
                    .iter()
                    .filter(|name| {
                        name.starts_with(&format!("{prefix}{separator}")) || *name == prefix
                    })
                    .count();
                if count < 2 || snake_case(prefix) == canonical_server {
                    continue;
                }
            }
            return (snake_case(prefix), format!("prefix {prefix:?}"));
        }
    }
    ("default".to_string(), "default".to_string())
}
