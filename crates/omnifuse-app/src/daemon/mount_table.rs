//! Degraded mode: discover currently-mounted `OmniFuse` instances by inspecting
//! the operating-system mount table.
//!
//! Used by `of list / status / unmount` when no daemon is running. The output
//! is intentionally lower-fidelity than the daemon's view: we cannot recover
//! the instance hash, dirty-file count, or cache hit ratio from the mount
//! table.

use std::path::{Path, PathBuf};

use tokio::process::Command;

use super::protocol::MountSummary;

/// Backends whose FUSE `fs_name` we recognise.
const OMNIFUSE_FS_NAMES: &[&str] = &["omnifuse", "omnifuse-git", "omnifuse-wiki", "omnifuse-s3"];

/// Read the OS mount table and return entries that look like `OmniFuse` mounts.
///
/// # Errors
///
/// Returns an error when the underlying OS command cannot be executed or its
/// output cannot be parsed.
pub async fn list_active() -> anyhow::Result<Vec<MountSummary>> {
  let mounts = read_native_mount_table().await?;
  Ok(mounts.into_iter().filter_map(|raw| classify_entry(&raw)).collect())
}

/// Look up an entry by mount point.
///
/// # Errors
///
/// Same as [`list_active`].
pub async fn lookup(mount_point: &Path) -> anyhow::Result<Option<MountSummary>> {
  let target = canonicalize(mount_point);
  Ok(
    list_active()
      .await?
      .into_iter()
      .find(|entry| entry.mount_point == target)
  )
}

/// Run the platform-appropriate unmount command for a mount point.
///
/// # Errors
///
/// Returns an error when the unmount command fails. The error message contains
/// the command's stderr.
pub async fn unmount(mount_point: &Path) -> anyhow::Result<()> {
  #[cfg(target_os = "macos")]
  {
    return run_unmount("umount", mount_point).await;
  }

  #[cfg(target_os = "linux")]
  {
    // `fusermount -u` is the conventional unmount for FUSE on Linux.
    return run_unmount("fusermount", mount_point).await;
  }

  #[cfg(not(any(target_os = "macos", target_os = "linux")))]
  {
    let _ = mount_point;
    anyhow::bail!("unmount is only supported on macOS and Linux in V1");
  }
}

#[cfg(target_os = "linux")]
async fn run_unmount(program: &str, mount_point: &Path) -> anyhow::Result<()> {
  let output = Command::new(program).args(["-u"]).arg(mount_point).output().await?;
  if output.status.success() {
    return Ok(());
  }
  let stderr = String::from_utf8_lossy(&output.stderr);
  anyhow::bail!("`{program} -u {}` failed: {stderr}", mount_point.display())
}

#[cfg(target_os = "macos")]
async fn run_unmount(program: &str, mount_point: &Path) -> anyhow::Result<()> {
  let output = Command::new(program).arg(mount_point).output().await?;
  if output.status.success() {
    return Ok(());
  }
  let stderr = String::from_utf8_lossy(&output.stderr);
  anyhow::bail!("`{program} {}` failed: {stderr}", mount_point.display())
}

/// Parsed raw mount table entry.
#[derive(Debug, Clone)]
struct RawMount {
  fs_name: String,
  mount_point: PathBuf
}

#[cfg(target_os = "linux")]
async fn read_native_mount_table() -> anyhow::Result<Vec<RawMount>> {
  let data = tokio::fs::read_to_string("/proc/self/mountinfo").await?;
  let mut out = Vec::new();
  for line in data.lines() {
    if let Some(entry) = parse_linux_mountinfo_line(line) {
      out.push(entry);
    }
  }
  Ok(out)
}

#[cfg(target_os = "linux")]
fn parse_linux_mountinfo_line(line: &str) -> Option<RawMount> {
  // mountinfo format:
  //   <id> <parent> <maj:min> <root> <mount-point> ... - <fstype> <source> <opts>
  let separator_idx = line.find(" - ")?;
  let (left, right) = line.split_at(separator_idx);
  let right = right.trim_start_matches(" - ");
  let mut left_iter = left.split_whitespace();
  let _id = left_iter.next()?;
  let _parent = left_iter.next()?;
  let _major_minor = left_iter.next()?;
  let _root = left_iter.next()?;
  let mount_point = left_iter.next()?;
  let mut right_iter = right.split_whitespace();
  let fstype = right_iter.next()?;
  let source = right_iter.next()?;
  // FUSE mounts surface as `fuse.<fsname>` or sometimes `fuse` with source=fsname.
  let fs_name = fstype.strip_prefix("fuse.").unwrap_or(source).to_string();
  Some(RawMount {
    fs_name,
    mount_point: PathBuf::from(decode_octal(mount_point))
  })
}

#[cfg(target_os = "linux")]
fn decode_octal(input: &str) -> String {
  let bytes = input.as_bytes();
  let mut out = Vec::with_capacity(bytes.len());
  let mut idx = 0;
  while idx < bytes.len() {
    if bytes[idx] == b'\\' && idx + 3 < bytes.len() {
      let triple = &bytes[idx + 1..idx + 4];
      if triple.iter().all(|b| (b'0'..=b'7').contains(b)) {
        let value = (triple[0] - b'0') * 64 + (triple[1] - b'0') * 8 + (triple[2] - b'0');
        out.push(value);
        idx += 4;
        continue;
      }
    }
    out.push(bytes[idx]);
    idx += 1;
  }
  String::from_utf8_lossy(&out).into_owned()
}

#[cfg(target_os = "macos")]
async fn read_native_mount_table() -> anyhow::Result<Vec<RawMount>> {
  let output = Command::new("mount").output().await?;
  if !output.status.success() {
    let stderr = String::from_utf8_lossy(&output.stderr);
    anyhow::bail!("`mount` failed: {stderr}");
  }
  let text = String::from_utf8_lossy(&output.stdout);
  let mut out = Vec::new();
  for line in text.lines() {
    if let Some(entry) = parse_macos_mount_line(line) {
      out.push(entry);
    }
  }
  Ok(out)
}

#[cfg(target_os = "macos")]
fn parse_macos_mount_line(line: &str) -> Option<RawMount> {
  // macOS `mount` output: `<source> on <mount-point> (<fstype>, <opts>)`
  let on_marker = " on ";
  let opt_start = line.rfind(" (")?;
  let on_idx = line.find(on_marker)?;
  if opt_start <= on_idx {
    return None;
  }
  let source = &line[..on_idx];
  let mount_point = &line[on_idx + on_marker.len()..opt_start];
  let opts = &line[opt_start + 2..line.len() - 1];
  let fstype = opts.split(',').next().unwrap_or("").trim();
  let fs_name = if fstype.starts_with("macfuse") || fstype == "fuse" {
    source.to_string()
  } else {
    fstype.to_string()
  };
  Some(RawMount {
    fs_name,
    mount_point: PathBuf::from(mount_point)
  })
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
async fn read_native_mount_table() -> anyhow::Result<Vec<RawMount>> {
  Ok(Vec::new())
}

fn classify_entry(raw: &RawMount) -> Option<MountSummary> {
  let backend = OMNIFUSE_FS_NAMES.iter().find(|name| **name == raw.fs_name).copied()?;
  let backend = backend.strip_prefix("omnifuse-").unwrap_or("omnifuse").to_string();
  Some(MountSummary {
    backend,
    instance: String::new(),
    mount_point: canonicalize(&raw.mount_point),
    age_secs: 0
  })
}

fn canonicalize(path: &Path) -> PathBuf {
  path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
  use super::*;

  #[cfg(target_os = "linux")]
  #[test]
  fn parses_linux_mountinfo_fuse_line() {
    let line = "1 2 0:42 / /mnt/wiki rw,relatime - fuse.omnifuse-wiki omnifuse-wiki rw";
    let parsed = parse_linux_mountinfo_line(line).expect("parsed");
    assert_eq!(parsed.fs_name, "omnifuse-wiki");
    assert_eq!(parsed.mount_point, PathBuf::from("/mnt/wiki"));
  }

  #[cfg(target_os = "linux")]
  #[test]
  fn linux_mountinfo_decodes_octal_in_paths() {
    let line = r"42 0 0:21 / /mnt/with\040spaces ro,relatime - fuse.omnifuse-s3 omnifuse-s3 ro";
    let parsed = parse_linux_mountinfo_line(line).expect("parsed");
    assert_eq!(parsed.fs_name, "omnifuse-s3");
    assert_eq!(parsed.mount_point, PathBuf::from("/mnt/with spaces"));
  }

  #[cfg(target_os = "macos")]
  #[test]
  fn parses_macos_mount_line_for_macfuse() {
    let line = "omnifuse-s3@osxfuse0 on /Volumes/bucket (macfuse, nodev, nosuid)";
    let parsed = parse_macos_mount_line(line).expect("parsed");
    assert_eq!(parsed.fs_name, "omnifuse-s3@osxfuse0");
    assert_eq!(parsed.mount_point, PathBuf::from("/Volumes/bucket"));
  }

  #[test]
  fn classify_keeps_omnifuse_backends_only() {
    let entry = RawMount {
      fs_name: "omnifuse-git".to_string(),
      mount_point: PathBuf::from("/mnt/repo")
    };
    assert!(classify_entry(&entry).is_some());

    let unrelated = RawMount {
      fs_name: "ext4".to_string(),
      mount_point: PathBuf::from("/")
    };
    assert!(classify_entry(&unrelated).is_none());
  }
}
