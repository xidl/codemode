use xidl_parser::hir::{
    Annotation, AnnotationParams, ConstExpr, ConstrTypeDcl, Declarator, Definition, Export,
    InterfaceDclInner, Literal, OpDcl, StructDcl, TypeDcl, TypeSpec,
};

pub(super) struct Interface {
    pub(super) name: String,
    pub(super) annotations: Vec<Annotation>,
    pub(super) operations: Vec<OpDcl>,
}

pub(super) struct StructOut {
    pub(super) name: String,
    pub(super) fields: Vec<StructField>,
}

pub(super) struct StructField {
    pub(super) name: String,
    pub(super) ty: TypeSpec,
    pub(super) optional: bool,
}

pub(super) fn walk_definitions(
    definitions: &[Definition],
    out: &mut Vec<Interface>,
    structs: &mut Vec<StructOut>,
) {
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
            Definition::ModuleDcl(module) => walk_definitions(&module.definition, out, structs),
            Definition::ConstrTypeDcl(ConstrTypeDcl::StructDcl(decl)) => {
                structs.push(struct_out(decl));
            }
            Definition::TypeDcl(TypeDcl::ConstrTypeDcl(ConstrTypeDcl::StructDcl(decl))) => {
                structs.push(struct_out(decl));
            }
            _ => {}
        }
    }
}

fn struct_out(decl: &StructDcl) -> StructOut {
    let fields = decl
        .member
        .iter()
        .filter_map(|member| {
            let name = match member.ident.first()? {
                Declarator::SimpleDeclarator(simple) => simple.0.clone(),
                Declarator::ArrayDeclarator(array) => array.ident.clone(),
            };
            Some(StructField {
                name,
                ty: member.ty.clone(),
                optional: has_optional(&member.annotations),
            })
        })
        .collect();
    StructOut {
        name: decl.ident.clone(),
        fields,
    }
}

pub(super) fn has_optional(annotations: &[Annotation]) -> bool {
    annotations
        .iter()
        .any(|annotation| matches!(annotation, Annotation::Optional { .. }))
}

pub(super) fn annotation_rename(annotations: &[Annotation]) -> Option<String> {
    annotations.iter().find_map(|annotation| match annotation {
        Annotation::Rename { name } => Some(name.clone()),
        _ => None,
    })
}

pub(super) fn annotation_string(annotations: &[Annotation], name: &str) -> Option<String> {
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
