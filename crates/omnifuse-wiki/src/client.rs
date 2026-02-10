//! Wiki API HTTP client.
//!
//! Ported from `YaWikiFS` `src/wiki/client.rs`.
//! `WikiErr` -> `anyhow::Error`.

use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use tracing::{debug, error, trace};

use crate::models::{
  AsyncOperationStatusSchema, Collection, CreatePageSchema, ErrorResponse, MoveCluster,
  MoveClusterRequest, OperationCreatedSchema, PageFullDetailsSchema, PageSchema,
  PageTreeResponseSchema, PageUpdateSchema, Status
};

/// Wiki API HTTP client.
pub struct Client {
  /// reqwest HTTP client.
  c: reqwest::Client,
  /// Base URL (without trailing `/`).
  base: String
}

impl Client {
  /// Creates a Wiki API client.
  ///
  /// # Errors
  ///
  /// Returns an error if the input parameters are empty or the HTTP client cannot be built.
  pub fn new(base_url: &str, auth_token: &str) -> anyhow::Result<Self> {
    if base_url.trim().is_empty() {
      anyhow::bail!("base_url must not be empty");
    }
    if auth_token.trim().is_empty() {
      anyhow::bail!("auth_token must not be empty");
    }

    let mut h = HeaderMap::new();
    h.insert(
      AUTHORIZATION,
      HeaderValue::from_str(&format!("Bearer {auth_token}"))
        .map_err(|e| anyhow::anyhow!("invalid auth_token: {e}"))?
    );
    h.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

    Ok(Self {
      c: reqwest::Client::builder()
        .default_headers(h)
        .no_proxy()
        .build()?,
      base: base_url.trim_end_matches('/').to_string()
    })
  }

  /// Reads a page by slug (with `content`).
  ///
  /// # Errors
  ///
  /// Returns a network/HTTP/deserialization error.
  pub async fn get_page_by_slug(&self, slug: &str) -> anyhow::Result<PageFullDetailsSchema> {
    self
      .get_json(
        self
          .c
          .get(format!("{}/api/v2/public/pages", self.base))
          .query(&[("slug", slug), ("fields", "content")])
      )
      .await
  }

  /// Reads a page by id (with `content`).
  ///
  /// # Errors
  ///
  /// Returns a network/HTTP/deserialization error.
  pub async fn get_page_by_idx(&self, idx: u64) -> anyhow::Result<PageFullDetailsSchema> {
    self
      .get_json(
        self
          .c
          .get(format!("{}/api/v2/public/pages/{idx}", self.base))
          .query(&[("fields", "content")])
      )
      .await
  }

  /// Updates a page.
  ///
  /// # Errors
  ///
  /// Returns a network/HTTP/deserialization error.
  pub async fn update_page(
    &self,
    idx: u64,
    title: Option<&str>,
    content: Option<&str>,
    allow_merge: bool
  ) -> anyhow::Result<PageFullDetailsSchema> {
    self
      .get_json(
        self
          .c
          .post(format!("{}/api/v2/public/pages/{idx}", self.base))
          .query(&[("allow_merge", allow_merge)])
          .json(&PageUpdateSchema {
            title: title.map(str::to_string),
            content: content.map(str::to_string)
          })
      )
      .await
  }

  /// Creates a page.
  ///
  /// # Errors
  ///
  /// Returns a network/HTTP/deserialization error.
  pub async fn create_page(
    &self,
    slug: &str,
    title: &str,
    content: Option<&str>,
    page_type: &str
  ) -> anyhow::Result<PageFullDetailsSchema> {
    self
      .get_json(
        self
          .c
          .post(format!("{}/api/v2/public/pages", self.base))
          .json(&CreatePageSchema {
            page_type: page_type.to_string(),
            title: title.to_string(),
            slug: slug.to_string(),
            content: content.map(str::to_string)
          })
      )
      .await
  }

  /// Deletes a page.
  ///
  /// # Errors
  ///
  /// Returns a network/HTTP error.
  pub async fn delete_page(&self, idx: u64) -> anyhow::Result<()> {
    self
      .send_ok(
        self
          .c
          .delete(format!("{}/api/v2/public/pages/{idx}", self.base))
      )
      .await
  }

  /// Returns the list of page descendants (with pagination).
  ///
  /// # Errors
  ///
  /// Returns a network/HTTP/deserialization error.
  pub async fn get_descendants(&self, idx: u64) -> anyhow::Result<Vec<PageSchema>> {
    let mut out = vec![];
    let mut cursor: Option<String> = None;

    loop {
      let mut r = self
        .c
        .get(format!(
          "{}/api/v2/public/pages/{idx}/descendants",
          self.base
        ))
        .query(&[("page_size", 100u32)]);

      if let Some(c) = cursor.as_deref() {
        r = r.query(&[("cursor", c)]);
      }

      let p: Collection<PageSchema> = self.get_json(r).await?;
      out.extend(p.results);

      if !p.has_next || p.next_cursor.is_none() {
        break;
      }
      cursor = p.next_cursor;
    }

    Ok(out)
  }

  /// Page tree starting from `slug`.
  ///
  /// # Errors
  ///
  /// Returns a network/HTTP/deserialization error.
  pub async fn get_page_tree(
    &self,
    slug: &str,
    max_pages: u32,
    max_depth: u32
  ) -> anyhow::Result<PageTreeResponseSchema> {
    self
      .get_json(
        self
          .c
          .get(format!("{}/api/v2/public/pages/tree", self.base))
          .query(&[
            ("slug", slug),
            ("order_by", "modified_at"),
            ("max_pages", &max_pages.to_string()),
            ("max_depth", &max_depth.to_string())
          ])
      )
      .await
  }

  /// Moves a subtree from `source` to `target`.
  ///
  /// # Errors
  ///
  /// Returns a network/HTTP/deserialization error.
  pub async fn move_cluster(
    &self,
    source: &str,
    target: &str
  ) -> anyhow::Result<OperationCreatedSchema> {
    self
      .get_json(
        self
          .c
          .post(format!("{}/api/v2/public/pages/move", self.base))
          .json(&MoveClusterRequest {
            operations: vec![MoveCluster {
              source: source.to_string(),
              target: target.to_string()
            }],
            copy_inherited_access: None,
            check_inheritance: None
          })
      )
      .await
  }

  /// Waits for an asynchronous operation to complete.
  ///
  /// # Errors
  ///
  /// Returns a network/HTTP/deserialization error.
  pub async fn poll_status_url(
    &self,
    url: &str,
    timeout: std::time::Duration
  ) -> anyhow::Result<Status> {
    let url = if url.starts_with("http://") || url.starts_with("https://") {
      url.to_string()
    } else if url.starts_with('/') {
      format!("{}{url}", self.base)
    } else {
      format!("{}/{url}", self.base)
    };

    let deadline = std::time::Instant::now() + timeout;

    loop {
      let st: AsyncOperationStatusSchema = self.get_json(self.c.get(&url)).await?;

      match st.status {
        Status::Success | Status::Failed => return Ok(st.status),
        Status::Scheduled | Status::InProgress => {}
      }

      if std::time::Instant::now() >= deadline {
        return Ok(st.status);
      }

      tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
  }

  /// Execute a request and deserialize the JSON response.
  async fn get_json<T: serde::de::DeserializeOwned>(
    &self,
    r: reqwest::RequestBuilder
  ) -> anyhow::Result<T> {
    let rq = r
      .try_clone()
      .ok_or_else(|| anyhow::anyhow!("failed to clone request"))?
      .build()?;

    let start = std::time::Instant::now();
    debug!(method = %rq.method(), url = %rq.url(), "wiki request");

    let resp = self.c.execute(rq).await?;
    let st = resp.status();
    let txt = resp.text().await?;

    debug!(
      status = st.as_u16(),
      ms = start.elapsed().as_millis(),
      bytes = txt.len(),
      "wiki response"
    );

    if tracing::enabled!(tracing::Level::TRACE) {
      let n = 4096usize.min(txt.len());
      trace!(status = st.as_u16(), body = %&txt[..n], "wiki response body");
    }

    if st.is_success() {
      return serde_json::from_str(&txt)
        .map_err(|e| anyhow::anyhow!("deserialization ({st}): {e}"));
    }

    let e: Option<ErrorResponse> = serde_json::from_str(&txt).ok();
    Self::check_error_response(st.as_u16(), e.as_ref(), &txt, start.elapsed().as_millis())
  }

  /// Execute a request and check status (without deserialization).
  async fn send_ok(&self, r: reqwest::RequestBuilder) -> anyhow::Result<()> {
    let rq = r
      .try_clone()
      .ok_or_else(|| anyhow::anyhow!("failed to clone request"))?
      .build()?;

    let start = std::time::Instant::now();
    debug!(method = %rq.method(), url = %rq.url(), "wiki request");

    let resp = self.c.execute(rq).await?;
    let st = resp.status();
    let txt = resp.text().await?;

    debug!(
      status = st.as_u16(),
      ms = start.elapsed().as_millis(),
      bytes = txt.len(),
      "wiki response"
    );

    if st.is_success() {
      return Ok(());
    }

    let e: Option<ErrorResponse> = serde_json::from_str(&txt).ok();
    Self::check_error_response::<()>(st.as_u16(), e.as_ref(), &txt, start.elapsed().as_millis())
  }

  /// Check HTTP error and convert to `anyhow::Error`.
  fn check_error_response<T>(
    status: u16,
    e: Option<&ErrorResponse>,
    body: &str,
    elapsed_ms: u128
  ) -> anyhow::Result<T> {
    let error_code = e.map(|x| x.error_code.as_str());

    error!(
      status,
      error_code,
      ms = elapsed_ms,
      "wiki error"
    );

    if status == 404 {
      anyhow::bail!("page not found");
    }
    if status == 403 {
      anyhow::bail!("access denied");
    }
    if matches!(error_code, Some("CHANGES_CONFLICT")) {
      anyhow::bail!("changes conflict");
    }
    if matches!(error_code, Some("SLUG_OCCUPIED" | "SLUG_RESERVED")) {
      anyhow::bail!("slug is occupied or reserved");
    }

    anyhow::bail!("HTTP {status}: {body}")
  }
}
