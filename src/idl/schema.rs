use std::collections::BTreeMap;

use serde_json::Value;

use crate::catalog::snake_case;

/// Resolve a JSON pointer into a `$defs`/`definitions` entry name. Only
/// root-level definition maps are supported; anything else falls back to
/// `any` at the call site.
pub(crate) fn ref_name(reference: &str) -> Option<&str> {
    let path = reference.strip_prefix("#/")?;
    let components: Vec<&str> = path.split('/').collect();
    let name = components.last()?;
    match components.len() {
        2 if components[0] == "$defs" || components[0] == "definitions" => Some(name),
        3 if components[0] == "components" && components[1] == "schemas" => Some(name),
        _ => None,
    }
}

pub(crate) fn collect_defs(schema: &Value, defs: &mut BTreeMap<String, Value>) {
    for key in ["$defs", "definitions"] {
        if let Some(entries) = schema.get(key).and_then(Value::as_object) {
            for (name, value) in entries {
                defs.entry(name.clone()).or_insert_with(|| value.clone());
            }
        }
    }
}

pub(crate) fn method_name(tool: &str) -> String {
    snake_case(tool)
}

pub(crate) fn doc_comment(text: &str) -> String {
    doc_comment_indented(text, 4)
}

pub(crate) fn doc_comment_indented(text: &str, indent: usize) -> String {
    if text.trim().is_empty() {
        return String::new();
    }
    let padding = " ".repeat(indent);
    let mut out = String::new();
    for line in text.lines().take(12) {
        let line = line.trim();
        if line.is_empty() {
            out.push_str(&format!("{padding}///\n"));
        } else {
            out.push_str(&format!(
                "{padding}/// {}\n",
                line.replace(['\r', '\u{0}'], "")
            ));
        }
    }
    out
}

pub(crate) fn escape(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}
