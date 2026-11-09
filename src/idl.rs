use std::collections::{BTreeMap, HashSet};

use serde_json::{Map, Value};

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
    let mut agents = String::new();
    agents.push_str(&format!("# {}\n\n", catalog.name));
    agents.push_str(&format!(
        "Use the `execute` tool and call APIs as `mcp.{}.<tool>(args)`.\n\n",
        catalog.js_name
    ));

    for (namespace, tools) in grouped {
        let mut builder = IdlBuilder::new(&catalog.name, &namespace);
        for tool in &tools {
            builder.add_tool(tool);
        }
        let body = builder.finish(false);
        let body_with_result = builder.finish(true);
        let idl = format!(
            "#pragma xidlc package {}\n#pragma xidlc version 1.0.0\n\n{}",
            catalog.name, body_with_result
        );
        bundle_parts.push(body);
        namespaces.push(GeneratedNamespace {
            name: namespace.clone(),
            idl,
        });

        agents.push_str(&format!("## /{}/{}.idl\n\n", catalog.name, namespace));
        for tool in &tools {
            let js_name = method_name(&tool.name);
            agents.push_str(&format!(
                "- `mcp.{}.{}({})`",
                catalog.js_name,
                js_name,
                compact_args(&tool.input_schema)
            ));
            if tool.read_only {
                agents.push_str(" [read-only]");
            }
            let first_line = tool
                .description
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .unwrap_or("");
            if !first_line.is_empty() {
                agents.push_str(" — ");
                agents.push_str(first_line);
            }
            agents.push('\n');
        }
        agents.push('\n');
    }

    let bundle = format!(
        "#pragma xidlc package {}\n#pragma xidlc version 1.0.0\n\n\
         /// Raw MCP tools/call result.\n\
         struct McpToolResult {{\n    boolean isError;\n    sequence<any> content;\n    any structuredContent;\n}};\n\n{}",
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
    result_struct: bool,
    warnings: Vec<String>,
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
            result_struct: false,
            warnings: Vec::new(),
        }
    }

    fn add_tool(&mut self, tool: &ToolInfo) {
        let tool_pascal = pascal_case(&method_name(&tool.name));
        let input_name = self.unique_name(&format!(
            "{}{}{}Input",
            self.server_pascal, self.namespace_pascal, tool_pascal
        ));
        self.emit_struct(&input_name, &tool.input_schema);
        if tool.output_schema.is_some() {
            self.warnings.push(format!(
                "tool {} has outputSchema; using McpToolResult (typed output not implemented)",
                tool.name
            ));
        }
        self.result_struct = true;
        let method = method_name(&tool.name);
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
            "{docs}{annotations}    McpToolResult {method}(in {input_name} input);"
        ));
    }

    fn finish(&self, include_result: bool) -> String {
        let mut out = String::new();
        if include_result && self.result_struct {
            out.push_str(
                "/// Raw MCP tools/call result.\nstruct McpToolResult {\n    boolean isError;\n    sequence<any> content;\n    any structuredContent;\n};\n\n",
            );
        }
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
            self.warnings
                .push(format!("$ref in {name_hint} degraded to any"));
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
    if text.trim().is_empty() {
        return String::new();
    }
    let mut out = String::new();
    for line in text.lines().take(12) {
        let line = line.trim();
        if line.is_empty() {
            out.push_str("    ///\n");
        } else {
            out.push_str(&format!("    /// {}\n", line.replace(['\r', '\u{0}'], "")));
        }
    }
    out
}

fn escape(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

fn compact_args(schema: &Value) -> String {
    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        return String::new();
    };
    let required: HashSet<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|values| values.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let mut parts = Vec::new();
    for (key, value) in properties {
        let ty = short_type(value);
        if required.contains(key.as_str()) {
            parts.push(format!("{key}: {ty}"));
        } else {
            parts.push(format!("{key}?: {ty}"));
        }
    }
    parts.join(", ")
}

fn short_type(schema: &Value) -> String {
    match schema.get("type").and_then(Value::as_str) {
        Some("string") => "string".into(),
        Some("integer") | Some("number") => "number".into(),
        Some("boolean") => "boolean".into(),
        Some("array") => format!(
            "{}[]",
            schema
                .get("items")
                .map(short_type)
                .unwrap_or_else(|| "any".into())
        ),
        Some("object") => "object".into(),
        _ => "any".into(),
    }
}

pub fn schema_property_names(schema: &Value) -> Vec<String> {
    schema
        .get("properties")
        .and_then(Value::as_object)
        .map(|properties: &Map<String, Value>| properties.keys().cloned().collect())
        .unwrap_or_default()
}
