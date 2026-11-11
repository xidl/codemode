use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use rmcp::ServiceExt;
use tracing_subscriber::EnvFilter;
use xidl_codemode::server::CodemodeServer;
use xidl_codemode::{build, default_workspace, diagnose, BuildOptions, DiagnoseOptions};

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
        /// Persistent docs directory. By default docs live in a temporary
        /// `<workspace>/.cache/<run-id>/` directory that is removed on exit.
        #[arg(long)]
        docs_dir: Option<PathBuf>,
    },
    /// Export persistent workspace docs, then exit.
    Generate {
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long)]
        workspace: Option<PathBuf>,
        #[arg(long)]
        docs_dir: PathBuf,
    },
    /// Discover servers/tools and print a read-only health report.
    Doctor {
        #[arg(long)]
        config: Option<PathBuf>,
        /// Print the health report as JSON.
        #[arg(long)]
        json: bool,
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
        Command::Serve {
            config,
            workspace,
            docs_dir,
        } => {
            let app = build(&BuildOptions {
                config,
                workspace: workspace.unwrap_or_else(default_workspace),
                docs_dir,
            })?;
            for summary in &app.summaries {
                match &summary.error {
                    Some(error) => tracing::warn!("{error}"),
                    None => tracing::info!("{}: {} tools", summary.name, summary.tools),
                }
            }
            tracing::info!("docs directory: {}", app.docs_dir.display());
            let server = CodemodeServer::new(app.state);
            let running = server.serve(rmcp::transport::stdio()).await?;
            running.waiting().await?;
        }
        Command::Generate {
            config,
            workspace,
            docs_dir,
        } => {
            let app = build(&BuildOptions {
                config,
                workspace: workspace.unwrap_or_else(default_workspace),
                docs_dir: Some(docs_dir),
            })?;
            println!("docs directory: {}", app.docs_dir.display());
            for summary in &app.summaries {
                match &summary.error {
                    Some(error) => println!("{}: error: {error}", summary.name),
                    None => println!("{}: {} tools", summary.name, summary.tools),
                }
            }
        }
        Command::Doctor { config, json } => {
            let summaries = diagnose(&DiagnoseOptions { config })?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&summaries).expect("summaries serialize as JSON")
                );
                return Ok(());
            }
            for summary in &summaries {
                match &summary.error {
                    Some(error) => println!("{}: ERROR: {error}", summary.name),
                    None => println!("{}: ok, {} tools", summary.name, summary.tools),
                }
            }
            let failed = summaries
                .iter()
                .filter(|summary| summary.error.is_some())
                .count();
            if failed > 0 {
                println!("\n{failed} server(s) failed");
            }
        }
    }
    Ok(())
}
