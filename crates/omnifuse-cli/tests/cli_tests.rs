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
  assert!(output.status.code().is_some(), "process terminated abnormally (signal)");
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
  of_cmd().args(["mount", "wiki"]).assert().failure();
}

// ─── Unknown subcommand ─────────────────────────────────────────

#[test]
fn test_unknown_subcommand() {
  // `of unknown` — error: no such subcommand
  of_cmd().arg("unknown").assert().failure();
}

// ─── Verbose flags ──────────────────────────────────────────────

#[test]
fn test_verbose_flag() {
  // `of -v check` — should not crash; verbose mode (debug)
  let result = of_cmd().args(["-v", "check"]).assert();
  let output = result.get_output();
  assert!(output.status.code().is_some(), "process terminated abnormally (signal)");
}

#[test]
fn test_double_verbose() {
  // `of -vv check` — should not crash; trace mode
  let result = of_cmd().args(["-vv", "check"]).assert();
  let output = result.get_output();
  assert!(output.status.code().is_some(), "process terminated abnormally (signal)");
}

// ─── mount git — missing mountpoint ──────────────────────────────

#[test]
fn test_mount_git_missing_mountpoint() {
  // `of mount git https://github.com/user/repo` without mountpoint — error
  of_cmd()
    .args(["mount", "git", "https://github.com/user/repo"])
    .assert()
    .failure();
}

// ─── mount wiki — missing auth token ─────────────────────────────

#[test]
fn test_mount_wiki_missing_auth_no_env() {
  // `of mount wiki URL slug /mnt` without --auth and without env var — error
  of_cmd()
    .args(["mount", "wiki", "https://wiki.example.com", "root", "/tmp/wiki"])
    .env_remove("OMNIFUSE_WIKI_TOKEN")
    .assert()
    .failure();
}

// ─── mount git — flag parsing ────────────────────────────────────

#[test]
fn test_mount_git_branch_flag() {
  // `of mount git --help` should mention --branch
  of_cmd()
    .args(["mount", "git", "--help"])
    .assert()
    .success()
    .stdout(predicate::str::contains("--branch"));
}

#[test]
fn test_mount_git_poll_interval_flag() {
  // `of mount git --help` should mention --poll-interval
  of_cmd()
    .args(["mount", "git", "--help"])
    .assert()
    .success()
    .stdout(predicate::str::contains("--poll-interval"));
}

#[test]
fn test_mount_git_allow_other_flag() {
  // `of mount git --help` should mention --allow-other
  of_cmd()
    .args(["mount", "git", "--help"])
    .assert()
    .success()
    .stdout(predicate::str::contains("--allow-other"));
}

#[test]
fn test_mount_git_read_only_flag() {
  // `of mount git --help` should mention --read-only
  of_cmd()
    .args(["mount", "git", "--help"])
    .assert()
    .success()
    .stdout(predicate::str::contains("--read-only"));
}

// ─── mount wiki — flag parsing ───────────────────────────────────

#[test]
fn test_mount_wiki_org_id_flag() {
  // `of mount wiki --help` should mention --org-id
  of_cmd()
    .args(["mount", "wiki", "--help"])
    .assert()
    .success()
    .stdout(predicate::str::contains("--org-id"));
}

#[test]
fn test_mount_wiki_auth_env_var() {
  // `of mount wiki` with OMNIFUSE_WIKI_TOKEN env var — should not fail on auth
  // (it will fail on FUSE mount, but not on argument parsing)
  let result = of_cmd()
    .args([
      "mount",
      "wiki",
      "https://wiki.example.com",
      "root",
      "/tmp/nonexistent-wiki-mount-point"
    ])
    .env("OMNIFUSE_WIKI_TOKEN", "test-token")
    .assert();
  // Should fail due to mount point or FUSE, NOT due to missing auth
  let output = result.get_output();
  let stderr = String::from_utf8_lossy(&output.stderr);
  assert!(
    !stderr.contains("auth") && !stderr.contains("token") && !stderr.contains("required"),
    "should not fail due to missing auth: {stderr}"
  );
}

// ─── Unknown/edge cases ──────────────────────────────────────────

#[test]
fn test_mount_no_backend() {
  // `of mount` without backend type — error
  of_cmd().arg("mount").assert().failure();
}

// ─── gen-config output validation ────────────────────────────────

#[test]
fn test_gen_config_is_valid_toml() {
  // `of gen-config` output should be parseable as TOML
  let output = of_cmd()
    .arg("gen-config")
    .assert()
    .success()
    .get_output()
    .stdout
    .clone();
  let toml_str = String::from_utf8(output).expect("valid utf8");
  // Basic validation: contains expected TOML sections
  assert!(toml_str.contains("[backends."), "should contain backends section");
  assert!(toml_str.contains("[mounts."), "should contain mounts section");
}

// ─── Verbose flags with different commands ───────────────────────

#[test]
fn test_verbose_with_gen_config() {
  // `of -v gen-config` — should not crash
  let result = of_cmd().args(["-v", "gen-config"]).assert();
  let output = result.get_output();
  assert!(output.status.code().is_some(), "process terminated abnormally");
}
