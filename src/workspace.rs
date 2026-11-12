use std::collections::HashSet;
use std::path::{Path, PathBuf};

use tempfile::TempDir;

use crate::error::{Error, Result};
use crate::idl::GeneratedServer;

pub struct DocsDir {
    pub path: PathBuf,
    pub guard: Option<TempDir>,
}

pub struct KeptServer {
    pub name: String,
    pub idl_files: Vec<String>,
    pub agents_md: bool,
}

pub fn prepare(docs_dir: Option<&Path>) -> Result<DocsDir> {
    let workspace_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    prepare_in(&workspace_root, docs_dir)
}

fn prepare_in(workspace_root: &Path, docs_dir: Option<&Path>) -> Result<DocsDir> {
    match docs_dir {
        Some(dir) => {
            std::fs::create_dir_all(dir)
                .map_err(|err| Error::Workspace(format!("create {}: {err}", dir.display())))?;
            let path = dir
                .canonicalize()
                .map_err(|err| Error::Workspace(format!("resolve {}: {err}", dir.display())))?;
            Ok(DocsDir { path, guard: None })
        }
        None => {
            let cache = workspace_root.join(".cache");
            std::fs::create_dir_all(&cache)
                .map_err(|err| Error::Workspace(format!("create {}: {err}", cache.display())))?;
            cleanup_stale_runs(&cache);
            let temp = tempfile::Builder::new()
                .prefix("codemode-")
                .tempdir_in(&cache)
                .map_err(|err| {
                    Error::Workspace(format!(
                        "create codemode run dir in {}: {err}",
                        cache.display()
                    ))
                })?;
            let path = temp.path().canonicalize().map_err(|err| {
                Error::Workspace(format!("resolve {}: {err}", temp.path().display()))
            })?;
            Ok(DocsDir {
                path,
                guard: Some(temp),
            })
        }
    }
}

fn cleanup_stale_runs(cache: &Path) {
    let Ok(entries) = std::fs::read_dir(cache) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let is_stale_run = entry
            .file_type()
            .map(|file_type| file_type.is_dir())
            .unwrap_or(false)
            && entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with("codemode-"));
        if is_stale_run {
            let _ = std::fs::remove_dir_all(&path);
        }
    }
}

pub fn write_server(root: &Path, generated: &GeneratedServer) -> Result<PathBuf> {
    let server_dir = root.join(&generated.catalog.name);
    std::fs::create_dir_all(&server_dir)
        .map_err(|err| Error::Workspace(format!("create {}: {err}", server_dir.display())))?;
    let agents_path = server_dir.join("agents.md");
    if generated.agents_md.trim().is_empty() {
        let _ = std::fs::remove_file(&agents_path);
    } else {
        write_atomic(&agents_path, &generated.agents_md)?;
    }
    for namespace in &generated.namespaces {
        write_atomic(
            &server_dir.join(format!("{}.idl", namespace.name)),
            &namespace.idl,
        )?;
    }
    let internal = root.join(".internal");
    std::fs::create_dir_all(&internal)
        .map_err(|err| Error::Workspace(format!("create {}: {err}", internal.display())))?;
    write_atomic(
        &internal.join(format!("{}.bundle.idl", generated.catalog.name)),
        &generated.bundle,
    )?;
    Ok(server_dir)
}

pub fn cleanup_stale(root: &Path, kept: &[KeptServer]) -> Result<()> {
    for server in kept {
        let dir = root.join(&server.name);
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        let idl_files: HashSet<&str> = server.idl_files.iter().map(String::as_str).collect();
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if name.ends_with(".idl") && !idl_files.contains(name) {
                let _ = std::fs::remove_file(&path);
            }
            if name == "agents.md" && !server.agents_md {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
    let internal = root.join(".internal");
    if let Ok(entries) = std::fs::read_dir(&internal) {
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if name.ends_with(".bundle.idl") {
                let current = format!("{}.bundle.idl", server_name_from_bundle(name));
                if !kept
                    .iter()
                    .any(|server| format!("{}.bundle.idl", server.name) == current)
                {
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
    }
    Ok(())
}

fn server_name_from_bundle(name: &str) -> &str {
    name.strip_suffix(".bundle.idl").unwrap_or(name)
}

fn write_atomic(path: &Path, content: &str) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::Workspace(format!("{} has no parent", path.display())))?;
    let temp = parent.join(format!(
        ".{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("xidl-codemode")
    ));
    std::fs::write(&temp, content)
        .map_err(|err| Error::Workspace(format!("write {}: {err}", temp.display())))?;
    std::fs::rename(&temp, path)
        .map_err(|err| Error::Workspace(format!("rename {}: {err}", path.display())))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o444));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temporary_docs_use_codemode_prefix_and_preserve_cache() {
        let root = tempfile::tempdir().unwrap();
        let cache = root.path().join(".cache");
        std::fs::create_dir_all(cache.join("keepme")).unwrap();
        std::fs::write(cache.join("keepme/data"), "keep").unwrap();

        let docs = prepare_in(root.path(), None).unwrap();
        assert!(docs.path.starts_with(&cache));
        assert!(docs
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("codemode-")));
        assert!(cache.join("keepme/data").exists());
        drop(docs);
        assert!(cache.join("keepme/data").exists());
    }

    #[test]
    fn stale_cleanup_only_removes_codemode_runs() {
        let root = tempfile::tempdir().unwrap();
        let cache = root.path().join(".cache");
        std::fs::create_dir_all(cache.join("codemode-stale")).unwrap();
        std::fs::create_dir_all(cache.join("keepme")).unwrap();
        std::fs::write(cache.join("codemode-stale/data"), "stale").unwrap();
        std::fs::write(cache.join("keepme/data"), "keep").unwrap();

        let docs = prepare_in(root.path(), None).unwrap();
        assert!(!cache.join("codemode-stale").exists());
        assert!(cache.join("keepme/data").exists());
        assert!(docs.path.exists());
    }
}
