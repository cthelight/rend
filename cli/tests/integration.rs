//! Integration tests exercising the binary end to end.
//!
//! These avoid touching real hardware: only argument handling and
//! deterministic device-not-found paths are tested.

use assert_cmd::Command;
use predicates::str::{contains, starts_with};

fn bin() -> Command {
    Command::cargo_bin("rend").unwrap()
}

#[test]
fn help_exits_zero() {
    bin()
        .arg("--help")
        .assert()
        .success()
        .stdout(contains("drives"))
        .stdout(contains("rip"));
}

#[test]
fn version_exits_zero() {
    bin()
        .arg("--version")
        .assert()
        .success()
        .stdout(starts_with("rend "));
}

#[test]
fn no_subcommand_is_usage_error() {
    bin().assert().code(2).stderr(contains("Usage: rend"));
}

#[test]
fn toc_with_missing_device_fails_cleanly() {
    bin()
        .args(["toc", "-d", "/dev/sr-definitely-not-there"])
        .assert()
        .failure()
        .stderr(contains("device /dev/sr-definitely-not-there not found"));
}

#[test]
fn rip_with_missing_device_fails_cleanly() {
    bin()
        .args(["rip", "-d", "/dev/sr-definitely-not-there"])
        .assert()
        .failure()
        .stderr(contains("device /dev/sr-definitely-not-there not found"));
}

#[test]
fn rip_with_two_missing_devices_fails_cleanly() {
    bin()
        .args([
            "rip",
            "-d",
            "/dev/sr-definitely-not-there",
            "-d",
            "/dev/sr-definitely-not-either",
        ])
        .assert()
        .failure()
        .stderr(contains("device /dev/sr-definitely-not-there not found"));
}

#[test]
fn rip_accepts_all_flag() {
    // `--all` is accepted as a flag; with no hardware it reports no devices.
    bin()
        .args(["rip", "--all"])
        .assert()
        .failure()
        .stderr(contains("no CD-ROM devices found"));
}

#[test]
fn toc_with_two_devices_fails() {
    bin()
        .args(["toc", "-d", "/dev/sr0", "-d", "/dev/sr1"])
        .assert()
        .failure()
        .stderr(contains("expected a single device"));
}

#[test]
fn eject_with_missing_device_fails_cleanly() {
    bin()
        .args(["eject", "-d", "/dev/sr-definitely-not-there"])
        .assert()
        .failure()
        .stderr(contains("device /dev/sr-definitely-not-there not found"));
}
