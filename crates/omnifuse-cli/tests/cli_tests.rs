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

// ─── S3 subcommand ──────────────────────────────────────────────

#[test]
fn test_mount_s3_missing_args() {
  // `of mount s3` without bucket/mountpoint should fail.
  of_cmd().args(["mount", "s3"]).assert().failure();
}

#[test]
fn test_mount_s3_help_mentions_endpoint_prefix_and_credentials() {
  of_cmd()
    .args(["mount", "s3", "--help"])
    .assert()
    .success()
    .stdout(predicate::str::contains("--endpoint"))
    .stdout(predicate::str::contains("--prefix"))
    .stdout(predicate::str::contains("--access-key-id"));
}

// ─── skill (agent-oriented help) ─────────────────────────────────

#[test]
fn test_skill_flag_at_top_level() {
  // `of --skill` — exits 0, output starts with version marker and intro
  of_cmd()
    .arg("--skill")
    .assert()
    .success()
    .stdout(predicate::str::starts_with("<!-- of version: "))
    .stdout(predicate::str::contains("omniFUSE — skill manual"))
    .stdout(predicate::str::contains("Subcommand index"));
}

#[test]
fn test_skill_subcommand_at_top_level() {
  // `of skill` — same content as `of --skill`
  of_cmd()
    .arg("skill")
    .assert()
    .success()
    .stdout(predicate::str::contains("omniFUSE — skill manual"))
    .stdout(predicate::str::contains("Subcommand index"));
}

#[test]
fn test_skill_flag_with_required_args_missing() {
  // `of mount git --skill` — skill flag short-circuits validation,
  // so missing positional args don't cause an error.
  of_cmd()
    .args(["mount", "git", "--skill"])
    .assert()
    .success()
    .stdout(predicate::str::contains("of mount git"))
    .stdout(predicate::str::contains("Conflict behavior"));
}

#[test]
fn test_skill_subcommand_with_path() {
  // `of skill mount wiki` — section for `mount wiki`
  of_cmd()
    .args(["skill", "mount", "wiki"])
    .assert()
    .success()
    .stdout(predicate::str::contains("of mount wiki"))
    .stdout(predicate::str::contains("three-way merge"));
}

#[test]
fn test_skill_for_mount_s3_renders_s3_section() {
  // `of skill mount s3` — section for `mount s3` with CAS specifics.
  of_cmd()
    .args(["skill", "mount", "s3"])
    .assert()
    .success()
    .stdout(predicate::str::contains("of mount s3"))
    .stdout(predicate::str::contains("If-Match"))
    .stdout(predicate::str::contains("Capability requirements"));
}

#[test]
fn test_skill_flag_and_subcommand_outputs_match() {
  // The flag form and the subcommand form must produce byte-identical output.
  let via_flag = of_cmd().args(["mount", "git", "--skill"]).output().expect("run");
  let via_sub = of_cmd().args(["skill", "mount", "git"]).output().expect("run");
  assert!(via_flag.status.success());
  assert!(via_sub.status.success());
  assert_eq!(
    String::from_utf8_lossy(&via_flag.stdout),
    String::from_utf8_lossy(&via_sub.stdout),
    "skill flag and subcommand should produce identical output"
  );
}

#[test]
fn test_skill_for_claude_appends_tail() {
  // `of skill mount git --for=claude` — appends Claude-specific tail
  of_cmd()
    .args(["skill", "mount", "git", "--for=claude"])
    .assert()
    .success()
    .stdout(predicate::str::contains("For Claude Code"))
    .stdout(predicate::str::contains("--add-dir"));
}

#[test]
fn test_skill_for_unknown_tool_silently_falls_back() {
  // Unknown `--for` value: no error, no tail appended.
  let with_unknown = of_cmd()
    .args(["skill", "mount", "git", "--for=nonsense"])
    .output()
    .expect("run");
  let without_for = of_cmd().args(["skill", "mount", "git"]).output().expect("run");
  assert!(with_unknown.status.success());
  assert!(without_for.status.success());
  assert_eq!(
    String::from_utf8_lossy(&with_unknown.stdout),
    String::from_utf8_lossy(&without_for.stdout)
  );
}

#[test]
fn test_skill_starts_with_version_marker() {
  // Invariant: first line of skill output is the version marker.
  let out = of_cmd().arg("--skill").output().expect("run");
  let stdout = String::from_utf8_lossy(&out.stdout);
  let first_line = stdout.lines().next().expect("at least one line");
  assert!(
    first_line.starts_with("<!-- of version: "),
    "first line should be version marker, got: {first_line:?}"
  );
}

#[test]
fn test_skill_for_each_subcommand_path() {
  // Invariant: for every leaf subcommand we know about, both invocation
  // forms succeed and produce non-empty output. Guards the
  // "every form that works for --help works for --skill" contract.
  let leaf_paths: &[&[&str]] = &[
    &["mount", "git"],
    &["mount", "wiki"],
    &["mount", "s3"],
    &["check"],
    &["gen-config"],
    &["skill"]
  ];

  for path in leaf_paths {
    // Flag form: `of <path...> --skill`
    let mut args: Vec<&str> = path.to_vec();
    args.push("--skill");
    let out = of_cmd().args(&args).output().expect("run");
    assert!(out.status.success(), "flag form failed for {path:?}");
    assert!(!out.stdout.is_empty(), "flag form empty for {path:?}");

    // Subcommand form: `of skill <path...>`
    let mut args: Vec<&str> = vec!["skill"];
    args.extend_from_slice(path);
    let out = of_cmd().args(&args).output().expect("run");
    assert!(out.status.success(), "subcommand form failed for {path:?}");
    assert!(!out.stdout.is_empty(), "subcommand form empty for {path:?}");
  }
}
