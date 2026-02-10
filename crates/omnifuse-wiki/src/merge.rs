//! Three-way merge via diffy.

/// Result of a three-way merge.
#[allow(clippy::module_name_repetitions)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeResult {
  /// No conflict: remote unchanged or local unchanged.
  NoConflict,
  /// Successful merge, contains the result.
  Merged(String),
  /// Conflict (markers or merge error).
  Failed {
    /// Local content.
    local: String,
    /// Remote content.
    remote: String
  }
}

/// Three-way merge via diffy.
///
/// # Arguments
/// * `base` — original content (at the time of page download)
/// * `local` — user's local changes
/// * `remote` — current content on the server
///
/// # Returns
/// `MergeResult` with the merge outcome.
#[must_use]
#[allow(clippy::module_name_repetitions)]
pub fn three_way_merge(base: &str, local: &str, remote: &str) -> MergeResult {
  // Fast path: remote unchanged -> no conflict
  if base == remote {
    return MergeResult::NoConflict;
  }

  // Fast path: local unchanged -> take remote
  if base == local {
    return MergeResult::Merged(remote.to_string());
  }

  // Fast path: identical changes -> no conflict
  if local == remote {
    return MergeResult::NoConflict;
  }

  // Actual merge via diffy
  let merge = diffy::merge(base, local, remote);
  match merge {
    Ok(merged) => MergeResult::Merged(merged),
    Err(conflict) => {
      // Check for conflict markers
      if conflict.contains("<<<<<<<") || conflict.contains(">>>>>>>") {
        MergeResult::Failed {
          local: local.to_string(),
          remote: remote.to_string()
        }
      } else {
        // No markers — clean merge
        MergeResult::Merged(conflict)
      }
    }
  }
}

#[cfg(test)]
#[allow(clippy::panic)]
mod tests {
  use super::*;

  #[test]
  fn no_conflict_when_remote_unchanged() {
    let base = "original";
    let local = "modified locally";
    let remote = "original";

    assert_eq!(three_way_merge(base, local, remote), MergeResult::NoConflict);
  }

  #[test]
  fn takes_remote_when_local_unchanged() {
    let base = "original";
    let local = "original";
    let remote = "modified remotely";

    assert_eq!(
      three_way_merge(base, local, remote),
      MergeResult::Merged("modified remotely".to_string())
    );
  }

  #[test]
  fn no_conflict_when_same_change() {
    let base = "original";
    let local = "same change";
    let remote = "same change";

    assert_eq!(three_way_merge(base, local, remote), MergeResult::NoConflict);
  }

  #[test]
  fn merges_non_overlapping_changes() {
    let base = "line1\nline2\nline3";
    let local = "LOCAL\nline2\nline3";
    let remote = "line1\nline2\nREMOTE";

    let result = three_way_merge(base, local, remote);

    match result {
      MergeResult::Merged(merged) => {
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

    let result = three_way_merge(base, local, remote);
    assert!(matches!(result, MergeResult::Failed { .. }));
  }

  #[test]
  fn empty_base_different_content_is_conflict() {
    let base = "";
    let local = "local content";
    let remote = "remote content";

    let result = three_way_merge(base, local, remote);
    assert!(
      matches!(result, MergeResult::Failed { .. }),
      "empty base + different content -> Failed, got {result:?}"
    );
  }

  /// Changes at different positions: local modifies line 1, remote modifies line 3 -> merge without conflicts.
  #[test]
  fn test_merges_changes_at_different_positions() {
    let base = "aaa\nbbb\nccc";
    let local = "AAA\nbbb\nccc";
    let remote = "aaa\nbbb\nCCC";

    let result = three_way_merge(base, local, remote);
    match result {
      MergeResult::Merged(merged) => {
        assert!(merged.contains("AAA"), "should contain AAA: {merged}");
        assert!(merged.contains("CCC"), "should contain CCC: {merged}");
      }
      other => panic!("expected Merged, got {other:?}")
    }
  }

  /// Conflicting changes on the same line: local -> XXX, remote -> YYY -> Failed.
  #[test]
  fn test_detects_conflicting_changes() {
    let base = "aaa\nbbb";
    let local = "XXX\nbbb";
    let remote = "YYY\nbbb";

    let result = three_way_merge(base, local, remote);
    assert!(
      matches!(result, MergeResult::Failed { .. }),
      "conflicting changes should return Failed: {result:?}"
    );
  }

  /// Empty base, local adds content, remote is empty -> result = local.
  #[test]
  fn test_empty_base_treated_as_new_file() {
    let base = "";
    let local = "new content";
    let remote = "";

    // remote unchanged (base == remote == ""), so NoConflict
    let result = three_way_merge(base, local, remote);
    assert_eq!(
      result,
      MergeResult::NoConflict,
      "empty base, remote unchanged -> NoConflict: {result:?}"
    );
  }

  #[test]
  fn empty_local_vs_modified_remote() {
    let base = "original content";
    let local = "";
    let remote = "modified remotely";

    let result = three_way_merge(base, local, remote);
    assert!(
      matches!(result, MergeResult::Failed { .. }),
      "empty local + modified remote -> Failed, got {result:?}"
    );
  }
}
