//! Integration tests exercising the binary end to end.
//!
//! These avoid touching real hardware: only argument handling and
//! deterministic device-not-found paths are tested.

use assert_cmd::Command;
use predicates::str::{contains, starts_with};

fn bin() -> Command {
    Command::cargo_bin("rend").unwrap()
}

/// True if the machine has any `/dev/sr*` CD-ROM device node.
///
/// `rend rip --all` rips every disc it finds, so a test that relies on
/// there being no hardware would rip a real disc on a developer's machine.
/// Such tests are skipped when a device node is present; they still run on
/// hardware-less CI.
fn has_cdrom_device() -> bool {
    std::fs::read_dir("/dev")
        .map(|entries| {
            entries
                .flatten()
                .any(|e| e.file_name().to_str().is_some_and(|n| n.starts_with("sr")))
        })
        .unwrap_or(false)
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
fn rip_rejects_unknown_format() {
    bin()
        .args(["rip", "--format", "mp3"])
        .assert()
        .code(2)
        .stderr(contains("unknown format 'mp3'"));
}

#[test]
fn rip_accepts_wav_format() {
    // `--format wav` parses; with a missing device it fails on the device.
    bin()
        .args([
            "rip",
            "-d",
            "/dev/sr-definitely-not-there",
            "--format",
            "wav",
        ])
        .assert()
        .failure()
        .stderr(contains("device /dev/sr-definitely-not-there not found"));
}

#[test]
fn rip_accepts_all_flag() {
    // With no hardware, `--all` is accepted and reports no devices. On a
    // machine with a CD-ROM present we only assert the flag parses, since
    // actually running it would rip the real disc.
    if has_cdrom_device() {
        bin()
            .args(["rip", "--help"])
            .assert()
            .success()
            .stdout(contains("--all"));
        return;
    }
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

#[test]
fn rip_accepts_no_metadata_flag() {
    // `--no-metadata` parses; with a missing device it fails on the device.
    bin()
        .args(["rip", "-d", "/dev/sr-definitely-not-there", "--no-metadata"])
        .assert()
        .failure()
        .stderr(contains("device /dev/sr-definitely-not-there not found"));
}

#[test]
fn info_is_a_known_subcommand() {
    bin()
        .arg("--help")
        .assert()
        .success()
        .stdout(contains("info"));
}

#[test]
fn info_with_missing_device_fails_cleanly() {
    bin()
        .args(["info", "-d", "/dev/sr-definitely-not-there"])
        .assert()
        .failure()
        .stderr(contains("device /dev/sr-definitely-not-there not found"));
}
