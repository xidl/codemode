use serde::Serialize;
use serde_json::Value;

use crate::catalog::{namespace_for, snake_case, ServerCatalog};

#[derive(Debug, Clone, Serialize)]
pub struct CatalogTool {
    pub server: String,
    pub wire_server: String,
    pub namespace: String,
    pub tool: String,
    pub description: String,
    pub read_only: bool,
    pub arguments: Vec<CatalogArgument>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CatalogArgument {
    pub name: String,
    pub wire_name: String,
    pub kind: String,
    pub optional: bool,
}

pub fn tool_catalog(catalogs: &[ServerCatalog]) -> Vec<CatalogTool> {
    let mut tools = Vec::new();
    for catalog in catalogs {
        let all_names: Vec<String> = catalog.tools.iter().map(|tool| tool.name.clone()).collect();
        for tool in &catalog.tools {
            let namespace = namespace_for(
                &catalog.name,
                &tool.name,
                &all_names,
                &catalog.namespace_map,
            );
            let required: std::collections::HashSet<&str> = tool
                .input_schema
                .get("required")
                .and_then(Value::as_array)
                .map(|values| values.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            let arguments = tool
                .input_schema
                .get("properties")
                .and_then(Value::as_object)
                .map(|properties| {
                    properties
                        .iter()
                        .map(|(name, schema)| CatalogArgument {
                            name: snake_case(name),
                            wire_name: name.clone(),
                            kind: schema
                                .get("type")
                                .and_then(Value::as_str)
                                .unwrap_or("any")
                                .to_string(),
                            optional: !required.contains(name.as_str()),
                        })
                        .collect()
                })
                .unwrap_or_default();
            tools.push(CatalogTool {
                server: catalog.js_name.clone(),
                wire_server: catalog.name.clone(),
                namespace,
                tool: snake_case(&tool.name),
                description: tool.description.clone(),
                read_only: tool.read_only,
                arguments,
            });
        }
    }
    tools
}

pub fn search_catalog(
    catalogs: &[ServerCatalog],
    query: Option<&str>,
    server: Option<&str>,
    namespace: Option<&str>,
) -> Vec<CatalogTool> {
    let terms: Vec<String> = query
        .map(|query| {
            query
                .split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_')
                .filter(|term| !term.is_empty())
                .map(str::to_ascii_lowercase)
                .collect()
        })
        .unwrap_or_default();
    let tools = tool_catalog(catalogs);
    tools
        .into_iter()
        .filter(|tool| server.is_none_or(|server| tool.server.eq_ignore_ascii_case(server)))
        .filter(|tool| {
            namespace.is_none_or(|namespace| tool.namespace.eq_ignore_ascii_case(namespace))
        })
        .filter(|tool| {
            if terms.is_empty() {
                return true;
            }
            let haystack = format!(
                "{} {} {} {} {}",
                tool.server,
                tool.namespace,
                tool.tool,
                tool.description,
                tool.arguments
                    .iter()
                    .map(|argument| argument.name.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
            )
            .to_ascii_lowercase();
            terms.iter().all(|term| haystack.contains(term))
        })
        .collect()
}
