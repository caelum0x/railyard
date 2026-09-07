//! Binary-driven test of the documented `snapshot`/`verify` CLI and its exit
//! codes: 0 = clean & signed, 1 = integrity violation, 2 = usage/setup error.
//!
//! Cargo exposes the built binary path via CARGO_BIN_EXE_<name>, so this drives
//! the real executable end to end without hardcoding a target path.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const BIN: &str = env!("CARGO_BIN_EXE_ranguard");
const KEY: &str = "cli-integration-key";

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let p = std::env::temp_dir().join(format!("ranguard_cli_{tag}_{nanos}"));
    fs::create_dir_all(&p).unwrap();
    p
}

fn write_file(dir: &Path, name: &str, contents: &[u8]) {
    let path = dir.join(name);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    let mut f = fs::File::create(path).unwrap();
    f.write_all(contents).unwrap();
}

/// Run the binary with the given args and RANGUARD_KEY, returning its exit code.
fn run_with_key(key: Option<&str>, args: &[&str]) -> i32 {
    let mut cmd = Command::new(BIN);
    cmd.args(args);
    match key {
        Some(k) => cmd.env("RANGUARD_KEY", k),
        None => cmd.env_remove("RANGUARD_KEY"),
    };
    let status = cmd.status().expect("failed to run ranguard binary");
    status.code().expect("process terminated by signal")
}

#[test]
fn snapshot_then_verify_exit_codes() {
    let dir = temp_dir("flow");
    write_file(&dir, "config.cfg", b"fov=90\n");
    write_file(&dir, "assets/data.bin", &[1, 2, 3, 4]);
    // The manifest must live OUTSIDE the scanned dir, otherwise a re-scan would
    // (correctly) flag it as an unexpected ADDED file.
    let manifest = dir.with_extension("manifest");
    let dir_s = dir.to_str().unwrap();
    let man_s = manifest.to_str().unwrap();

    // snapshot with a valid key and directory -> OK (0).
    assert_eq!(run_with_key(Some(KEY), &["snapshot", dir_s, man_s]), 0);
    assert!(manifest.exists());

    // verify an untampered directory -> OK (0).
    assert_eq!(run_with_key(Some(KEY), &["verify", dir_s, man_s]), 0);

    // Tamper a file -> verify reports an integrity violation (1).
    write_file(&dir, "config.cfg", b"fov=90\ncheat=1\n");
    assert_eq!(run_with_key(Some(KEY), &["verify", dir_s, man_s]), 1);

    // Correct files but the wrong key -> bad signature is a violation (1).
    write_file(&dir, "config.cfg", b"fov=90\n");
    assert_eq!(
        run_with_key(Some("totally-wrong-key"), &["verify", dir_s, man_s]),
        1
    );

    fs::remove_dir_all(&dir).ok();
    fs::remove_file(&manifest).ok();
}

#[test]
fn usage_and_setup_errors_exit_two() {
    let dir = temp_dir("usage");
    write_file(&dir, "a.txt", b"hi");
    let dir_s = dir.to_str().unwrap();

    // Missing signing key -> setup error (2).
    assert_eq!(run_with_key(None, &["snapshot", dir_s]), 2);

    // Empty signing key -> setup error (2).
    assert_eq!(run_with_key(Some(""), &["snapshot", dir_s]), 2);

    // Missing directory argument -> usage error (2).
    assert_eq!(run_with_key(Some(KEY), &["snapshot"]), 2);

    // Non-existent directory -> usage error (2).
    let missing = dir.join("does-not-exist");
    assert_eq!(
        run_with_key(Some(KEY), &["snapshot", missing.to_str().unwrap()]),
        2
    );

    // Unknown subcommand -> usage error (2).
    assert_eq!(run_with_key(Some(KEY), &["frobnicate"]), 2);

    fs::remove_dir_all(&dir).ok();
}
