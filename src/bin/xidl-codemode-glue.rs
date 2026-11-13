use xidl_codemode::glue_codegen::GluePlugin;
use xidlc::jsonrpc::serve_stdio;

fn main() -> std::io::Result<()> {
    serve_stdio(GluePlugin)
}
