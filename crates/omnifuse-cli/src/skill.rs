//! Agent-oriented help format.
//!
//! Symmetric to `--help`: every invocation form that works for `help`
//! also works for `skill`. Rendered output is markdown, embedded into the
//! binary at build time.

use std::fmt::Write;

use clap::CommandFactory;

use crate::Cli;

const INTRO: &str = include_str!("../skill/intro.md");
const MOUNT_GIT: &str = include_str!("../skill/mount-git.md");
const MOUNT_WIKI: &str = include_str!("../skill/mount-wiki.md");
const CHECK: &str = include_str!("../skill/check.md");
const GEN_CONFIG: &str = include_str!("../skill/gen-config.md");
const SKILL_SELF: &str = include_str!("../skill/skill.md");
const FOR_CLAUDE: &str = include_str!("../skill/for-claude.md");
const FOR_OPENAI: &str = include_str!("../skill/for-openai.md");
const FOR_LANGCHAIN: &str = include_str!("../skill/for-langchain.md");

/// A `--skill` flag invocation detected before clap parsing.
pub struct SkillInvocation {
  pub path: Vec<String>,
  pub for_tool: Option<String>
}

/// Detect `--skill` anywhere in raw argv and, if present, return the
/// derived subcommand path and `--for=<tool>` value.
///
/// Detection runs before clap so that required positional args of the
/// matched subcommand (e.g. `mount git <SOURCE> <MOUNTPOINT>`) do not
/// need to be supplied just to render a skill section. Mirrors how
/// `--help` short-circuits clap validation.
pub fn detect_skill_flag(argv: &[String]) -> Option<SkillInvocation> {
  let mut has_skill = false;
  let mut for_tool: Option<String> = None;
  let mut positionals: Vec<String> = Vec::new();
  let mut after_dashdash = false;
  let mut iter = argv.iter().skip(1);

  while let Some(a) = iter.next() {
    if after_dashdash {
      continue;
    }
    if a == "--" {
      after_dashdash = true;
      continue;
    }
    if a == "--skill" {
      has_skill = true;
    } else if let Some(v) = a.strip_prefix("--for=") {
      for_tool = Some(v.to_string());
    } else if a == "--for" {
      if let Some(v) = iter.next() {
        for_tool = Some(v.clone());
      }
    } else if !a.starts_with('-') {
      positionals.push(a.clone());
    }
  }

  if !has_skill {
    return None;
  }

  Some(SkillInvocation {
    path: resolve_subcommand_path(&positionals),
    for_tool
  })
}

/// Walk positionals through the clap command tree, keeping the longest
/// prefix that matches a real subcommand chain. Stops at the first
/// non-subcommand token (e.g. a source URL or mountpoint).
fn resolve_subcommand_path(positionals: &[String]) -> Vec<String> {
  let mut cmd = Cli::command();
  let mut path = Vec::new();
  for p in positionals {
    if let Some(sub) = cmd.find_subcommand(p) {
      path.push(p.clone());
      cmd = sub.clone();
    } else {
      break;
    }
  }
  path
}

/// Render the skill manual for a given subcommand path.
pub fn render(path: &[String], for_tool: Option<&str>) -> String {
  let mut out = String::new();
  let _ = writeln!(out, "<!-- of version: {} -->", env!("CARGO_PKG_VERSION"));
  out.push('\n');

  out.push_str(INTRO);
  ensure_trailing_blank_line(&mut out);

  let path_strs: Vec<&str> = path.iter().map(String::as_str).collect();
  match path_strs.as_slice() {
    [] => out.push_str(&top_level_index()),
    ["mount", "git"] => out.push_str(MOUNT_GIT),
    ["mount", "wiki"] => out.push_str(MOUNT_WIKI),
    ["mount"] => {
      out.push_str(MOUNT_GIT);
      ensure_trailing_blank_line(&mut out);
      out.push_str(MOUNT_WIKI);
    }
    ["check"] => out.push_str(CHECK),
    ["gen-config"] => out.push_str(GEN_CONFIG),
    ["skill"] => out.push_str(SKILL_SELF),
    _ => {
      out.push_str(&fallback_section(path));
    }
  }
  ensure_trailing_blank_line(&mut out);

  if let Some(tool) = for_tool {
    if let Some(tail) = tail_for_tool(tool) {
      out.push_str(tail);
      ensure_trailing_blank_line(&mut out);
    }
  }

  out
}

fn tail_for_tool(tool: &str) -> Option<&'static str> {
  match tool {
    "claude" => Some(FOR_CLAUDE),
    "openai" => Some(FOR_OPENAI),
    "langchain" => Some(FOR_LANGCHAIN),
    _ => None
  }
}

fn top_level_index() -> String {
  let cmd = Cli::command();
  let mut out = String::from("## Subcommand index\n\n");
  for sub in cmd.get_subcommands() {
    let name = sub.get_name();
    let about = sub.get_about().map_or_else(String::new, |a| format!(" — {a}"));
    let _ = writeln!(out, "- `of {name}`{about}");
  }
  out.push('\n');
  out.push_str("Run `of skill <subcommand>` or `of <subcommand> --skill` for\n");
  out.push_str("a section dedicated to that subcommand.\n");
  out
}

fn fallback_section(path: &[String]) -> String {
  let mut cmd = Cli::command();
  for p in path {
    if let Some(sub) = cmd.find_subcommand(p) {
      cmd = sub.clone();
    } else {
      break;
    }
  }
  let mut out = String::new();
  let _ = writeln!(out, "## `of {}`", path.join(" "));
  out.push('\n');
  out.push_str(
    "No dedicated skill section is registered for this subcommand. The\n\
     clap-generated help is included verbatim for completeness:\n\n"
  );
  out.push_str("```\n");
  out.push_str(&cmd.render_long_help().to_string());
  if !out.ends_with('\n') {
    out.push('\n');
  }
  out.push_str("```\n");
  out
}

fn ensure_trailing_blank_line(s: &mut String) {
  if !s.ends_with("\n\n") {
    if !s.ends_with('\n') {
      s.push('\n');
    }
    s.push('\n');
  }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
  use super::*;

  fn argv(args: &[&str]) -> Vec<String> {
    std::iter::once("of")
      .chain(args.iter().copied())
      .map(String::from)
      .collect()
  }

  #[test]
  fn detects_top_level_flag() {
    let inv = detect_skill_flag(&argv(&["--skill"])).expect("detected");
    assert!(inv.path.is_empty());
    assert!(inv.for_tool.is_none());
  }

  #[test]
  fn detects_flag_with_subcommand_path() {
    let inv = detect_skill_flag(&argv(&["mount", "git", "--skill"])).expect("detected");
    assert_eq!(inv.path, vec!["mount".to_string(), "git".to_string()]);
  }

  #[test]
  fn detects_flag_in_middle() {
    let inv = detect_skill_flag(&argv(&["--skill", "mount", "wiki"])).expect("detected");
    assert_eq!(inv.path, vec!["mount".to_string(), "wiki".to_string()]);
  }

  #[test]
  fn stops_path_at_non_subcommand() {
    let inv = detect_skill_flag(&argv(&["--skill", "mount", "git", "src", "dst"])).expect("detected");
    assert_eq!(inv.path, vec!["mount".to_string(), "git".to_string()]);
  }

  #[test]
  fn parses_for_equals_form() {
    let inv = detect_skill_flag(&argv(&["--skill", "--for=claude"])).expect("detected");
    assert_eq!(inv.for_tool.as_deref(), Some("claude"));
  }

  #[test]
  fn parses_for_space_form() {
    let inv = detect_skill_flag(&argv(&["--skill", "--for", "openai"])).expect("detected");
    assert_eq!(inv.for_tool.as_deref(), Some("openai"));
  }

  #[test]
  fn returns_none_without_flag() {
    assert!(detect_skill_flag(&argv(&["mount", "git", "src", "dst"])).is_none());
  }

  #[test]
  fn double_dash_blocks_late_skill_flag() {
    assert!(detect_skill_flag(&argv(&["mount", "git", "--", "--skill"])).is_none());
  }

  #[test]
  fn render_empty_path_includes_intro_and_index() {
    let s = render(&[], None);
    assert!(s.contains("omniFUSE — skill manual"));
    assert!(s.contains("Subcommand index"));
    assert!(s.contains("`of mount`"));
  }

  #[test]
  fn render_mount_git_includes_section() {
    let s = render(&["mount".into(), "git".into()], None);
    assert!(s.contains("of mount git"));
    assert!(s.contains("Conflict behavior"));
  }

  #[test]
  fn render_with_for_claude_appends_tail() {
    let s = render(&["mount".into(), "git".into()], Some("claude"));
    assert!(s.contains("For Claude Code"));
  }

  #[test]
  fn render_with_unknown_for_silently_falls_back() {
    let with = render(&["mount".into(), "git".into()], Some("nonexistent"));
    let without = render(&["mount".into(), "git".into()], None);
    assert_eq!(with, without);
  }

  #[test]
  fn render_starts_with_version_marker() {
    let s = render(&[], None);
    assert!(s.starts_with("<!-- of version: "));
  }

  #[test]
  fn render_unknown_path_falls_back_to_clap_help() {
    let s = render(&["nonexistent".into()], None);
    assert!(s.contains("No dedicated skill section"));
  }
}
