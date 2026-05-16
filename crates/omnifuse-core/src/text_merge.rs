//! Shared UTF-8 text three-way merge policy.

/// Result of a raw three-way text merge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextMergeResult {
  /// No conflict: remote unchanged or local unchanged.
  NoConflict,
  /// Successful merge, contains the result.
  Merged(String),
  /// Conflict between local and remote content.
  Failed {
    /// Local content.
    local: String,
    /// Remote content.
    remote: String
  }
}

/// Domain action selected after comparing base, local and remote content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextMergeDecision {
  /// Upload local content to remote.
  UploadLocal,
  /// Upload auto-merged content to remote and local file.
  UploadMerged(String),
  /// Accept remote content locally without uploading.
  AcceptRemote(String),
  /// Local and remote content are already equivalent.
  AlreadySynced,
  /// Local and remote changes conflict.
  Conflict
}

/// Three-way merge via `diffy`.
///
/// # Arguments
/// * `base` — original content (at the time of base capture)
/// * `local` — user's local changes
/// * `remote` — current content on the server
#[must_use]
pub fn three_way_text_merge(base: &str, local: &str, remote: &str) -> TextMergeResult {
  if base == remote {
    return TextMergeResult::NoConflict;
  }

  if base == local {
    return TextMergeResult::Merged(remote.to_string());
  }

  if local == remote {
    return TextMergeResult::NoConflict;
  }

  match diffy::merge(base, local, remote) {
    Ok(merged) => TextMergeResult::Merged(merged),
    Err(conflict) if conflict.contains("<<<<<<<") || conflict.contains(">>>>>>>") => TextMergeResult::Failed {
      local: local.to_string(),
      remote: remote.to_string()
    },
    Err(merged_without_markers) => TextMergeResult::Merged(merged_without_markers)
  }
}

/// Select a sync action for a base/local/remote text comparison.
#[must_use]
pub fn decide_text_merge(base: &str, local: &str, remote: &str) -> TextMergeDecision {
  if base == remote {
    return if local == remote {
      TextMergeDecision::AlreadySynced
    } else {
      TextMergeDecision::UploadLocal
    };
  }

  if base == local {
    return TextMergeDecision::AcceptRemote(remote.to_string());
  }

  if local == remote {
    return TextMergeDecision::AlreadySynced;
  }

  match three_way_text_merge(base, local, remote) {
    TextMergeResult::NoConflict => TextMergeDecision::AlreadySynced,
    TextMergeResult::Merged(merged) => TextMergeDecision::UploadMerged(merged),
    TextMergeResult::Failed { .. } => TextMergeDecision::Conflict
  }
}

/// Decode bytes as UTF-8 text.
#[must_use]
pub fn decode_utf8_text(bytes: &[u8]) -> Option<&str> {
  std::str::from_utf8(bytes).ok()
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
  use super::*;

  #[test]
  fn no_conflict_when_remote_unchanged() {
    let base = "original";
    let local = "modified locally";
    let remote = "original";

    assert_eq!(three_way_text_merge(base, local, remote), TextMergeResult::NoConflict);
  }

  #[test]
  fn takes_remote_when_local_unchanged() {
    let base = "original";
    let local = "original";
    let remote = "modified remotely";

    assert_eq!(
      three_way_text_merge(base, local, remote),
      TextMergeResult::Merged("modified remotely".to_string())
    );
  }

  #[test]
  fn no_conflict_when_same_change() {
    let base = "original";
    let local = "same change";
    let remote = "same change";

    assert_eq!(three_way_text_merge(base, local, remote), TextMergeResult::NoConflict);
  }

  #[test]
  fn merges_non_overlapping_changes() {
    let base = "line1\nline2\nline3";
    let local = "LOCAL\nline2\nline3";
    let remote = "line1\nline2\nREMOTE";

    let result = three_way_text_merge(base, local, remote);

    match result {
      TextMergeResult::Merged(merged) => {
        assert!(merged.contains("LOCAL"), "should contain LOCAL: {merged}");
        assert!(merged.contains("REMOTE"), "should contain REMOTE: {merged}");
      }
      other => panic!("expected Merged, got {other:?}")
    }
  }

  #[test]
  fn detects_conflicting_changes() {
    let base = "line1\ncommon\nline3";
    let local = "line1\nlocal change\nline3";
    let remote = "line1\nremote change\nline3";

    let result = three_way_text_merge(base, local, remote);
    assert!(matches!(result, TextMergeResult::Failed { .. }));
  }

  #[test]
  fn empty_base_different_content_is_conflict() {
    let base = "";
    let local = "local content";
    let remote = "remote content";

    let result = three_way_text_merge(base, local, remote);
    assert!(
      matches!(result, TextMergeResult::Failed { .. }),
      "empty base + different content -> Failed, got {result:?}"
    );
  }

  #[test]
  fn merges_changes_at_different_positions() {
    let base = "aaa\nbbb\nccc";
    let local = "AAA\nbbb\nccc";
    let remote = "aaa\nbbb\nCCC";

    let result = three_way_text_merge(base, local, remote);
    match result {
      TextMergeResult::Merged(merged) => {
        assert!(merged.contains("AAA"), "should contain AAA: {merged}");
        assert!(merged.contains("CCC"), "should contain CCC: {merged}");
      }
      other => panic!("expected Merged, got {other:?}")
    }
  }

  #[test]
  fn detects_conflicting_changes_on_same_line() {
    let base = "aaa\nbbb";
    let local = "XXX\nbbb";
    let remote = "YYY\nbbb";

    let result = three_way_text_merge(base, local, remote);
    assert!(
      matches!(result, TextMergeResult::Failed { .. }),
      "conflicting changes should return Failed: {result:?}"
    );
  }

  #[test]
  fn empty_base_treated_as_new_file() {
    let base = "";
    let local = "new content";
    let remote = "";

    let result = three_way_text_merge(base, local, remote);
    assert_eq!(
      result,
      TextMergeResult::NoConflict,
      "empty base, remote unchanged -> NoConflict: {result:?}"
    );
  }

  #[test]
  fn empty_local_vs_modified_remote() {
    let base = "original content";
    let local = "";
    let remote = "modified remotely";

    let result = three_way_text_merge(base, local, remote);
    assert!(
      matches!(result, TextMergeResult::Failed { .. }),
      "empty local + modified remote -> Failed, got {result:?}"
    );
  }
}
