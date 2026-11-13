use xidl_parser::hir::OpDcl;

use crate::catalog::snake_case;

use super::hir::{annotation_rename, has_optional, Interface};

pub(super) fn emit_js(server: &str, interfaces: &[Interface]) -> String {
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
