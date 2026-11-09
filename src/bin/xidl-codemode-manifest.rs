use std::collections::BTreeMap;

use xidl_codemode::manifest_types::{Manifest, ManifestNamespace, ManifestParam, ManifestTool};
use xidl_parser::hir::{
    Annotation, AnnotationParams, ConstExpr, Definition, Export, InterfaceDclInner, Literal, OpDcl,
    OpTypeSpec, ParserProperties, Specification, TypeSpec,
};
use xidlc::jsonrpc::{serve_stdio, Artifact, ArtifactFile, Codegen, CodegenInput, RpcError};

struct ManifestPlugin;

impl Codegen for ManifestPlugin {
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
        let manifest = build_manifest(&spec).map_err(RpcError::new)?;
        let content = serde_json::to_string_pretty(&manifest)
            .map_err(|err| RpcError::new(err.to_string()))?;
        Ok(vec![Artifact::File(ArtifactFile {
            path: "manifest.json".to_string(),
            content,
        })])
    }
}

fn main() -> std::io::Result<()> {
    serve_stdio(ManifestPlugin)
}

fn build_manifest(spec: &Specification) -> Result<Manifest, String> {
    let mut interfaces = Vec::new();
    walk_definitions(&spec.0, &mut interfaces);
    let mut server = None::<String>;
    let mut namespaces: BTreeMap<String, ManifestNamespace> = BTreeMap::new();
    for interface in interfaces {
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
        let namespace = annotation_string(&interface.annotations, "mcp_namespace")
            .unwrap_or_else(|| "default".to_string());
        let entry = namespaces
            .entry(namespace.clone())
            .or_insert_with(|| ManifestNamespace {
                idl: format!("/{interface_server}/{namespace}.idl"),
                interface: interface.name.clone(),
                tools: Vec::new(),
            });
        if entry.interface != interface.name {
            entry.interface.push(',');
            entry.interface.push_str(&interface.name);
        }
        for op in interface.operations {
            entry.tools.push(manifest_tool(op)?);
        }
    }
    let server = server.ok_or_else(|| "bundle contains no @mcp_server interface".to_string())?;
    if namespaces.is_empty() {
        return Err("bundle contains no tools".to_string());
    }
    Ok(Manifest {
        version: 1,
        server,
        namespaces,
    })
}

struct Interface {
    name: String,
    annotations: Vec<Annotation>,
    operations: Vec<OpDcl>,
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

fn manifest_tool(op: OpDcl) -> Result<ManifestTool, String> {
    let js = op.ident.clone();
    let mcp = annotation_string(&op.annotations, "mcp_tool").unwrap_or_else(|| js.clone());
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
                    ManifestParam {
                        name,
                        wire,
                        required: !has_optional(&param.annotations),
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    let output = match &op.ty {
        OpTypeSpec::Void => "Void".to_string(),
        OpTypeSpec::TypeSpec(ty) => type_name(ty),
    };
    Ok(ManifestTool {
        js,
        mcp,
        output,
        params,
        read_only: has_annotation(&op.annotations, "mcp_read_only"),
        destructive: has_annotation(&op.annotations, "mcp_destructive"),
        idempotent: has_annotation(&op.annotations, "mcp_idempotent"),
        open_world: has_annotation(&op.annotations, "mcp_open_world"),
    })
}

fn type_name(ty: &TypeSpec) -> String {
    match ty {
        TypeSpec::ScopedName(name) => name
            .name
            .last()
            .cloned()
            .unwrap_or_else(|| "Any".to_string()),
        TypeSpec::SequenceType(sequence) => format!("sequence<{}>", type_name(&sequence.ty)),
        TypeSpec::MapType(map) => format!("map<{},{}>", type_name(&map.key), type_name(&map.value)),
        TypeSpec::StringType(_) | TypeSpec::WideStringType(_) => "string".to_string(),
        TypeSpec::Boolean => "boolean".to_string(),
        TypeSpec::AnyType => "any".to_string(),
        TypeSpec::IntegerType(_) => "int64".to_string(),
        TypeSpec::FloatingPtType => "double".to_string(),
        _ => "any".to_string(),
    }
}

fn has_annotation(annotations: &[Annotation], name: &str) -> bool {
    annotations
        .iter()
        .any(|annotation| annotation_name(annotation) == Some(name))
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
