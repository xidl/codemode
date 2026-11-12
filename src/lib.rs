pub mod bridge;
pub mod catalog;
pub mod config;
pub mod error;
pub mod glue;
pub mod idl;
pub mod runlog;
pub mod sandbox;
pub mod server;
pub mod workspace;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;

use crate::bridge::UpstreamManager;
use crate::catalog::ServerCatalog;
use crate::error::{Error, Result};
use crate::sandbox::RuntimeServer;
use crate::server::AppState;

#[derive(Debug, Clone)]
pub struct BuildOptions {
    pub config: Option<PathBuf>,
    pub docs_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ServerSummary {
    pub name: String,
    pub tools: usize,
    pub error: Option<String>,
}

pub struct BuiltApp {
    pub state: Arc<AppState>,
    pub docs_dir: PathBuf,
    pub docs_guard: Option<tempfile::TempDir>,
    pub summaries: Vec<ServerSummary>,
}

struct ServerNameIndex {
    names_by_js: BTreeMap<String, Vec<String>>,
}

impl ServerNameIndex {
    fn new<'a>(catalogs: impl IntoIterator<Item = &'a ServerCatalog>) -> Self {
        let mut names_by_js: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for catalog in catalogs {
            names_by_js
                .entry(catalog.js_name.clone())
                .or_default()
                .push(catalog.name.clone());
        }
        Self { names_by_js }
    }

    fn collision_message(&self, catalog: &ServerCatalog) -> Option<String> {
        let names = self.names_by_js.get(&catalog.js_name)?;
        (names.len() > 1).then(|| {
            format!(
                "server namespace collision: {} both map to {:?}",
                names
                    .iter()
                    .map(|name| format!("{name:?}"))
                    .collect::<Vec<_>>()
                    .join(" and "),
                catalog.js_name
            )
        })
    }
}

pub fn build(options: &BuildOptions) -> Result<BuiltApp> {
    ensure_plugin_on_path();
    let loaded = config::load(options.config.as_deref())?;
    let bridge = Arc::new(UpstreamManager::start()?);
    let docs = workspace::prepare(options.docs_dir.as_deref())?;

    let mut discovered = Vec::new();
    for spec in loaded.servers {
        let result = bridge.discover(spec.clone());
        discovered.push((spec, result));
    }

    let server_names = ServerNameIndex::new(
        discovered
            .iter()
            .filter_map(|(_, discovered)| discovered.as_ref().ok()),
    );

    let mut runtime_servers = Vec::new();
    let mut summaries = Vec::new();
    let mut errors = Vec::new();
    let mut connected_servers = Vec::new();
    let mut tool_index = Vec::new();
    let mut kept = Vec::new();

    let run_log_root = prepare_run_log_root(loaded.run_log_dir.as_deref());
    for (spec, discovered) in discovered {
        match discovered {
            Ok(catalog) => {
                if let Some(collision) = server_names.collision_message(&catalog) {
                    let message = format!("{}: {collision}", catalog.name);
                    errors.push(message.clone());
                    summaries.push(ServerSummary {
                        name: catalog.name,
                        tools: catalog.tools.len(),
                        error: Some(message),
                    });
                    continue;
                }
                if let Err(message) = catalog::validate_server_catalog(&catalog) {
                    errors.push(message.clone());
                    summaries.push(ServerSummary {
                        name: catalog.name,
                        tools: catalog.tools.len(),
                        error: Some(message),
                    });
                    continue;
                }
                let generated = idl::generate(&catalog);
                let glue_result = glue::generate(&catalog.name, &generated.bundle)
                    .and_then(|glue| glue::audit(&glue.js, &catalog).map(|_| glue));
                match glue_result {
                    Ok(glue) => {
                        workspace::write_server(&docs.path, &generated)?;
                        kept.push(workspace::KeptServer {
                            name: catalog.name.clone(),
                            idl_files: generated
                                .namespaces
                                .iter()
                                .map(|namespace| format!("{}.idl", namespace.name))
                                .collect(),
                            agents_md: !generated.agents_md.trim().is_empty(),
                        });
                        runtime_servers.push(RuntimeServer {
                            name: catalog.name.clone(),
                            js_name: catalog.js_name.clone(),
                            glue: glue.js,
                        });
                        connected_servers.push(catalog.js_name.clone());
                        tool_index.push(catalog.clone());
                        summaries.push(ServerSummary {
                            name: catalog.name,
                            tools: catalog.tools.len(),
                            error: None,
                        });
                    }
                    Err(err) => {
                        let message = format!("{}: {err}", catalog.name);
                        errors.push(message.clone());
                        summaries.push(ServerSummary {
                            name: catalog.name,
                            tools: catalog.tools.len(),
                            error: Some(message),
                        });
                    }
                }
            }
            Err(err) => {
                let message = format!("{}: {err}", spec.name);
                errors.push(message.clone());
                summaries.push(ServerSummary {
                    name: spec.name,
                    tools: 0,
                    error: Some(message),
                });
            }
        }
    }
    if docs.guard.is_none() {
        workspace::cleanup_stale(&docs.path, &kept)?;
    }

    let instructions = build_instructions(&docs.path, &connected_servers, &errors);
    let docs_dir = docs.path.clone();
    Ok(BuiltApp {
        state: Arc::new(AppState {
            bridge,
            servers: runtime_servers,
            docs_dir: docs_dir.clone(),
            connected_servers,
            instructions,
            tool_index,
            errors,
            execute_timeout_hard_ms: loaded.execute_timeout_hard_ms,
            execute_file_roots: loaded.execute_file_roots,
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            run_log_root,
        }),
        docs_dir,
        docs_guard: docs.guard,
        summaries,
    })
}

#[derive(Debug, Clone, Default)]
pub struct DiagnoseOptions {
    pub config: Option<PathBuf>,
}

pub fn diagnose(options: &DiagnoseOptions) -> Result<Vec<ServerSummary>> {
    ensure_plugin_on_path();
    let loaded = config::load(options.config.as_deref())?;
    let bridge = UpstreamManager::start()?;
    let mut summaries = Vec::new();
    let mut catalogs = Vec::new();
    for spec in loaded.servers {
        match bridge.discover(spec.clone()) {
            Ok(catalog) => {
                catalogs.push(catalog);
            }
            Err(err) => {
                summaries.push(ServerSummary {
                    name: spec.name,
                    tools: 0,
                    error: Some(err.to_string()),
                });
            }
        }
    }
    let server_names = ServerNameIndex::new(&catalogs);
    for catalog in catalogs {
        let summary = if let Some(collision) = server_names.collision_message(&catalog) {
            ServerSummary {
                name: catalog.name.clone(),
                tools: catalog.tools.len(),
                error: Some(collision),
            }
        } else {
            match catalog::validate_server_catalog(&catalog)
                .map_err(Error::Glue)
                .and_then(|()| {
                    let generated = idl::generate(&catalog);
                    glue::generate(&catalog.name, &generated.bundle)
                        .and_then(|glue| glue::audit(&glue.js, &catalog))
                }) {
                Ok(()) => ServerSummary {
                    name: catalog.name.clone(),
                    tools: catalog.tools.len(),
                    error: None,
                },
                Err(err) => ServerSummary {
                    name: catalog.name.clone(),
                    tools: catalog.tools.len(),
                    error: Some(err.to_string()),
                },
            }
        };
        summaries.push(summary);
    }
    Ok(summaries)
}

pub fn ensure_plugin_on_path() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let Some(dir) = exe.parent() else {
        return;
    };
    let plugin = dir.join(format!(
        "xidl-codemode-glue{}",
        std::env::consts::EXE_SUFFIX
    ));
    if plugin.exists() {
        prepend_path(dir);
        return;
    }
    #[cfg(debug_assertions)]
    if let Some(debug_dir) = debug_plugin_dir() {
        prepend_path(&debug_dir);
    }
}

#[cfg(debug_assertions)]
fn debug_plugin_dir() -> Option<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/debug");
    let plugin = dir.join(format!(
        "xidl-codemode-glue{}",
        std::env::consts::EXE_SUFFIX
    ));
    plugin.exists().then_some(dir)
}

fn prepend_path(dir: &Path) {
    let current = std::env::var_os("PATH").unwrap_or_default();
    let mut paths: Vec<PathBuf> = std::env::split_paths(&current).collect();
    if !paths.iter().any(|path| path == dir) {
        paths.insert(0, dir.to_path_buf());
    }
    if let Ok(joined) = std::env::join_paths(paths) {
        std::env::set_var("PATH", joined);
    }
}

pub fn build_instructions(docs_dir: &Path, server_names: &[String], errors: &[String]) -> String {
    let mut error_section = String::new();
    for error in errors {
        error_section.push_str(&format!("- {error}\n"));
    }
    if error_section.is_empty() {
        error_section.push_str("_None._\n");
    }
    let server_names = if server_names.is_empty() {
        "none".to_string()
    } else {
        server_names.join(", ")
    };
    let instructions = include_str!("../docs/instructions.md")
        .replace("{SERVER_NAMES}", &server_names)
        .replace("{DOCS_DIR}", &docs_dir.display().to_string())
        .replace("{ERRORS}", &error_section);
    instructions
}

/// Default run log root is a persistent `.cache/codemode-log-<random>`
/// directory under the workspace; `runLogDir` overrides it. Startup cleanup
/// of stale `codemode-*` runs removes old roots, and each root prunes its own
/// runs during execution.
fn prepare_run_log_root(override_dir: Option<&std::path::Path>) -> Option<PathBuf> {
    let root = match override_dir {
        Some(dir) => dir.to_path_buf(),
        None => {
            let cache = std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(".cache");
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or_default();
            cache.join(format!("codemode-log-{nanos:x}"))
        }
    };
    match std::fs::create_dir_all(&root) {
        Ok(()) => root.canonicalize().ok().or(Some(root)),
        Err(err) => {
            tracing::warn!("run log dir {} unavailable: {err}", root.display());
            None
        }
    }
}
