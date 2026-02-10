//! Модели Wiki API (serde).

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Поля страницы (подробно).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageFullDetailsSchema {
  /// ID страницы.
  pub id: u64,
  /// Заголовок.
  pub title: String,
  /// Slug.
  pub slug: String,
  /// Тип страницы.
  pub page_type: String,
  /// Контент (markdown), если запрошен.
  pub content: Option<String>,
  /// Время последнего изменения (ISO 8601).
  #[serde(default)]
  pub modified_at: String
}

/// Поля страницы (кратко).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageSchema {
  /// ID.
  pub id: u64,
  /// Slug.
  pub slug: String
}

/// Обновление страницы.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageUpdateSchema {
  /// Заголовок (опционально).
  pub title: Option<String>,
  /// Контент (опционально).
  pub content: Option<String>
}

/// Создание страницы.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreatePageSchema {
  /// Тип страницы.
  pub page_type: String,
  /// Заголовок.
  pub title: String,
  /// Slug.
  pub slug: String,
  /// Контент (опционально).
  pub content: Option<String>
}

/// Ответ удаления.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeleteResponse {
  /// Токен восстановления.
  pub recovery_token: String
}

/// Ошибка API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorResponse {
  /// Код ошибки.
  pub error_code: String,
  /// Диагностическое сообщение.
  pub debug_message: String,
  /// Доп. поля (если есть).
  pub details: Option<Value>
}

/// Пагинированная коллекция.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Collection<T> {
  /// Элементы.
  pub results: Vec<T>,
  /// Курсор следующей страницы.
  pub next_cursor: Option<String>,
  /// Есть ли следующая страница.
  pub has_next: bool,
  /// Метаданные.
  pub metadata: Option<Map<String, Value>>
}

/// Узел дерева страниц.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageTreeNodeSchema {
  /// ID.
  pub id: u64,
  /// Slug.
  pub slug: String,
  /// Заголовок.
  pub title: String,
  /// Время изменения (строкой API).
  pub modified_at: String,
  /// Дети.
  pub children: Option<Vec<Self>>
}

/// Ответ дерева страниц.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageTreeResponseSchema {
  /// Корень дерева.
  pub root: PageTreeNodeSchema
}

/// Идентификатор операции.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationIdentity {
  /// Тип операции.
  #[serde(rename = "type")]
  pub ty: String,
  /// ID операции.
  pub id: String
}

/// Ответ создания операции.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationCreatedSchema {
  /// ID операции.
  pub operation: OperationIdentity,
  /// URL статуса (если есть).
  pub status_url: Option<String>
}

/// Элемент move.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MoveCluster {
  /// Исходный slug.
  pub source: String,
  /// Целевой slug.
  pub target: String
}

/// Запрос move.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MoveClusterRequest {
  /// Операции.
  pub operations: Vec<MoveCluster>,
  /// Копировать наследуемые права.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub copy_inherited_access: Option<bool>,
  /// Проверять наследование.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub check_inheritance: Option<bool>
}

/// Статус асинхронной операции.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
  /// Запланировано.
  Scheduled,
  /// Выполняется.
  InProgress,
  /// Успех.
  Success,
  /// Ошибка.
  Failed
}

/// Ответ статуса асинхронной операции.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AsyncOperationStatusSchema {
  /// Статус.
  pub status: Status
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
  use super::*;

  #[test]
  fn page_full_details_deserialize() {
    let json =
      r##"{"id":123,"title":"Test","slug":"test/page","page_type":"page","content":"# Hello"}"##;
    let p: PageFullDetailsSchema = serde_json::from_str(json).expect("deserialize");
    assert_eq!(p.id, 123);
    assert_eq!(p.title, "Test");
    assert_eq!(p.slug, "test/page");
    assert_eq!(p.content.as_deref(), Some("# Hello"));
  }

  #[test]
  fn page_full_details_content_optional() {
    let json = r#"{"id":1,"title":"T","slug":"s","page_type":"page"}"#;
    let p: PageFullDetailsSchema = serde_json::from_str(json).expect("deserialize");
    assert!(p.content.is_none());
  }

  #[test]
  fn status_deserialize_snake_case() {
    assert!(matches!(
      serde_json::from_str::<Status>(r#""scheduled""#),
      Ok(Status::Scheduled)
    ));
    assert!(matches!(
      serde_json::from_str::<Status>(r#""in_progress""#),
      Ok(Status::InProgress)
    ));
    assert!(matches!(
      serde_json::from_str::<Status>(r#""success""#),
      Ok(Status::Success)
    ));
    assert!(matches!(
      serde_json::from_str::<Status>(r#""failed""#),
      Ok(Status::Failed)
    ));
  }

  #[test]
  fn error_response_deserialize() {
    let json = r#"{"error_code":"NOT_FOUND","debug_message":"page not found","details":null}"#;
    let e: ErrorResponse = serde_json::from_str(json).expect("deserialize");
    assert_eq!(e.error_code, "NOT_FOUND");
    assert_eq!(e.debug_message, "page not found");
  }

  #[test]
  fn collection_deserialize() {
    let json =
      r#"{"results":[{"id":1,"slug":"a"}],"next_cursor":"c","has_next":true,"metadata":null}"#;
    let c: Collection<PageSchema> = serde_json::from_str(json).expect("deserialize");
    assert_eq!(c.results.len(), 1);
    assert!(c.has_next);
    assert_eq!(c.next_cursor.as_deref(), Some("c"));
  }
}
