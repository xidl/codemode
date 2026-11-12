use serde_json::json;
use xidl_codemode::catalog::{ServerCatalog, ToolInfo};
use xidl_codemode::idl;
use xidl_parser::hir::{
    Annotation, Definition, Export, InterfaceDcl, InterfaceDclInner, OpDcl, Specification, TypeSpec,
};

fn find_interface<'a>(spec: &'a Specification, name: &str) -> Option<&'a InterfaceDcl> {
    for definition in &spec.0 {
        if let Definition::InterfaceDcl(interface) = definition {
            if let InterfaceDclInner::InterfaceDef(def) = &interface.decl {
                if def.header.ident == name {
                    return Some(interface);
                }
            }
        }
    }
    None
}

fn find_operation<'a>(interface: &'a InterfaceDcl, name: &str) -> &'a OpDcl {
    let InterfaceDclInner::InterfaceDef(def) = &interface.decl else {
        panic!("interface is a forward declaration");
    };
    def.interface_body
        .as_ref()
        .expect("interface body")
        .0
        .iter()
        .find_map(|export| match export {
            Export::OpDcl(op) if op.ident == name => Some(op),
            _ => None,
        })
        .expect("operation")
}

#[test]
fn generates_namespace_idl_and_agents_md() {
    let catalog = ServerCatalog {
        name: "demo".into(),
        js_name: "demo".into(),
        instructions: Some("Use graph tools carefully. Read the IDL for parameters.".into()),
        namespace_map: Default::default(),
        tools: vec![
            ToolInfo {
                name: "issues.get".into(),
                description: "Get an issue".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "id": { "type": "integer" },
                        "title": { "type": "string" }
                    },
                    "required": ["id"]
                }),
                output_schema: None,
                read_only: true,
                destructive: false,
                idempotent: true,
                open_world: false,
            },
            ToolInfo {
                name: "issues.list".into(),
                description: "List issues".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "labels": { "type": "array", "items": { "type": "string" } }
                    }
                }),
                output_schema: None,
                read_only: true,
                destructive: false,
                idempotent: true,
                open_world: false,
            },
        ],
    };
    let generated = idl::generate(&catalog);
    assert_eq!(generated.namespaces.len(), 1);
    assert_eq!(generated.namespaces[0].name, "issues");
    let idl = &generated.namespaces[0].idl;
    assert!(idl.contains("@mcp_server(\"demo\")"));
    assert!(idl.contains("@mcp_namespace(\"issues\")"));
    assert!(idl.contains("@rename(\"issues.get\")"));
    assert!(!idl.contains("@mcp_tool"));
    let typed = xidl_parser::parser::parser_text(idl).expect("parse generated IDL");
    let hir = Specification::from(typed);
    let interface = find_interface(&hir, "DemoIssuesApi").expect("DemoIssuesApi");
    let get = find_operation(interface, "issues_get");
    let params = get.parameter.as_ref().expect("parameters").0.clone();
    let names: Vec<&str> = params
        .iter()
        .map(|param| param.declarator.0.as_str())
        .collect();
    assert_eq!(names, ["id", "title"]);
    assert!(matches!(
        &params[0].ty,
        TypeSpec::IntegerType(xidl_parser::hir::IntegerType::I64)
    ));
    assert!(!params[0]
        .annotations
        .iter()
        .any(|annotation| matches!(annotation, Annotation::Optional { .. })));
    assert!(matches!(&params[1].ty, TypeSpec::StringType(_)));
    assert!(params[1]
        .annotations
        .iter()
        .any(|annotation| matches!(annotation, Annotation::Optional { .. })));
    let list = find_operation(interface, "issues_list");
    let list_params = list.parameter.as_ref().expect("list parameters").0.clone();
    assert_eq!(list_params[0].declarator.0, "labels");
    assert!(matches!(
        &list_params[0].ty,
        TypeSpec::SequenceType(ref sequence)
            if matches!(sequence.ty.as_ref(), TypeSpec::StringType(_))
    ));
    assert!(list_params[0]
        .annotations
        .iter()
        .any(|annotation| matches!(annotation, Annotation::Optional { .. })));
    assert!(idl.contains("@mcp_read_only"));
    assert!(idl.contains("@mcp_idempotent"));
    assert!(generated
        .agents_md
        .contains("Use graph tools carefully. Read the IDL for parameters."));
    assert!(!generated.agents_md.contains("mcp.demo"));
    assert!(!generated.agents_md.contains("issues_get("));
    assert!(generated.bundle.contains("interface DemoIssuesApi"));
}

#[test]
fn identical_tool_names_do_not_get_rename() {
    let catalog = ServerCatalog {
        name: "demo".into(),
        js_name: "demo".into(),
        instructions: None,
        namespace_map: Default::default(),
        tools: vec![ToolInfo {
            name: "ping".into(),
            description: String::new(),
            input_schema: serde_json::json!({"type": "object"}),
            output_schema: None,
            read_only: false,
            destructive: false,
            idempotent: false,
            open_world: false,
        }],
    };
    let generated = idl::generate(&catalog);
    let idl = &generated.namespaces[0].idl;
    assert!(idl.contains("any ping()"));
    assert!(!idl.contains("@rename"));
    assert!(!idl.contains("@mcp_tool"));
}
