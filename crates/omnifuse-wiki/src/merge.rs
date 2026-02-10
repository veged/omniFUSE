//! Three-way merge через diffy.

/// Результат three-way merge.
#[allow(clippy::module_name_repetitions)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeResult {
  /// Нет конфликта: remote не изменился или local не изменился.
  NoConflict,
  /// Успешное слияние, содержит результат.
  Merged(String),
  /// Конфликт (маркеры или ошибка merge).
  Failed {
    /// Локальный контент.
    local: String,
    /// Remote контент.
    remote: String
  }
}

/// Three-way merge через diffy.
///
/// # Аргументы
/// * `base` — оригинальный контент (при загрузке страницы)
/// * `local` — локальные изменения пользователя
/// * `remote` — текущий контент на сервере
///
/// # Возвращает
/// `MergeResult` с результатом слияния.
#[must_use]
#[allow(clippy::module_name_repetitions)]
pub fn three_way_merge(base: &str, local: &str, remote: &str) -> MergeResult {
  // Fast path: remote не изменился → нет конфликта
  if base == remote {
    return MergeResult::NoConflict;
  }

  // Fast path: local не изменился → берём remote
  if base == local {
    return MergeResult::Merged(remote.to_string());
  }

  // Fast path: одинаковые изменения → нет конфликта
  if local == remote {
    return MergeResult::NoConflict;
  }

  // Реальный merge через diffy
  let merge = diffy::merge(base, local, remote);
  match merge {
    Ok(merged) => MergeResult::Merged(merged),
    Err(conflict) => {
      // Проверяем наличие маркеров конфликта
      if conflict.contains("<<<<<<<") || conflict.contains(">>>>>>>") {
        MergeResult::Failed {
          local: local.to_string(),
          remote: remote.to_string()
        }
      } else {
        // Нет маркеров — чистый merge
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
      "пустой base + разный контент → Failed, got {result:?}"
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
      "пустой local + изменённый remote → Failed, got {result:?}"
    );
  }
}
