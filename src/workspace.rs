use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::idl::GeneratedServer;

pub fn default_root() -> PathBuf {
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".codemode")
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
