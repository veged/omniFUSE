//! E2E tests for the `of` CLI.
//!
//! Tests cover main subcommands, flags, and error handling.
//! Run with: `cargo test -p omnifuse-cli --test cli_tests`
#![allow(clippy::expect_used)]
#![allow(deprecated)]

use assert_cmd::Command;
use predicates::prelude::*;

/// Helper to invoke the `of` binary.
fn of_cmd() -> Command {
    Command::cargo_bin("of").expect("of binary not found")
}

// ─── Basic flags ────────────────────────────────────────────────

#[test]
fn test_help_flag() {
    // `of --help` should exit 0 and print description
    of_cmd()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("OmniFuse").or(predicate::str::contains("universal VFS")));
}

#[test]
fn test_version_flag() {
    // `of --version` — successful exit, stdout contains version number
    of_cmd()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::is_match(r"\d+\.\d+\.\d+").expect("regex"));
}

// ─── check subcommand ───────────────────────────────────────────

#[test]
fn test_check_command() {
    // `of check` — should not panic; exit code depends on FUSE availability
    let result = of_cmd().arg("check").assert();
    // Ensure the process exited normally (no segfault, etc.)
    let output = result.get_output();
    assert!(
        output.status.code().is_some(),
        "process terminated abnormally (signal)"
    );
}

// ─── gen-config subcommand ──────────────────────────────────────

#[test]
fn test_gen_config() {
    // `of gen-config` — successful exit, stdout contains config template
    of_cmd()
        .arg("gen-config")
        .assert()
        .success()
        .stdout(predicate::str::contains("[backends"))
        .stdout(predicate::str::contains("type = "));
}

#[test]
fn test_gen_config_contains_git_example() {
    // Config template should contain a git backend example
    of_cmd()
        .arg("gen-config")
        .assert()
        .success()
        .stdout(predicate::str::contains("git"));
}

#[test]
fn test_gen_config_contains_wiki_example() {
    // Config template should contain a wiki backend example
    of_cmd()
        .arg("gen-config")
        .assert()
        .success()
        .stdout(predicate::str::contains("wiki"));
}

// ─── mount git — missing required arguments ────────────────────

#[test]
fn test_mount_git_missing_args() {
    // `of mount git` without arguments — error
    of_cmd()
        .args(["mount", "git"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("required").or(predicate::str::contains("Usage")));
}

// ─── mount wiki — missing required arguments ───────────────────

#[test]
fn test_mount_wiki_missing_args() {
    // `of mount wiki` without arguments — error
    of_cmd()
        .args(["mount", "wiki"])
        .assert()
        .failure();
}

// ─── Unknown subcommand ─────────────────────────────────────────

#[test]
fn test_unknown_subcommand() {
    // `of unknown` — error: no such subcommand
    of_cmd()
        .arg("unknown")
        .assert()
        .failure();
}

// ─── Verbose flags ──────────────────────────────────────────────

#[test]
fn test_verbose_flag() {
    // `of -v check` — should not crash; verbose mode (debug)
    let result = of_cmd().args(["-v", "check"]).assert();
    let output = result.get_output();
    assert!(
        output.status.code().is_some(),
        "process terminated abnormally (signal)"
    );
}

#[test]
fn test_double_verbose() {
    // `of -vv check` — should not crash; trace mode
    let result = of_cmd().args(["-vv", "check"]).assert();
    let output = result.get_output();
    assert!(
        output.status.code().is_some(),
        "process terminated abnormally (signal)"
    );
}
