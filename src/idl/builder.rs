use std::collections::{BTreeMap, HashSet};

use serde_json::Value;

use crate::catalog::{pascal_case, snake_case, ToolInfo};

use super::schema::{
    collect_defs, doc_comment, doc_comment_indented, escape, method_name, ref_name,
};

pub(crate) struct IdlBuilder {
    server: String,
    server_pascal: String,
    namespace: String,
    namespace_pascal: String,
    structs: Vec<String>,
    used: HashSet<String>,
    methods: Vec<String>,
    defs: BTreeMap<String, Value>,
    resolving: HashSet<String>,
    resolved_names: BTreeMap<String, String>,
    current_struct: Vec<String>,
}

impl IdlBuilder {
    pub(crate) fn new(server: &str, namespace: &str) -> Self {
        Self {
            server: server.to_string(),
            server_pascal: pascal_case(server),
            namespace: namespace.to_string(),
            namespace_pascal: pascal_case(namespace),
            structs: Vec::new(),
            used: HashSet::new(),
            methods: Vec::new(),
            defs: BTreeMap::new(),
            resolving: HashSet::new(),
            resolved_names: BTreeMap::new(),
            current_struct: Vec::new(),
        }
    }

    pub(crate) fn add_tool(&mut self, tool: &ToolInfo) {
        self.defs.clear();
        self.resolving.clear();
        self.resolved_names.clear();
        collect_defs(&tool.input_schema, &mut self.defs);
        if let Some(output) = &tool.output_schema {
            collect_defs(output, &mut self.defs);
        }
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
                let param_name = snake_case(key);
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
        if method != tool.name {
            annotations.push_str(&format!("    @rename(\"{}\")\n", escape(&tool.name)));
        }
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
        let return_type = match &tool.output_schema {
            Some(schema) => {
                let hint = format!(
                    "{}{}{}Output",
                    self.server_pascal, self.namespace_pascal, tool_pascal
                );
                self.type_for(schema, &hint)
            }
            None => "any".to_string(),
        };
        self.methods.push(format!(
            "{docs}{annotations}    {return_type} {method}({param_block});"
        ));
    }

    pub(crate) fn finish(&self) -> String {
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
        self.current_struct.push(name.to_string());
        let required: HashSet<&str> = schema
            .get("required")
            .and_then(Value::as_array)
            .map(|values| values.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let mut fields = Vec::new();
        for (key, value) in properties {
            let field_name = snake_case(key);
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
        self.current_struct.pop();
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
        if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
            return self.resolve_ref(reference, name_hint);
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
                let item_type = match item.get("$ref").and_then(Value::as_str).and_then(ref_name) {
                    Some(name) if self.resolving.contains(name) => self
                        .current_struct
                        .last()
                        .cloned()
                        .unwrap_or_else(|| "any".to_string()),
                    _ => self.type_for(&item, &format!("{name_hint}Item")),
                };
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

    fn resolve_ref(&mut self, reference: &str, name_hint: &str) -> String {
        let Some(name) = ref_name(reference) else {
            return "any".to_string();
        };
        if !self.resolving.contains(name) {
            if let Some(cached) = self.resolved_names.get(name) {
                return cached.clone();
            }
        }
        let Some(target) = self.defs.get(name).cloned() else {
            return "any".to_string();
        };
        if !self.resolving.insert(name.to_string()) {
            return "any".to_string();
        }
        let resolved = self.type_for(&target, name_hint);
        self.resolving.remove(name);
        if resolved != "any" {
            self.resolved_names
                .insert(name.to_string(), resolved.clone());
        }
        resolved
    }
}
