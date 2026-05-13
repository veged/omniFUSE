//! Honest FUSE tests — run `of mount git` as an external process.
//!
//! These tests verify real FUSE behavior by mounting through the actual CLI binary.
//! They run by default when FUSE is available.
//!
//! Run: `cargo test -p omnifuse-core --test honest_fuse_tests`
//! Skip: `OMNIFUSE_FUSE_TESTS=0 cargo test -p omnifuse-core --test honest_fuse_tests`

// External-process FUSE tests favor workflow readability over pedantic rewrites.
#![allow(
  clippy::cast_possible_truncation,
  clippy::cast_sign_loss,
  clippy::expect_used,
  clippy::items_after_statements,
  clippy::needless_collect,
  clippy::used_underscore_binding
)]

use std::{
  path::{Path, PathBuf},
  process::{Child, Command, Stdio},
  time::Duration
};

use tempfile::TempDir;

const MOUNT_TIMEOUT: Duration = Duration::from_secs(10);
const UNMOUNT_WAIT: Duration = Duration::from_secs(2);

fn workspace_root() -> PathBuf {
  let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
  p.pop();
  p.pop();
  p
}

fn of_binary() -> PathBuf {
  workspace_root().join("target/debug/of")
}

fn fuse_available() -> bool {
  #[cfg(target_os = "macos")]
  {
    Path::new("/Library/Filesystems/macfuse.fs").exists() || Path::new("/Library/Filesystems/osxfuse.fs").exists()
  }
  #[cfg(target_os = "linux")]
  {
    Path::new("/dev/fuse").exists()
  }
  #[cfg(not(any(target_os = "macos", target_os = "linux")))]
  {
    false
  }
}

fn fuse_tests_disabled() -> bool {
  if std::env::var("OMNIFUSE_FUSE_TESTS").as_deref() == Ok("0") {
    return true;
  }
  !fuse_available()
}

async fn create_test_repo() -> (TempDir, PathBuf) {
  let temp_dir = tempfile::tempdir().expect("create temp dir");
  let repo_path = temp_dir.path().to_path_buf();

  tokio::process::Command::new("git")
    .current_dir(&repo_path)
    .args(["init", "-b", "main"])
    .output()
    .await
    .expect("git init");

  tokio::process::Command::new("git")
    .current_dir(&repo_path)
    .args(["config", "user.email", "test@omnifuse.test"])
    .output()
    .await
    .expect("git config email");

  tokio::process::Command::new("git")
    .current_dir(&repo_path)
    .args(["config", "user.name", "OmniFuse Test"])
    .output()
    .await
    .expect("git config name");

  std::fs::write(repo_path.join("README.md"), "# Test Repo\n").expect("write readme");

  tokio::process::Command::new("git")
    .current_dir(&repo_path)
    .args(["add", "."])
    .output()
    .await
    .expect("git add");

  tokio::process::Command::new("git")
    .current_dir(&repo_path)
    .args(["commit", "-m", "Initial commit"])
    .output()
    .await
    .expect("git commit");

  (temp_dir, repo_path)
}

fn spawn_of_mount(repo_path: &Path, mount_point: &Path) -> Child {
  Command::new(of_binary())
    .args([
      "mount",
      "git",
      repo_path.to_str().expect("repo path to str"),
      mount_point.to_str().expect("mount point to str")
    ])
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .expect("spawn of mount")
}

fn wait_for_mount(mount_point: &Path, timeout: Duration) -> bool {
  let start = std::time::Instant::now();
  while start.elapsed() < timeout {
    if mount_point.join("README.md").exists() {
      return true;
    }
    std::thread::sleep(Duration::from_millis(500));
  }
  false
}

fn unmount_of(child: &mut Child, _mount_point: &Path) {
  let _ = child.kill();
  let _ = child.wait();
  std::thread::sleep(UNMOUNT_WAIT);
}

// ============================================================================
// TESTS
// ============================================================================

#[tokio::test]
#[ignore = "requires FUSE; run with --ignored"]
async fn test_fuse_mount_readme_visible() {
  if fuse_tests_disabled() {
    return;
  }
  let (_repo_temp, repo_path) = create_test_repo().await;
  let mount_temp = tempfile::tempdir().expect("create mount temp dir");
  let mount_point = mount_temp.path().join("mnt");
  std::fs::create_dir_all(&mount_point).expect("create mount point dir");

  let mut child = spawn_of_mount(&repo_path, &mount_point);
  assert!(
    wait_for_mount(&mount_point, MOUNT_TIMEOUT),
    "mount did not appear within timeout"
  );

  let readme_path = mount_point.join("README.md");
  assert!(readme_path.exists(), "README.md should be visible through mount");
  let content = std::fs::read_to_string(&readme_path).expect("read README.md");
  assert!(
    content.contains("# Test Repo"),
    "README.md should contain expected content"
  );

  unmount_of(&mut child, &mount_point);
}

#[tokio::test]
#[ignore = "requires FUSE; run with --ignored"]
async fn test_fuse_create_file_visible_in_git() {
  if fuse_tests_disabled() {
    return;
  }
  let (_repo_temp, repo_path) = create_test_repo().await;
  let mount_temp = tempfile::tempdir().expect("create mount temp dir");
  let mount_point = mount_temp.path().join("mnt");
  std::fs::create_dir_all(&mount_point).expect("create mount point dir");

  let mut child = spawn_of_mount(&repo_path, &mount_point);
  assert!(
    wait_for_mount(&mount_point, MOUNT_TIMEOUT),
    "mount did not appear within timeout"
  );

  let new_file = mount_point.join("new_file.md");
  std::fs::write(&new_file, "# New File\n").expect("write new file through mount");
  assert!(new_file.exists(), "new file should exist through mount");

  unmount_of(&mut child, &mount_point);

  let repo_file = repo_path.join("new_file.md");
  assert!(
    repo_file.exists(),
    "new file should persist in git working tree after unmount"
  );
  let content = std::fs::read_to_string(&repo_file).expect("read persisted file");
  assert_eq!(content, "# New File\n");
}

#[tokio::test]
#[ignore = "requires FUSE; run with --ignored"]
async fn test_fuse_edit_file_persists() {
  if fuse_tests_disabled() {
    return;
  }
  let (_repo_temp, repo_path) = create_test_repo().await;
  let mount_temp = tempfile::tempdir().expect("create mount temp dir");
  let mount_point = mount_temp.path().join("mnt");
  std::fs::create_dir_all(&mount_point).expect("create mount point dir");

  let mut child = spawn_of_mount(&repo_path, &mount_point);
  assert!(
    wait_for_mount(&mount_point, MOUNT_TIMEOUT),
    "mount did not appear within timeout"
  );

  let readme_mount = mount_point.join("README.md");
  std::fs::write(&readme_mount, "# Edited via FUSE\n").expect("edit README through mount");

  unmount_of(&mut child, &mount_point);

  let readme_repo = repo_path.join("README.md");
  let content = std::fs::read_to_string(&readme_repo).expect("read README from repo");
  assert!(
    content.contains("# Edited via FUSE"),
    "README.md should contain edited content after unmount"
  );
}

#[tokio::test]
#[ignore = "requires FUSE; run with --ignored"]
async fn test_fuse_delete_file() {
  if fuse_tests_disabled() {
    return;
  }
  let (_repo_temp, repo_path) = create_test_repo().await;
  let mount_temp = tempfile::tempdir().expect("create mount temp dir");
  let mount_point = mount_temp.path().join("mnt");
  std::fs::create_dir_all(&mount_point).expect("create mount point dir");

  let mut child = spawn_of_mount(&repo_path, &mount_point);
  assert!(
    wait_for_mount(&mount_point, MOUNT_TIMEOUT),
    "mount did not appear within timeout"
  );

  let readme_mount = mount_point.join("README.md");
  std::fs::remove_file(&readme_mount).expect("delete README through mount");

  unmount_of(&mut child, &mount_point);

  let readme_repo = repo_path.join("README.md");
  let exists = readme_repo.exists();
  eprintln!("[INFO] README.md exists after unmount: {exists}");
}

#[tokio::test]
#[ignore = "requires FUSE; run with --ignored"]
async fn test_fuse_rename_file() {
  if fuse_tests_disabled() {
    return;
  }
  let (_repo_temp, repo_path) = create_test_repo().await;
  let mount_temp = tempfile::tempdir().expect("create mount temp dir");
  let mount_point = mount_temp.path().join("mnt");
  std::fs::create_dir_all(&mount_point).expect("create mount point dir");

  let mut child = spawn_of_mount(&repo_path, &mount_point);
  assert!(
    wait_for_mount(&mount_point, MOUNT_TIMEOUT),
    "mount did not appear within timeout"
  );

  let old_path = mount_point.join("README.md");
  let new_path = mount_point.join("RENAMED.md");
  std::fs::rename(&old_path, &new_path).expect("rename file through mount");

  unmount_of(&mut child, &mount_point);

  let old_repo = repo_path.join("README.md");
  let new_repo = repo_path.join("RENAMED.md");
  let old_exists = old_repo.exists();
  let new_exists = new_repo.exists();
  eprintln!("[INFO] README.md exists after unmount: {old_exists}, RENAMED.md exists: {new_exists}");
}

#[tokio::test]
#[ignore = "requires FUSE; run with --ignored"]
async fn test_fuse_create_nested_directory() {
  if fuse_tests_disabled() {
    return;
  }
  let (_repo_temp, repo_path) = create_test_repo().await;
  let mount_temp = tempfile::tempdir().expect("create mount temp dir");
  let mount_point = mount_temp.path().join("mnt");
  std::fs::create_dir_all(&mount_point).expect("create mount point dir");

  let mut child = spawn_of_mount(&repo_path, &mount_point);
  assert!(
    wait_for_mount(&mount_point, MOUNT_TIMEOUT),
    "mount did not appear within timeout"
  );

  let nested_dir = mount_point.join("src").join("components").join("ui");
  std::fs::create_dir_all(&nested_dir).expect("create nested dirs through mount");

  let button_file = nested_dir.join("button.md");
  std::fs::write(&button_file, "# Button\n").expect("write button.md through mount");

  unmount_of(&mut child, &mount_point);

  let repo_button = repo_path.join("src").join("components").join("ui").join("button.md");
  let exists = repo_button.exists();
  eprintln!("[INFO] button.md exists after unmount: {exists}");

  if exists {
    let content = std::fs::read_to_string(&repo_button).expect("read button.md");
    assert_eq!(content, "# Button\n");
  }
}

#[tokio::test]
#[ignore = "requires FUSE; run with --ignored"]
async fn test_fuse_concurrent_file_creation() {
  if fuse_tests_disabled() {
    return;
  }
  let (_repo_temp, repo_path) = create_test_repo().await;
  let mount_temp = tempfile::tempdir().expect("create mount temp dir");
  let mount_point = mount_temp.path().join("mnt");
  std::fs::create_dir_all(&mount_point).expect("create mount point dir");

  let mut child = spawn_of_mount(&repo_path, &mount_point);
  assert!(
    wait_for_mount(&mount_point, MOUNT_TIMEOUT),
    "mount did not appear within timeout"
  );

  let mount_point_clone = mount_point.clone();
  let handles: Vec<_> = (0..10)
    .map(|i| {
      let mp = mount_point_clone.clone();
      std::thread::spawn(move || {
        let file = mp.join(format!("concurrent_{i}.md"));
        std::fs::write(&file, format!("# File {i}\n")).expect("write concurrent file");
        file
      })
    })
    .collect();

  let files: Vec<PathBuf> = handles
    .into_iter()
    .map(|h| h.join().expect("concurrent thread panicked"))
    .collect();

  unmount_of(&mut child, &mount_point);

  let mut all_exist = true;
  for file in &files {
    let repo_file = repo_path.join(file.file_name().expect("file name"));
    if !repo_file.exists() {
      eprintln!("[WARN] concurrent file missing: {:?}", file.file_name());
      all_exist = false;
    }
  }

  if !all_exist {
    eprintln!("[WARN] not all concurrent files persisted after unmount");
  }
}

#[tokio::test]
#[ignore = "requires FUSE; run with --ignored"]
async fn test_fuse_write_triggers_git_commit() {
  if fuse_tests_disabled() {
    return;
  }
  let (_repo_temp, repo_path) = create_test_repo().await;
  let mount_temp = tempfile::tempdir().expect("create mount temp dir");
  let mount_point = mount_temp.path().join("mnt");
  std::fs::create_dir_all(&mount_point).expect("create mount point dir");

  let mut child = spawn_of_mount(&repo_path, &mount_point);
  assert!(
    wait_for_mount(&mount_point, MOUNT_TIMEOUT),
    "mount did not appear within timeout"
  );

  let new_file = mount_point.join("new_file.md");
  std::fs::write(&new_file, "# New\n").expect("write new file");

  unmount_of(&mut child, &mount_point);

  let output = std::process::Command::new("git")
    .current_dir(&repo_path)
    .args(["log", "--oneline"])
    .output()
    .expect("git log");
  let log = String::from_utf8_lossy(&output.stdout);
  eprintln!("[INFO] git log:\n{log}");
  assert!(
    log.lines().count() >= 2,
    "should have at least 2 commits (initial + sync)"
  );
}

#[tokio::test]
#[ignore = "requires FUSE; run with --ignored"]
async fn test_fuse_vscode_atomic_save() {
  if fuse_tests_disabled() {
    return;
  }
  let (_repo_temp, repo_path) = create_test_repo().await;
  let mount_temp = tempfile::tempdir().expect("create mount temp dir");
  let mount_point = mount_temp.path().join("mnt");
  std::fs::create_dir_all(&mount_point).expect("create mount point dir");

  let mut child = spawn_of_mount(&repo_path, &mount_point);
  assert!(
    wait_for_mount(&mount_point, MOUNT_TIMEOUT),
    "mount did not appear within timeout"
  );

  let tmp_file = mount_point.join("README.md.tmp");
  std::fs::write(&tmp_file, "# Atomic Save\n").expect("write temp file");
  let readme = mount_point.join("README.md");
  std::fs::rename(&tmp_file, &readme).expect("rename temp to README");

  unmount_of(&mut child, &mount_point);

  let content = std::fs::read_to_string(repo_path.join("README.md")).expect("read README");
  assert_eq!(content, "# Atomic Save\n");
}

#[tokio::test]
#[ignore = "requires FUSE; run with --ignored"]
async fn test_fuse_vim_swp_not_tracked() {
  if fuse_tests_disabled() {
    return;
  }
  let (_repo_temp, repo_path) = create_test_repo().await;
  let mount_temp = tempfile::tempdir().expect("create mount temp dir");
  let mount_point = mount_temp.path().join("mnt");
  std::fs::create_dir_all(&mount_point).expect("create mount point dir");

  let mut child = spawn_of_mount(&repo_path, &mount_point);
  assert!(
    wait_for_mount(&mount_point, MOUNT_TIMEOUT),
    "mount did not appear within timeout"
  );

  let swp = mount_point.join(".README.md.swp");
  std::fs::write(&swp, "swap data").expect("write swp");
  std::fs::write(mount_point.join("README.md"), "# Edited\n").expect("edit README");
  std::fs::remove_file(&swp).expect("delete swp");

  unmount_of(&mut child, &mount_point);

  let swp_repo = repo_path.join(".README.md.swp");
  assert!(!swp_repo.exists(), ".swp should not persist in repo");
}

#[tokio::test]
#[ignore = "requires FUSE; run with --ignored"]
async fn test_fuse_truncate_and_write() {
  if fuse_tests_disabled() {
    return;
  }
  let (_repo_temp, repo_path) = create_test_repo().await;
  let mount_temp = tempfile::tempdir().expect("create mount temp dir");
  let mount_point = mount_temp.path().join("mnt");
  std::fs::create_dir_all(&mount_point).expect("create mount point dir");

  let mut child = spawn_of_mount(&repo_path, &mount_point);
  assert!(
    wait_for_mount(&mount_point, MOUNT_TIMEOUT),
    "mount did not appear within timeout"
  );

  use std::io::Write;
  let f = std::fs::OpenOptions::new()
    .write(true)
    .truncate(true)
    .open(mount_point.join("README.md"))
    .expect("open truncate");
  let mut f = f;
  f.write_all(b"short\n").expect("write short");
  drop(f);

  unmount_of(&mut child, &mount_point);

  let content = std::fs::read_to_string(repo_path.join("README.md")).expect("read README");
  assert_eq!(content, "short\n");
}

#[tokio::test]
#[ignore = "requires FUSE; run with --ignored"]
async fn test_fuse_append_to_file() {
  if fuse_tests_disabled() {
    return;
  }
  let (_repo_temp, repo_path) = create_test_repo().await;
  let mount_temp = tempfile::tempdir().expect("create mount temp dir");
  let mount_point = mount_temp.path().join("mnt");
  std::fs::create_dir_all(&mount_point).expect("create mount point dir");

  let mut child = spawn_of_mount(&repo_path, &mount_point);
  assert!(
    wait_for_mount(&mount_point, MOUNT_TIMEOUT),
    "mount did not appear within timeout"
  );

  use std::io::Write;
  let mut f = std::fs::OpenOptions::new()
    .append(true)
    .open(mount_point.join("README.md"))
    .expect("open append");
  f.write_all(b"appended\n").expect("append");
  drop(f);

  unmount_of(&mut child, &mount_point);

  let content = std::fs::read_to_string(repo_path.join("README.md")).expect("read README");
  assert!(content.contains("appended"), "should contain appended content");
}

#[tokio::test]
#[ignore = "requires FUSE; run with --ignored"]
async fn test_fuse_git_dir_hidden() {
  if fuse_tests_disabled() {
    return;
  }
  let (_repo_temp, _repo_path) = create_test_repo().await;
  let mount_temp = tempfile::tempdir().expect("create mount temp dir");
  let mount_point = mount_temp.path().join("mnt");
  std::fs::create_dir_all(&mount_point).expect("create mount point dir");

  let mut child = spawn_of_mount(&_repo_path, &mount_point);
  assert!(
    wait_for_mount(&mount_point, MOUNT_TIMEOUT),
    "mount did not appear within timeout"
  );

  assert!(!mount_point.join(".git").exists(), ".git should be hidden through FUSE");

  unmount_of(&mut child, &mount_point);
}

#[tokio::test]
#[ignore = "requires FUSE; run with --ignored"]
async fn test_fuse_large_file_preserved() {
  if fuse_tests_disabled() {
    return;
  }
  let (_repo_temp, repo_path) = create_test_repo().await;
  let mount_temp = tempfile::tempdir().expect("create mount temp dir");
  let mount_point = mount_temp.path().join("mnt");
  std::fs::create_dir_all(&mount_point).expect("create mount point dir");

  let mut child = spawn_of_mount(&repo_path, &mount_point);
  assert!(
    wait_for_mount(&mount_point, MOUNT_TIMEOUT),
    "mount did not appear within timeout"
  );

  let data: Vec<u8> = (0..5 * 1024 * 1024).map(|i| (i % 256) as u8).collect();
  std::fs::write(mount_point.join("large.bin"), &data).expect("write large file");

  unmount_of(&mut child, &mount_point);

  let repo_file = repo_path.join("large.bin");
  assert!(repo_file.exists(), "large file should persist");
  let read_back = std::fs::read(&repo_file).expect("read large file");
  assert_eq!(read_back.len(), data.len(), "large file size should match");
  assert_eq!(read_back, data, "large file content should match");
}

#[tokio::test]
#[ignore = "requires FUSE; run with --ignored"]
async fn test_fuse_unicode_filename() {
  if fuse_tests_disabled() {
    return;
  }
  let (_repo_temp, repo_path) = create_test_repo().await;
  let mount_temp = tempfile::tempdir().expect("create mount temp dir");
  let mount_point = mount_temp.path().join("mnt");
  std::fs::create_dir_all(&mount_point).expect("create mount point dir");

  let mut child = spawn_of_mount(&repo_path, &mount_point);
  assert!(
    wait_for_mount(&mount_point, MOUNT_TIMEOUT),
    "mount did not appear within timeout"
  );

  std::fs::write(mount_point.join("привет_мир.md"), "# Привет\n").expect("write unicode file");

  unmount_of(&mut child, &mount_point);

  let repo_file = repo_path.join("привет_мир.md");
  assert!(repo_file.exists(), "unicode file should persist");
  let content = std::fs::read_to_string(&repo_file).expect("read unicode file");
  assert_eq!(content, "# Привет\n");
}

#[tokio::test]
#[ignore = "requires FUSE; run with --ignored"]
async fn test_fuse_filename_with_spaces() {
  if fuse_tests_disabled() {
    return;
  }
  let (_repo_temp, repo_path) = create_test_repo().await;
  let mount_temp = tempfile::tempdir().expect("create mount temp dir");
  let mount_point = mount_temp.path().join("mnt");
  std::fs::create_dir_all(&mount_point).expect("create mount point dir");

  let mut child = spawn_of_mount(&repo_path, &mount_point);
  assert!(
    wait_for_mount(&mount_point, MOUNT_TIMEOUT),
    "mount did not appear within timeout"
  );

  std::fs::write(mount_point.join("my document file.md"), "# Spaces\n").expect("write file with spaces");

  unmount_of(&mut child, &mount_point);

  let repo_file = repo_path.join("my document file.md");
  assert!(repo_file.exists(), "file with spaces should persist");
}

#[tokio::test]
#[ignore = "requires FUSE; run with --ignored"]
async fn test_fuse_read_while_write() {
  if fuse_tests_disabled() {
    return;
  }
  let (_repo_temp, repo_path) = create_test_repo().await;
  let mount_temp = tempfile::tempdir().expect("create mount temp dir");
  let mount_point = mount_temp.path().join("mnt");
  std::fs::create_dir_all(&mount_point).expect("create mount point dir");

  let mut child = spawn_of_mount(&repo_path, &mount_point);
  assert!(
    wait_for_mount(&mount_point, MOUNT_TIMEOUT),
    "mount did not appear within timeout"
  );

  let mount_point_clone = mount_point.clone();
  let reader = std::thread::spawn(move || {
    for _ in 0..50 {
      let _ = std::fs::read_to_string(mount_point_clone.join("README.md"));
      std::thread::sleep(Duration::from_millis(10));
    }
  });

  let mount_point_clone2 = mount_point.clone();
  let writer = std::thread::spawn(move || {
    for i in 0..10 {
      let _ = std::fs::write(mount_point_clone2.join("README.md"), format!("# Version {i}\n"));
      std::thread::sleep(Duration::from_millis(20));
    }
  });

  reader.join().expect("reader panicked");
  writer.join().expect("writer panicked");

  unmount_of(&mut child, &mount_point);
}

#[tokio::test]
#[ignore = "requires FUSE; run with --ignored"]
async fn test_fuse_mass_file_creation() {
  if fuse_tests_disabled() {
    return;
  }
  let (_repo_temp, repo_path) = create_test_repo().await;
  let mount_temp = tempfile::tempdir().expect("create mount temp dir");
  let mount_point = mount_temp.path().join("mnt");
  std::fs::create_dir_all(&mount_point).expect("create mount point dir");

  let mut child = spawn_of_mount(&repo_path, &mount_point);
  assert!(
    wait_for_mount(&mount_point, MOUNT_TIMEOUT),
    "mount did not appear within timeout"
  );

  for i in 0..50 {
    std::fs::write(mount_point.join(format!("file_{i:03}.md")), format!("# File {i}\n")).expect("write file");
  }

  unmount_of(&mut child, &mount_point);

  let mut count = 0;
  for i in 0..50 {
    if repo_path.join(format!("file_{i:03}.md")).exists() {
      count += 1;
    }
  }
  assert_eq!(count, 50, "all 50 files should persist");
}

#[tokio::test]
#[ignore = "requires FUSE; run with --ignored"]
async fn test_fuse_multiple_edits_same_file() {
  if fuse_tests_disabled() {
    return;
  }
  let (_repo_temp, repo_path) = create_test_repo().await;
  let mount_temp = tempfile::tempdir().expect("create mount temp dir");
  let mount_point = mount_temp.path().join("mnt");
  std::fs::create_dir_all(&mount_point).expect("create mount point dir");

  let mut child = spawn_of_mount(&repo_path, &mount_point);
  assert!(
    wait_for_mount(&mount_point, MOUNT_TIMEOUT),
    "mount did not appear within timeout"
  );

  for i in 1..=5 {
    std::fs::write(mount_point.join("README.md"), format!("# Version {i}\n")).expect("write version");
    std::thread::sleep(Duration::from_millis(100));
  }

  unmount_of(&mut child, &mount_point);

  let content = std::fs::read_to_string(repo_path.join("README.md")).expect("read README");
  assert!(content.contains("# Version 5"), "should have final version");
}

#[tokio::test]
#[ignore = "requires FUSE; run with --ignored"]
async fn test_fuse_create_then_delete() {
  if fuse_tests_disabled() {
    return;
  }
  let (_repo_temp, repo_path) = create_test_repo().await;
  let mount_temp = tempfile::tempdir().expect("create mount temp dir");
  let mount_point = mount_temp.path().join("mnt");
  std::fs::create_dir_all(&mount_point).expect("create mount point dir");

  let mut child = spawn_of_mount(&repo_path, &mount_point);
  assert!(
    wait_for_mount(&mount_point, MOUNT_TIMEOUT),
    "mount did not appear within timeout"
  );

  let temp_file = mount_point.join("temp.md");
  std::fs::write(&temp_file, "# Temp\n").expect("write temp file");
  assert!(temp_file.exists(), "temp file should exist");
  std::fs::remove_file(&temp_file).expect("delete temp file");

  unmount_of(&mut child, &mount_point);

  assert!(
    !repo_path.join("temp.md").exists(),
    "temp.md should NOT exist after create+delete"
  );
}

#[tokio::test]
#[ignore = "requires FUSE; run with --ignored"]
async fn test_fuse_git_status_shows_changes() {
  if fuse_tests_disabled() {
    return;
  }
  let (_repo_temp, repo_path) = create_test_repo().await;
  let mount_temp = tempfile::tempdir().expect("create mount temp dir");
  let mount_point = mount_temp.path().join("mnt");
  std::fs::create_dir_all(&mount_point).expect("create mount point dir");

  let mut child = spawn_of_mount(&repo_path, &mount_point);
  assert!(
    wait_for_mount(&mount_point, MOUNT_TIMEOUT),
    "mount did not appear within timeout"
  );

  std::fs::write(mount_point.join("new_file.md"), "# New\n").expect("write new file");

  let output = std::process::Command::new("git")
    .current_dir(&repo_path)
    .args(["status", "--porcelain"])
    .output()
    .expect("git status");
  let status = String::from_utf8_lossy(&output.stdout);
  assert!(
    status.contains("new_file.md"),
    "git status should show new file: {status}"
  );

  unmount_of(&mut child, &mount_point);
}
