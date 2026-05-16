//! Text merge compatibility exports for the wiki backend.
//!
//! The implementation lives in `omnifuse_core::text_merge`. This module keeps
//! the historical `MergeDecision` / `MergeResult` / `decide_merge` / `three_way_merge`
//! names for existing wiki call-sites.

pub use omnifuse_core::{
  TextMergeDecision as MergeDecision, TextMergeResult as MergeResult, decide_text_merge as decide_merge,
  three_way_text_merge as three_way_merge
};

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
  use super::*;

  #[test]
  fn wiki_merge_reexports_core_policy() {
    let result = decide_merge("aaa\nbbb\nccc\n", "AAA\nbbb\nccc\n", "aaa\nbbb\nCCC\n");
    assert!(matches!(result, MergeDecision::UploadMerged(_)));
  }
}
