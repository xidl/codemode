use std::collections::HashSet;

use xidl_parser::hir::{
    Annotation, AnnotationParams, ConstExpr, Definition, Export, InterfaceDclInner, Literal, OpDcl,
    ParserProperties, Specification, TypeSpec,
};
use xidlc::jsonrpc::{serve_stdio, Artifact, ArtifactFile, Codegen, CodegenInput, RpcError};

use xidl_codemode::catalog::{pascal_case, snake_case};

struct GluePlugin;

impl Codegen for GluePlugin {
    fn get_engine_version(&self) -> Result<String, RpcError> {
        Ok("*".to_string())
    }

    fn get_properties(&self) -> Result<ParserProperties, RpcError> {
        Ok(ParserProperties::new())
    }

    fn generate(
        &self,
        input: CodegenInput,
        _path: String,
        _props: ParserProperties,
    ) -> Result<Vec<Artifact>, RpcError> {
        let spec = input.into_rpc_hir();
        let glue = build_glue(&spec).map_err(RpcError::new)?;
        Ok(vec![
            Artifact::File(ArtifactFile {
                path: "glue.js".to_string(),
                content: glue.js,
            }),
            Artifact::File(ArtifactFile {
                path: "glue.d.ts".to_string(),
                content: glue.dts,
            }),
        ])
    }
}

fn main() -> std::io::Result<()> {
    serve_stdio(GluePlugin)
}

struct GlueOut {
    js: String,
    dts: String,
}

struct Interface {
    name: String,
    annotations: Vec<Annotation>,
    operations: Vec<OpDcl>,
}

fn build_glue(spec: &Specification) -> Result<GlueOut, String> {
    let mut interfaces = Vec::new();
    walk_definitions(&spec.0, &mut interfaces);
    let mut server = None::<String>;
    let mut js_names = HashSet::new();
    for interface in &interfaces {
        let interface_server = annotation_string(&interface.annotations, "mcp_server")
            .ok_or_else(|| format!("interface {} is missing @mcp_server", interface.name))?;
        match &server {
            Some(existing) if existing != &interface_server => {
                return Err(format!(
                    "bundle contains multiple MCP servers: {existing} and {interface_server}"
                ));
            }
            None => server = Some(interface_server.clone()),
            _ => {}
        }
        for op in &interface.operations {
            if !js_names.insert(op.ident.clone()) {
                return Err(format!(
                    "duplicate JS tool name {:?} across namespaces",
                    op.ident
                ));
            }
        }
    }
    let server = server.ok_or_else(|| "bundle contains no @mcp_server interface".to_string())?;
    if interfaces
        .iter()
        .all(|interface| interface.operations.is_empty())
    {
        return Err("bundle contains no tools".to_string());
    }
    let js = emit_js(&server, &interfaces);
    let dts = emit_dts(&server, &interfaces);
    Ok(GlueOut { js, dts })
}

fn emit_js(server: &str, interfaces: &[Interface]) -> String {
    let js_server = snake_case(server);
    let mut out = String::new();
    out.push_str("function installXidlGlue(target, host) {\n");
    out.push_str("  const __server = Object.create(null);\n");
    out.push_str("  const __ns = Object.create(null);\n");
    for interface in interfaces {
        for op in &interface.operations {
            emit_tool(&mut out, server, op);
        }
    }
    out.push_str("  Object.freeze(__ns);\n");
    out.push_str(&format!("  __server[{}] = __ns;\n", js_string(server)));
    out.push_str(&format!("  __server[{}] = __ns;\n", js_string(&js_server)));
    out.push_str("  Object.freeze(__server);\n");
    out.push_str(
        "  for (const [__key, __value] of Object.entries(__server)) {\n    \
         Object.defineProperty(target, __key, { value: __value, enumerable: true });\n  }\n",
    );
    out.push_str("}\n");
    out
}

fn emit_tool(out: &mut String, server: &str, op: &OpDcl) {
    let js_name = js_string(&op.ident);
    let wire_tool_name = annotation_rename(&op.annotations).unwrap_or_else(|| op.ident.clone());
    let wire_tool_json = js_string(&wire_tool_name);
    let tool_label = format!("{}.{}", escape_js(server), escape_js(&wire_tool_name));
    let params = op
        .parameter
        .as_ref()
        .map(|parameters| {
            parameters
                .0
                .iter()
                .map(|param| {
                    let name = param.declarator.0.clone();
                    let wire =
                        annotation_rename(&param.annotations).unwrap_or_else(|| name.clone());
                    (name, wire, has_optional(&param.annotations))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    out.push_str(&format!("  __ns[{js_name}] = function(input) {{\n"));
    out.push_str(&format!(
        "    if (input !== undefined && (typeof input !== \"object\" || input === null || Array.isArray(input))) {{\n      throw new Error(\"invalid arguments for {tool_label}: expected a single object argument\");\n    }}\n"
    ));
    out.push_str("    const __params = Object.create(null);\n");
    for (name, wire, optional) in &params {
        out.push_str("    {\n");
        out.push_str(&format!(
            "      const __value = input !== undefined && Object.prototype.hasOwnProperty.call(input, {}) ? input[{}] : undefined;\n",
            js_string(name),
            js_string(name)
        ));
        if *optional {
            out.push_str(&format!(
                "      if (__value !== undefined) {{ __params[{}] = __value; }}\n",
                js_string(wire)
            ));
        } else {
            out.push_str(&format!(
                "      if (__value === undefined) {{ throw new Error(\"missing required argument {} for {tool_label}\"); }}\n",
                escape_js(name)
            ));
            out.push_str(&format!("      __params[{}] = __value;\n", js_string(wire)));
        }
        out.push_str("    }\n");
    }
    if params.is_empty() {
        out.push_str(&format!(
            "    for (const __key of Object.keys(input ?? {{}})) {{\n      throw new Error(\"unknown argument \" + __key + \" for {tool_label} (expected no arguments)\");\n    }}\n"
        ));
    } else {
        let known = params
            .iter()
            .map(|(name, _, _)| format!("__key !== {}", js_string(name)))
            .collect::<Vec<_>>()
            .join(" && ");
        let expected = params
            .iter()
            .map(|(name, _, _)| name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        out.push_str(&format!(
            "    for (const __key of Object.keys(input ?? {{}})) {{\n      if ({known}) {{\n        throw new Error(\"unknown argument \" + __key + \" for {tool_label} (expected: {expected})\");\n      }}\n    }}\n"
        ));
    }
    out.push_str(&format!(
        "    return JSON.parse(host.callTool(JSON.stringify({{ server: {server}, tool: {wire_tool_json}, params: __params }})));\n",
        server = js_string(server),
    ));
    out.push_str("  };\n");
}

fn emit_dts(server: &str, interfaces: &[Interface]) -> String {
    let mut out = String::new();
    out.push_str("// Generated by xidl-codemode-glue. Do not edit.\n\n");
    out.push_str("export type CodeModeValue = unknown;\n\n");
    out.push_str("export interface XidlGlueHost {\n  callTool(call: string): string;\n}\n\n");
    out.push_str(
        "export declare function installXidlGlue(target: object, host: XidlGlueHost): void;\n\n",
    );
    for interface in interfaces {
        let namespace = annotation_string(&interface.annotations, "mcp_namespace")
            .unwrap_or_else(|| "default".to_string());
        out.push_str(&format!("export interface {} {{\n", interface.name));
        for op in &interface.operations {
            let wire_tool_name =
                annotation_rename(&op.annotations).unwrap_or_else(|| op.ident.clone());
            if wire_tool_name != op.ident {
                out.push_str(&format!("  /** MCP tool: {wire_tool_name} */\n"));
            }
            let params_interface_name = format!(
                "{}{}{}Params",
                pascal_case(server),
                pascal_case(&namespace),
                pascal_case(&op.ident)
            );
            let fields = op
                .parameter
                .as_ref()
                .map(|parameters| parameters.0.clone())
                .unwrap_or_default();
            out.push_str(&format!("export interface {params_interface_name} {{\n"));
            for param in &fields {
                let optional = param
                    .annotations
                    .iter()
                    .any(|annotation| matches!(annotation, Annotation::Optional { .. }));
                let suffix = if optional { "?" } else { "" };
                out.push_str(&format!(
                    "  {}{}: {};\n",
                    param.declarator.0,
                    suffix,
                    ts_type(&param.ty)
                ));
            }
            out.push_str("}\n\n");
            let argument = if fields.is_empty() {
                format!("params?: {params_interface_name}")
            } else {
                format!("params: {params_interface_name}")
            };
            out.push_str(&format!("  {}({}): CodeModeValue;\n", op.ident, argument));
        }
        out.push_str("}\n\n");
    }
    let _ = server;
    out
}

fn ts_type(ty: &TypeSpec) -> String {
    match ty {
        TypeSpec::ScopedName(name) => match name.name.last().map(String::as_str) {
            Some("string") => "string".to_string(),
            Some("boolean") => "boolean".to_string(),
            _ => "unknown".to_string(),
        },
        TypeSpec::SequenceType(sequence) => {
            format!("Array<{}>", ts_type(&sequence.ty))
        }
        TypeSpec::MapType(map) => {
            format!("Record<string, {}>", ts_type(&map.value))
        }
        TypeSpec::StringType(_) | TypeSpec::WideStringType(_) => "string".to_string(),
        TypeSpec::Boolean => "boolean".to_string(),
        TypeSpec::AnyType => "unknown".to_string(),
        TypeSpec::IntegerType(_) => "number".to_string(),
        TypeSpec::FloatingPtType => "number".to_string(),
        _ => "unknown".to_string(),
    }
}

fn js_string(value: &str) -> String {
    format!("\"{}\"", escape_js(value))
}

fn escape_js(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn walk_definitions(definitions: &[Definition], out: &mut Vec<Interface>) {
    for definition in definitions {
        match definition {
            Definition::InterfaceDcl(interface) => {
                if let InterfaceDclInner::InterfaceDef(def) = &interface.decl {
                    if let Some(body) = &def.interface_body {
                        let operations = body
                            .0
                            .iter()
                            .filter_map(|export| match export {
                                Export::OpDcl(op) => Some(op.clone()),
                                _ => None,
                            })
                            .collect();
                        out.push(Interface {
                            name: def.header.ident.clone(),
                            annotations: interface.annotations.clone(),
                            operations,
                        });
                    }
                }
            }
            Definition::ModuleDcl(module) => walk_definitions(&module.definition, out),
            _ => {}
        }
    }
}

fn has_optional(annotations: &[Annotation]) -> bool {
    annotations
        .iter()
        .any(|annotation| matches!(annotation, Annotation::Optional { .. }))
}

fn annotation_rename(annotations: &[Annotation]) -> Option<String> {
    annotations.iter().find_map(|annotation| match annotation {
        Annotation::Rename { name } => Some(name.clone()),
        _ => None,
    })
}

fn annotation_string(annotations: &[Annotation], name: &str) -> Option<String> {
    annotations
        .iter()
        .find(|annotation| annotation_name(annotation) == Some(name))
        .and_then(annotation_string_value)
}

fn annotation_name(annotation: &Annotation) -> Option<&str> {
    match annotation {
        Annotation::Builtin { name, .. } => Some(name.as_str()),
        Annotation::ScopedName { name, .. } => name.name.last().map(String::as_str),
        _ => None,
    }
}

fn annotation_string_value(annotation: &Annotation) -> Option<String> {
    let params = match annotation {
        Annotation::Builtin { params, .. } | Annotation::ScopedName { params, .. } => {
            params.as_ref()
        }
        _ => return None,
    }?;
    match params {
        AnnotationParams::ConstExpr(expr) => const_string(expr),
        AnnotationParams::Positional(values) => values.first().and_then(const_string),
        AnnotationParams::Params(values) => values
            .first()
            .and_then(|param| param.value.as_ref())
            .and_then(const_string),
        AnnotationParams::Raw(raw) => {
            Some(raw.trim().trim_matches('"').trim_matches('\'').to_string())
        }
    }
}

fn const_string(expr: &ConstExpr) -> Option<String> {
    let value = match expr {
        ConstExpr::Literal(Literal::StringLiteral(value))
        | ConstExpr::Literal(Literal::WideStringLiteral(value)) => value.clone(),
        ConstExpr::Literal(Literal::CharLiteral(value))
        | ConstExpr::Literal(Literal::WideCharacterLiteral(value)) => value.clone(),
        _ => return None,
    };
    Some(
        value
            .trim()
            .trim_matches('"')
            .trim_matches('\'')
            .to_string(),
    )
}
