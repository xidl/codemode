use xidl_parser::hir::ParserProperties;
use xidlc::jsonrpc::{Artifact, ArtifactFile, Codegen, CodegenInput, RpcError};

mod dts;
mod hir;
mod js;

use dts::emit_dts;
use hir::walk_definitions;
use js::emit_js;

pub struct GluePlugin;

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

pub struct GlueOut {
    pub js: String,
    pub dts: String,
}

pub fn build_glue(spec: &xidl_parser::hir::Specification) -> Result<GlueOut, String> {
    let mut interfaces = Vec::new();
    let mut structs = Vec::new();
    walk_definitions(&spec.0, &mut interfaces, &mut structs);
    let mut server = None::<String>;
    let mut js_names = std::collections::HashSet::new();
    for interface in &interfaces {
        let interface_server = hir::annotation_string(&interface.annotations, "mcp_server")
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
    let dts = emit_dts(&server, &interfaces, &structs);
    Ok(GlueOut { js, dts })
}
