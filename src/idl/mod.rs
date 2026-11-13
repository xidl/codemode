use std::collections::BTreeMap;

use crate::catalog::{namespace_for, ServerCatalog, ToolInfo};

mod builder;
mod schema;

use builder::IdlBuilder;

pub struct GeneratedNamespace {
    pub name: String,
    pub idl: String,
}

pub struct GeneratedServer {
    pub catalog: ServerCatalog,
    pub namespaces: Vec<GeneratedNamespace>,
    pub bundle: String,
    pub agents_md: String,
}

pub fn generate(catalog: &ServerCatalog) -> GeneratedServer {
    let all_names: Vec<String> = catalog.tools.iter().map(|tool| tool.name.clone()).collect();
    let mut grouped: BTreeMap<String, Vec<&ToolInfo>> = BTreeMap::new();
    for tool in &catalog.tools {
        let namespace = namespace_for(
            &catalog.name,
            &tool.name,
            &all_names,
            &catalog.namespace_map,
        );
        grouped.entry(namespace).or_default().push(tool);
    }

    let mut namespaces = Vec::new();
    let mut bundle_parts = Vec::new();
    let agents = catalog
        .instructions
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(|text| format!("{text}\n"))
        .unwrap_or_default();

    for (namespace, tools) in grouped {
        let mut builder = IdlBuilder::new(&catalog.name, &namespace);
        for tool in &tools {
            builder.add_tool(tool);
        }
        let body = builder.finish();
        let idl = format!(
            "#pragma xidlc package {}\n#pragma xidlc version 1.0.0\n\n{}",
            catalog.name, body
        );
        bundle_parts.push(body);
        namespaces.push(GeneratedNamespace {
            name: namespace.clone(),
            idl,
        });
    }

    let bundle = format!(
        "#pragma xidlc package {}\n#pragma xidlc version 1.0.0\n\n\
         {}",
        catalog.name,
        bundle_parts.join("\n")
    );

    GeneratedServer {
        catalog: catalog.clone(),
        namespaces,
        bundle,
        agents_md: agents,
    }
}
