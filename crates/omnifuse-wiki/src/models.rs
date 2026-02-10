//! Wiki API models (serde).

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Page fields (detailed).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageFullDetailsSchema {
  /// Page ID.
  pub id: u64,
  /// Title.
  pub title: String,
  /// Slug.
  pub slug: String,
  /// Page type.
  pub page_type: String,
  /// Content (markdown), if requested.
  pub content: Option<String>,
  /// Last modification time (ISO 8601).
  #[serde(default)]
  pub modified_at: String
}

/// Page fields (brief).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageSchema {
  /// ID.
  pub id: u64,
  /// Slug.
  pub slug: String
}

/// Page update.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageUpdateSchema {
  /// Title (optional).
  pub title: Option<String>,
  /// Content (optional).
  pub content: Option<String>
}

/// Page creation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreatePageSchema {
  /// Page type.
  pub page_type: String,
  /// Title.
  pub title: String,
  /// Slug.
  pub slug: String,
  /// Content (optional).
  pub content: Option<String>
}

/// Deletion response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeleteResponse {
  /// Recovery token.
  pub recovery_token: String
}

/// API error.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorResponse {
  /// Error code.
  pub error_code: String,
  /// Diagnostic message.
  pub debug_message: String,
  /// Additional fields (if any).
  pub details: Option<Value>
}

/// Paginated collection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Collection<T> {
  /// Items.
  pub results: Vec<T>,
  /// Next page cursor.
  pub next_cursor: Option<String>,
  /// Whether there is a next page.
  pub has_next: bool,
  /// Metadata.
  pub metadata: Option<Map<String, Value>>
}

/// Page tree node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageTreeNodeSchema {
  /// ID.
  pub id: u64,
  /// Slug.
  pub slug: String,
  /// Title.
  pub title: String,
  /// Modification time (API string).
  pub modified_at: String,
  /// Children.
  pub children: Option<Vec<Self>>
}

/// Page tree response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageTreeResponseSchema {
  /// Tree root.
  pub root: PageTreeNodeSchema
}

/// Operation identity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationIdentity {
  /// Operation type.
  #[serde(rename = "type")]
  pub ty: String,
  /// Operation ID.
  pub id: String
}

/// Operation created response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationCreatedSchema {
  /// Operation ID.
  pub operation: OperationIdentity,
  /// Status URL (if available).
  pub status_url: Option<String>
}

/// Move item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MoveCluster {
  /// Source slug.
  pub source: String,
  /// Target slug.
  pub target: String
}

/// Move request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MoveClusterRequest {
  /// Operations.
  pub operations: Vec<MoveCluster>,
  /// Copy inherited access rights.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub copy_inherited_access: Option<bool>,
  /// Check inheritance.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub check_inheritance: Option<bool>
}

/// Async operation status.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
  /// Scheduled.
  Scheduled,
  /// In progress.
  InProgress,
  /// Success.
  Success,
  /// Failed.
  Failed
}

/// Async operation status response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AsyncOperationStatusSchema {
  /// Status.
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
