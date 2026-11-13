use std::io::Read;
use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecuteArgs {
    /// Source mode: inline `code` (default) or a `path` on disk.
    #[serde(default)]
    pub r#type: Option<String>,
    /// JavaScript source. Required when `type` is `code` (the default).
    pub code: Option<String>,
    /// Path to a JavaScript file. Required when `type` is `file`.
    pub path: Option<String>,
    /// Execution timeout in milliseconds (default 10000, clamped to the
    /// configured hard limit).
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchToolsArgs {
    pub query: Option<String>,
    pub server: Option<String>,
    pub namespace: Option<String>,
    #[serde(default = "default_search_limit")]
    pub limit: usize,
}

fn default_search_limit() -> usize {
    8
}

#[derive(Debug)]
pub struct FileReadError {
    pub code: &'static str,
    pub message: String,
}

impl std::fmt::Display for FileReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// Validate the code/file split. Schema-shape problems return `Err` and map
/// to MCP `invalid_params`; IO problems come back from `read_execute_source`
/// and surface as `ExecuteOutcome` errors instead.
pub fn validate_execute_args(args: &ExecuteArgs) -> Result<&'static str, String> {
    match args.r#type.as_deref() {
        None | Some("code") => {
            if args.path.is_some() {
                return Err("`path` is only valid when type is \"file\"".into());
            }
            match &args.code {
                Some(code) if code.is_empty() => Err("`code` must not be empty".into()),
                Some(_) => Ok("code"),
                None => Err("`code` is required when type is \"code\"".into()),
            }
        }
        Some("file") => {
            if args.code.is_some() {
                return Err("`code` is only valid for type \"code\"; use `path` instead".into());
            }
            match &args.path {
                Some(path) if path.is_empty() => Err("`path` must not be empty".into()),
                Some(_) => Ok("file"),
                None => Err("`path` is required when type is \"file\"".into()),
            }
        }
        Some(other) => Err(format!(
            "invalid type {:?}: expected \"code\" or \"file\"",
            other
        )),
    }
}

fn path_within(path: &Path, root: &Path) -> bool {
    let normalize = |value: &Path| -> String {
        let mut text = value
            .to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "/");
        if cfg!(windows) {
            text = text.to_ascii_lowercase();
        }
        text.trim_end_matches('/').to_string()
    };
    let file = normalize(path);
    let root = normalize(root);
    root.is_empty() || file == root || file.starts_with(&format!("{root}/"))
}

/// Read the file-mode source. Relative paths resolve against `cwd`; the file
/// must live inside one of `roots` after canonicalization (symlink escapes
/// are rejected), be a regular file, and decode as UTF-8. A leading BOM and
/// shebang line are stripped while preserving line numbers.
pub fn read_execute_source(
    raw: &str,
    roots: &[PathBuf],
    cwd: &Path,
    max_bytes: usize,
) -> std::result::Result<String, FileReadError> {
    if roots.is_empty() {
        return Err(FileReadError {
            code: "file_forbidden",
            message: "file mode is disabled: executeFileRoots is empty".into(),
        });
    }
    let path = Path::new(raw);
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    let canonical = absolute.canonicalize().map_err(|err| {
        let code = if err.kind() == std::io::ErrorKind::NotFound {
            "file_not_found"
        } else if err.kind() == std::io::ErrorKind::PermissionDenied {
            "file_forbidden"
        } else {
            "file_read_error"
        };
        FileReadError {
            code,
            message: format!("cannot resolve {}: {err}", absolute.display()),
        }
    })?;
    if !roots.iter().any(|root| {
        root.canonicalize()
            .map(|canonical_root| path_within(&canonical, &canonical_root))
            .unwrap_or(false)
    }) {
        return Err(FileReadError {
            code: "file_forbidden",
            message: format!(
                "{} is outside the allowed execute roots",
                canonical.display()
            ),
        });
    }
    let metadata = std::fs::metadata(&canonical).map_err(|err| FileReadError {
        code: "file_read_error",
        message: format!("cannot stat {}: {err}", canonical.display()),
    })?;
    if !metadata.is_file() {
        return Err(FileReadError {
            code: "file_not_regular",
            message: format!("{} is not a regular file", canonical.display()),
        });
    }
    if metadata.len() > max_bytes as u64 {
        return Err(FileReadError {
            code: "file_too_large",
            message: format!(
                "{} is {} bytes; limit is {max_bytes}",
                canonical.display(),
                metadata.len()
            ),
        });
    }
    let file = std::fs::File::open(&canonical).map_err(|err| FileReadError {
        code: "file_read_error",
        message: format!("cannot open {}: {err}", canonical.display()),
    })?;
    let mut bytes = Vec::new();
    std::io::Read::take(file, max_bytes as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|err| FileReadError {
            code: "file_read_error",
            message: format!("cannot read {}: {err}", canonical.display()),
        })?;
    if bytes.len() > max_bytes {
        return Err(FileReadError {
            code: "file_too_large",
            message: format!(
                "{} exceeded the {max_bytes} byte limit while reading",
                canonical.display()
            ),
        });
    }
    let mut text = String::from_utf8(bytes).map_err(|_| FileReadError {
        code: "file_invalid_utf8",
        message: format!("{} is not valid UTF-8", canonical.display()),
    })?;
    if let Some(rest) = text.strip_prefix('\u{feff}') {
        text = rest.to_string();
    }
    if text.starts_with("#!") {
        if let Some(line_end) = text.find('\n') {
            text = text[line_end..].to_string();
        } else {
            text = String::new();
        }
    }
    Ok(text)
}
