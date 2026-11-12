use std::path::PathBuf;

use xidl_codemode::server::{read_execute_source, validate_execute_args, ExecuteArgs};

fn args_from(json: serde_json::Value) -> ExecuteArgs {
    serde_json::from_value(json).expect("parse execute args")
}

#[test]
fn code_mode_is_the_default() {
    let args = args_from(serde_json::json!({"code": "return 1;"}));
    assert_eq!(validate_execute_args(&args).unwrap(), "code");
    let args = args_from(serde_json::json!({"type": "code", "code": "return 1;"}));
    assert_eq!(validate_execute_args(&args).unwrap(), "code");
}

#[test]
fn validates_type_and_field_combinations() {
    let cases: Vec<(serde_json::Value, bool)> = vec![
        (serde_json::json!({"type": "code", "path": "x.js"}), false),
        (serde_json::json!({"code": ""}), false),
        (serde_json::json!({}), false),
        (serde_json::json!({"type": "file"}), false),
        (serde_json::json!({"type": "file", "code": "x"}), false),
        (serde_json::json!({"type": "file", "path": ""}), false),
        (serde_json::json!({"type": "repl", "code": "x"}), false),
        (serde_json::json!({"type": "file", "path": "x.js"}), true),
    ];
    for (value, expected_ok) in cases {
        let args = args_from(value.clone());
        assert_eq!(validate_execute_args(&args).is_ok(), expected_ok, "{value}");
    }
}

fn setup_dir() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

fn write(dir: &std::path::Path, name: &str, content: &[u8]) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, content).unwrap();
    path
}

#[test]
fn reads_relative_and_absolute_files_inside_root() {
    let dir = setup_dir();
    let path = write(dir.path(), "script.js", b"return 42;");
    let roots = vec![dir.path().to_path_buf()];
    let source = read_execute_source("script.js", &roots, dir.path(), 256).unwrap();
    assert_eq!(source, "return 42;");
    let source = read_execute_source(
        path.to_str().unwrap(),
        &roots,
        std::path::Path::new("/"),
        256,
    )
    .unwrap();
    assert_eq!(source, "return 42;");
}

#[test]
fn rejects_paths_outside_roots() {
    let dir = setup_dir();
    let other = setup_dir();
    let outside = write(other.path(), "script.js", b"return 1;");
    let roots = vec![dir.path().to_path_buf()];
    let err = read_execute_source(outside.to_str().unwrap(), &roots, dir.path(), 256).unwrap_err();
    assert_eq!(err.code, "file_forbidden");
}

#[test]
fn rejects_symlink_escapes_and_allows_internal_links() {
    let dir = setup_dir();
    let other = setup_dir();
    let outside = write(other.path(), "secret.js", b"return 1;");
    let roots = vec![dir.path().to_path_buf()];
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&outside, dir.path().join("escape.js")).unwrap();
        let err = read_execute_source("escape.js", &roots, dir.path(), 256).unwrap_err();
        assert_eq!(err.code, "file_forbidden");

        let inside = write(dir.path(), "real.js", b"return 2;");
        std::os::unix::fs::symlink(&inside, dir.path().join("link.js")).unwrap();
        let source = read_execute_source("link.js", &roots, dir.path(), 256).unwrap();
        assert_eq!(source, "return 2;");
    }
    let _ = outside;
}

#[test]
fn rejects_missing_files_and_directories() {
    let dir = setup_dir();
    let roots = vec![dir.path().to_path_buf()];
    let err = read_execute_source("missing.js", &roots, dir.path(), 256).unwrap_err();
    assert_eq!(err.code, "file_not_found");
    let err = read_execute_source(".", &roots, dir.path(), 256).unwrap_err();
    assert_eq!(err.code, "file_not_regular");
}

#[test]
fn enforces_size_and_utf8() {
    let dir = setup_dir();
    let roots = vec![dir.path().to_path_buf()];
    write(dir.path(), "big.js", &[b'x'; 40]);
    let err = read_execute_source("big.js", &roots, dir.path(), 16).unwrap_err();
    assert_eq!(err.code, "file_too_large");
    write(dir.path(), "binary.js", &[0xff, 0xfe, 0x41]);
    let err = read_execute_source("binary.js", &roots, dir.path(), 256).unwrap_err();
    assert_eq!(err.code, "file_invalid_utf8");
}

#[test]
fn strips_bom_and_shebang_preserving_lines() {
    let dir = setup_dir();
    let roots = vec![dir.path().to_path_buf()];
    write(dir.path(), "bom.js", "\u{feff}return 1;".as_bytes());
    assert_eq!(
        read_execute_source("bom.js", &roots, dir.path(), 256).unwrap(),
        "return 1;"
    );
    write(dir.path(), "shebang.js", b"#!/usr/bin/env node\nreturn 2;");
    assert_eq!(
        read_execute_source("shebang.js", &roots, dir.path(), 256).unwrap(),
        "\nreturn 2;"
    );
    write(dir.path(), "empty.js", b"");
    assert_eq!(
        read_execute_source("empty.js", &roots, dir.path(), 256).unwrap(),
        ""
    );
}

#[test]
fn empty_roots_disable_file_mode() {
    let dir = setup_dir();
    write(dir.path(), "script.js", b"return 1;");
    let err = read_execute_source("script.js", &[], dir.path(), 256).unwrap_err();
    assert_eq!(err.code, "file_forbidden");
    assert!(err.message.contains("disabled"));
}
