use std::collections::{BTreeMap, HashSet};

use serde_json::Value;

use crate::catalog::{
    is_valid_ident, namespace_for, pascal_case, sanitize_ident, ServerCatalog, ToolInfo,
};

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

struct IdlBuilder {
    server: String,
    server_pascal: String,
    namespace: String,
    namespace_pascal: String,
    structs: Vec<String>,
    used: HashSet<String>,
    methods: Vec<String>,
}

impl IdlBuilder {
    fn new(server: &str, namespace: &str) -> Self {
        Self {
            server: server.to_string(),
            server_pascal: pascal_case(server),
            namespace: namespace.to_string(),
            namespace_pascal: pascal_case(namespace),
            structs: Vec::new(),
            used: HashSet::new(),
            methods: Vec::new(),
        }
    }

    fn add_tool(&mut self, tool: &ToolInfo) {
        let tool_pascal = pascal_case(&method_name(&tool.name));
        let method = method_name(&tool.name);
        let required: HashSet<&str> = tool
            .input_schema
            .get("required")
            .and_then(Value::as_array)
            .map(|values| values.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let mut params = Vec::new();
        if let Some(properties) = tool
            .input_schema
            .get("properties")
            .and_then(Value::as_object)
        {
            for (key, value) in properties {
                let param_name = if is_valid_ident(key) {
                    key.clone()
                } else {
                    sanitize_ident(key)
                };
                let nested_name = format!(
                    "{}{}{}{}",
                    self.server_pascal,
                    self.namespace_pascal,
                    tool_pascal,
                    pascal_case(key)
                );
                let ty = self.type_for(value, &nested_name);
                let mut param = String::new();
                if let Some(desc) = value.get("description").and_then(Value::as_str) {
                    param.push_str(&doc_comment_indented(desc, 8));
                }
                let mut annotations = Vec::new();
                if !required.contains(key.as_str()) {
                    annotations.push("@optional".to_string());
                }
                if param_name != *key {
                    annotations.push(format!("@rename(\"{}\")", escape(key)));
                }
                if !annotations.is_empty() {
                    param.push_str(&format!("        {}\n", annotations.join(" ")));
                }
                param.push_str(&format!("        {ty} {param_name}"));
                params.push(param);
            }
        }
        let param_block = if params.is_empty() {
            String::new()
        } else {
            format!("\n{}\n    ", params.join(",\n"))
        };
        let mut annotations = String::new();
        annotations.push_str(&format!("    @mcp_tool(\"{}\")\n", escape(&tool.name)));
        if tool.read_only {
            annotations.push_str("    @mcp_read_only\n");
        }
        if tool.destructive {
            annotations.push_str("    @mcp_destructive\n");
        }
        if tool.idempotent {
            annotations.push_str("    @mcp_idempotent\n");
        }
        if tool.open_world {
            annotations.push_str("    @mcp_open_world\n");
        }
        let docs = doc_comment(&tool.description);
        self.methods.push(format!(
            "{docs}{annotations}    any {method}({param_block});"
        ));
    }

    fn finish(&self) -> String {
        let mut out = String::new();
        for item in &self.structs {
            out.push_str(item);
            out.push_str("\n\n");
        }
        out.push_str(&format!(
            "@mcp_server(\"{}\")\n@mcp_namespace(\"{}\")\ninterface {}{}Api {{\n",
            escape(&self.server),
            escape(&self.namespace),
            self.server_pascal,
            self.namespace_pascal
        ));
        for method in &self.methods {
            out.push_str(method);
            out.push('\n');
        }
        out.push_str("};\n");
        out
    }

    fn unique_name(&mut self, base: &str) -> String {
        let mut name = base.to_string();
        let mut counter = 2;
        while !self.used.insert(name.clone()) {
            name = format!("{base}{counter}");
            counter += 1;
        }
        name
    }

    fn emit_struct(&mut self, name: &str, schema: &Value) {
        let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
            return;
        };
        let required: HashSet<&str> = schema
            .get("required")
            .and_then(Value::as_array)
            .map(|values| values.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let mut fields = Vec::new();
        for (key, value) in properties {
            let field_name = if is_valid_ident(key) {
                key.clone()
            } else {
                sanitize_ident(key)
            };
            let nested_name = format!("{name}{}", pascal_case(key));
            let ty = self.type_for(value, &nested_name);
            let mut annotations = Vec::new();
            if !required.contains(key.as_str()) {
                annotations.push("@optional".to_string());
            }
            if field_name != *key {
                annotations.push(format!("@rename(\"{}\")", escape(key)));
            }
            let mut field = String::new();
            if let Some(desc) = value.get("description").and_then(Value::as_str) {
                field.push_str(&doc_comment(desc));
            }
            field.push_str("    ");
            if !annotations.is_empty() {
                field.push_str(&annotations.join(" "));
                field.push(' ');
            }
            field.push_str(&format!("{ty} {field_name};"));
            fields.push(field);
        }
        if fields.is_empty() {
            self.structs.push(format!("struct {name} {{\n}};"));
        } else {
            self.structs
                .push(format!("struct {name} {{\n{}\n}};", fields.join("\n")));
        }
    }

    fn type_for(&mut self, schema: &Value, name_hint: &str) -> String {
        if let Some(types) = schema.get("type") {
            if let Some(kind) = types.as_str() {
                return self.type_for_kind(kind, schema, name_hint);
            }
            if let Some(list) = types.as_array() {
                let non_null: Vec<&str> = list
                    .iter()
                    .filter_map(Value::as_str)
                    .filter(|kind| *kind != "null")
                    .collect();
                if non_null.len() == 1 {
                    return self.type_for_kind(non_null[0], schema, name_hint);
                }
                return "any".to_string();
            }
        }
        if schema.get("properties").is_some() {
            return self.type_for_kind("object", schema, name_hint);
        }
        if schema.get("$ref").is_some() {
            return "any".to_string();
        }
        "any".to_string()
    }

    fn type_for_kind(&mut self, kind: &str, schema: &Value, name_hint: &str) -> String {
        match kind {
            "string" => "string".to_string(),
            "integer" => "int64".to_string(),
            "number" => "double".to_string(),
            "boolean" => "boolean".to_string(),
            "array" => {
                let item = schema.get("items").cloned().unwrap_or(Value::Null);
                let item_type = self.type_for(&item, &format!("{name_hint}Item"));
                format!("sequence<{item_type}>")
            }
            "object" => {
                let has_properties = schema
                    .get("properties")
                    .and_then(Value::as_object)
                    .is_some_and(|properties| !properties.is_empty());
                if has_properties {
                    let name = self.unique_name(name_hint);
                    self.emit_struct(&name, schema);
                    name
                } else if let Some(additional) = schema.get("additionalProperties") {
                    if additional.is_boolean() {
                        "map<string, any>".to_string()
                    } else {
                        let value_type = self.type_for(additional, &format!("{name_hint}Value"));
                        format!("map<string, {value_type}>")
                    }
                } else {
                    "any".to_string()
                }
            }
            "null" => "any".to_string(),
            _ => "any".to_string(),
        }
    }
}

fn method_name(tool: &str) -> String {
    if is_valid_ident(tool) {
        tool.to_string()
    } else {
        sanitize_ident(tool)
    }
}

fn doc_comment(text: &str) -> String {
    doc_comment_indented(text, 4)
}

fn doc_comment_indented(text: &str, indent: usize) -> String {
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

fn escape(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}
