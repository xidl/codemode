use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

const MAX_RUNS: usize = 20;
const MAX_TOTAL_BYTES: u64 = 100 * 1024 * 1024;
const PREVIEW_BYTES: usize = 2048;

fn unix_perms(path: &Path, mode: u32) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        let _ = mode;
    }
}

fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

/// Keys whose values must never land in run logs verbatim.
fn is_sensitive_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    ["authorization", "token", "secret", "api_key", "password"]
        .iter()
        .any(|needle| key.contains(needle))
}

fn redact(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, value)| {
                    if is_sensitive_key(key) {
                        (key.clone(), Value::String("[REDACTED]".into()))
                    } else {
                        (key.clone(), redact(value))
                    }
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(redact).collect()),
        other => other.clone(),
    }
}

pub fn preview(value: &Value) -> Value {
    let redacted = redact(value);
    let text = serde_json::to_string(&redacted).unwrap_or_else(|_| "null".into());
    if text.len() <= PREVIEW_BYTES {
        return redacted;
    }
    let cut: String = text.chars().take(PREVIEW_BYTES).collect();
    Value::String(format!("{cut}…[truncated]"))
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct RunPaths {
    pub id: String,
    pub dir: String,
    pub metrics: String,
    pub log: String,
}

pub struct RunLogger {
    paths: RunPaths,
    log_file: Mutex<Option<std::fs::File>>,
    metrics_file: Mutex<Option<std::fs::File>>,
}

impl RunLogger {
    /// Disabled logger: every write is a no-op and `paths()` stays `None`.
    pub fn disabled() -> Self {
        Self {
            paths: RunPaths {
                id: String::new(),
                dir: String::new(),
                metrics: String::new(),
                log: String::new(),
            },
            log_file: Mutex::new(None),
            metrics_file: Mutex::new(None),
        }
    }

    /// Create `<root>/<run-id>/` with run.json plus append-only JSONL files.
    /// Failures degrade to a disabled logger; logging must never break
    /// execution.
    pub fn create(root: &Path, run_json: &Value) -> Self {
        let disabled = Self::disabled();
        if std::fs::create_dir_all(root).is_err() {
            return disabled;
        }
        let root = root.to_path_buf();
        unix_perms(&root, 0o700);
        let id = run_id();
        let run_dir = root.join(&id);
        if std::fs::create_dir_all(&run_dir).is_err() {
            return disabled;
        }
        unix_perms(&run_dir, 0o700);
        let log_path = run_dir.join("log.jsonl");
        let metrics_path = run_dir.join("metrics.jsonl");
        let log_file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .ok();
        let metrics_file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&metrics_path)
            .ok();
        unix_perms(&log_path, 0o600);
        unix_perms(&metrics_path, 0o600);
        let mut run_json = run_json.clone();
        if let Some(map) = run_json.as_object_mut() {
            map.insert("run_id".into(), Value::String(id.clone()));
            map.insert("created_at_ms".into(), Value::from(now_millis() as u64));
        }
        write_json_atomic(&run_dir.join("run.json"), &run_json);
        let logger = Self {
            paths: RunPaths {
                id,
                dir: run_dir.display().to_string(),
                metrics: metrics_path.display().to_string(),
                log: log_path.display().to_string(),
            },
            log_file: Mutex::new(log_file),
            metrics_file: Mutex::new(metrics_file),
        };
        logger.prune(&root);
        logger
    }

    pub fn paths(&self) -> Option<&RunPaths> {
        if self.paths.id.is_empty() {
            None
        } else {
            Some(&self.paths)
        }
    }

    pub fn log_event(&self, mut event: Value) {
        if let Some(map) = event.as_object_mut() {
            map.insert("ts_ms".into(), Value::from(now_millis() as u64));
        }
        self.write_line(&self.log_file, &event);
    }

    pub fn log_metric(&self, mut metric: Value) {
        if let Some(map) = metric.as_object_mut() {
            map.insert("ts_ms".into(), Value::from(now_millis() as u64));
        }
        self.write_line(&self.metrics_file, &metric);
    }

    pub fn write_summary(&self, summary: &Value) {
        if self.paths.id.is_empty() {
            return;
        }
        write_json_atomic(&Path::new(&self.paths.dir).join("summary.json"), summary);
    }

    fn write_line(&self, file: &Mutex<Option<std::fs::File>>, value: &Value) {
        let mut guard = match file.lock() {
            Ok(guard) => guard,
            Err(_) => return,
        };
        let Some(file) = guard.as_mut() else {
            return;
        };
        let line = serde_json::to_string(value).unwrap_or_else(|_| "{}".into());
        let _ = writeln!(file, "{line}");
        let _ = file.flush();
    }

    /// Keep at most `MAX_RUNS` or `MAX_TOTAL_BYTES`, deleting the oldest run
    /// directories first. The current run is never deleted.
    fn prune(&self, root: &Path) {
        let Ok(entries) = std::fs::read_dir(root) else {
            return;
        };
        let mut runs: Vec<(PathBuf, u64)> = entries
            .flatten()
            .filter(|entry| entry.path().is_dir() && entry.path() != Path::new(&self.paths.dir))
            .map(|entry| {
                let size = dir_size(&entry.path());
                (entry.path(), size)
            })
            .collect();
        runs.sort_by(|a, b| a.0.file_name().cmp(&b.0.file_name()));
        let current_size = dir_size(Path::new(&self.paths.dir));
        let mut total = current_size + runs.iter().map(|(_, size)| size).sum::<u64>();
        while runs.len() >= MAX_RUNS || total > MAX_TOTAL_BYTES {
            let Some((oldest, size)) = runs.first().cloned() else {
                break;
            };
            let _ = std::fs::remove_dir_all(&oldest);
            total = total.saturating_sub(size);
            runs.remove(0);
        }
    }
}

fn run_id() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let seconds = now.as_secs();
    let millis = now.subsec_millis();
    let days = seconds / 86_400;
    let rem = seconds % 86_400;
    let (year, month, day) = civil_from_days(days as i64);
    let hour = rem / 3600;
    let minute = (rem % 3600) / 60;
    let second = rem % 60;
    let nanos_tail = now.subsec_nanos() ^ (millis << 16);
    format!("{year:04}{month:02}{day:02}T{hour:02}{minute:02}{second:02}Z-{nanos_tail:06x}")
}

/// Convert days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

fn dir_size(path: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| {
            if entry.path().is_dir() {
                dir_size(&entry.path())
            } else {
                entry.metadata().map(|meta| meta.len()).unwrap_or(0)
            }
        })
        .sum()
}

fn write_json_atomic(path: &Path, value: &Value) {
    let temp = path.with_extension("tmp");
    if std::fs::write(
        &temp,
        serde_json::to_string_pretty(value).unwrap_or_default(),
    )
    .is_ok()
    {
        unix_perms(&temp, 0o600);
        let _ = std::fs::rename(&temp, path);
    }
}
