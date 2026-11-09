use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use rmcp::ServiceExt;
use tracing_subscriber::EnvFilter;
use xidl_codemode::server::CodemodeServer;
use xidl_codemode::{build, default_workspace, BuildOptions};

#[derive(Debug, Parser)]
#[command(
    name = "xidl-codemode",
    version,
    about = "Code-mode MCP gateway driven by xidl"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Start the stdio MCP server.
    Serve {
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long)]
        workspace: Option<PathBuf>,
    },
    /// Generate workspace docs and manifests, then exit.
    Generate {
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long)]
        workspace: Option<PathBuf>,
    },
    /// Discover servers/tools and print a health report.
    Doctor {
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long)]
        workspace: Option<PathBuf>,
    },
    /// Print a markdown bug report for the current configuration.
    BugReport {
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long)]
        workspace: Option<PathBuf>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .without_time()
        .init();
    let cli = Cli::parse();
    match cli.command {
        Command::Serve { config, workspace } => {
            let app = build(&options(config, workspace))?;
            for summary in &app.summaries {
                match &summary.error {
                    Some(error) => tracing::warn!("{error}"),
                    None => tracing::info!("{}: {} tools", summary.name, summary.tools),
                }
            }
            let server = CodemodeServer::new(app.state);
            let running = server.serve(rmcp::transport::stdio()).await?;
            running.waiting().await?;
        }
        Command::Generate { config, workspace } => {
            let app = build(&options(config, workspace))?;
            println!("workspace: {}", app.workspace.display());
            for summary in &app.summaries {
                match &summary.error {
                    Some(error) => println!("{}: error: {error}", summary.name),
                    None => println!("{}: {} tools", summary.name, summary.tools),
                }
            }
        }
        Command::Doctor { config, workspace } => {
            let app = build(&options(config, workspace))?;
            println!("workspace: {}", app.workspace.display());
            for summary in &app.summaries {
                match &summary.error {
                    Some(error) => println!("{}: ERROR: {error}", summary.name),
                    None => println!("{}: ok, {} tools", summary.name, summary.tools),
                }
            }
            if !app.state.errors.is_empty() {
                println!("\n{} server(s) failed", app.state.errors.len());
            }
        }
        Command::BugReport { config, workspace } => {
            let app = build(&options(config, workspace))?;
            println!("# xidl-codemode bug report\n");
            println!("- xidl-codemode: {}", env!("CARGO_PKG_VERSION"));
            println!("- workspace: `{}`", app.workspace.display());
            println!("- servers: {}\n", app.summaries.len());
            println!("## Errors\n");
            if app.state.errors.is_empty() {
                println!("No discovery or manifest errors.");
            } else {
                for error in &app.state.errors {
                    println!("- `{error}`");
                }
            }
            println!("\n## Reproduction\n");
            println!("```bash");
            println!("xidl-codemode doctor --config <config>");
            println!("xidl-codemode generate --config <config>");
            println!("xidlc gen --out-dir /tmp/out codemode-manifest <bundle.idl>");
            println!("```");
        }
    }
    Ok(())
}

fn options(config: Option<PathBuf>, workspace: Option<PathBuf>) -> BuildOptions {
    BuildOptions {
        config,
        workspace: workspace.unwrap_or_else(default_workspace),
    }
}
