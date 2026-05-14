//! Integration test stub for rtltcp
//!
//! These tests verify basic binary functionality without requiring
//! an RTL-SDR dongle. They test what can be tested without hardware.

use std::process::Command;

/// Verify the binary exists and can print help
#[test]
fn binary_exists_and_prints_help() {
    let output = Command::new(env!("CARGO_BIN_EXE_rtltcp"))
        .arg("--help")
        .output()
        .expect("failed to execute binary");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("I/Q spectrum server"));
    assert!(stdout.contains("address"));
    assert!(stdout.contains("port"));
    assert!(stdout.contains("device-index"));
    assert!(stdout.contains("buffers"));
    assert!(stdout.contains("tcp-buffers"));
}

/// Verify the binary prints version
#[test]
fn binary_prints_version() {
    let output = Command::new(env!("CARGO_BIN_EXE_rtltcp"))
        .arg("--version")
        .output()
        .expect("failed to execute binary");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("rtltcp"));
}
