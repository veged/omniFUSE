//! S3 synchronization session.

use std::{
  collections::BTreeMap,
  path::{Path, PathBuf},
  sync::{Arc, PoisonError, RwLock}
};

use omnifuse_core::{
  CacheKey, FilesystemCache, InitResult, InstanceHash, PersistentCache, RemoteApplyMode, RemoteDeferReason,
  RemoteRefresh, RemoteRefreshResult, SyncResult, TextMergeDecision, decide_text_merge, decode_utf8_text
};
use opendal::{EntryMode, Metadata, Operator};
use tracing::warn;

use crate::{
  S3Config, S3Error, classify_s3_error,
  manifest::{ManifestStore, ObjectState, S3Manifest},
  operator::build_operator,
  path::ObjectPath
};

const MAX_PUT_RETRIES: u32 = 3;

/// S3 sync session attached to one local directory.
pub struct S3Session {
  operator: Operator,
  local_dir: PathBuf,
  manifest_store: ManifestStore,
  manifest: RwLock<S3Manifest>,
  instance: InstanceHash,
  cache: Option<Arc<FilesystemCache>>
}

impl S3Session {
  /// Attach session using S3 config.
  ///
  /// # Errors
  ///
  /// Returns an error if the operator cannot be built, required capabilities are missing,
  /// or local manifest storage cannot be prepared.
  pub fn attach(config: &S3Config, local_dir: &Path, cache: Option<Arc<FilesystemCache>>) -> anyhow::Result<Self> {
    let operator = build_operator(config)?;
    validate_required_capabilities(&operator)?;
    Self::attach_unchecked(operator, local_dir, config.instance_hash(), cache)
  }

  /// Attach session using an existing operator without enforcing the capability gate.
  ///
  /// Unit tests against the OpenDAL memory service rely on this entry point because the
  /// memory backend deliberately does not honor `if_match` semantics; the live MinIO matrix
  /// in Task 9 covers conditional-write behavior on a real provider.
  ///
  /// # Errors
  ///
  /// Returns an error if local manifest storage cannot be prepared.
  #[cfg(any(test, feature = "test-utils"))]
  pub fn attach_operator_for_tests(operator: Operator, local_dir: &Path) -> anyhow::Result<Self> {
    Self::attach_unchecked(operator, local_dir, InstanceHash::from_parts(&["s3", "test"]), None)
  }

  /// Attach session for tests, wiring an explicit persistent cache.
  ///
  /// # Errors
  ///
  /// Returns an error if local manifest storage cannot be prepared.
  #[cfg(any(test, feature = "test-utils"))]
  pub fn attach_operator_with_cache_for_tests(
    operator: Operator,
    local_dir: &Path,
    instance: InstanceHash,
    cache: Option<Arc<FilesystemCache>>
  ) -> anyhow::Result<Self> {
    Self::attach_unchecked(operator, local_dir, instance, cache)
  }

  fn attach_unchecked(
    operator: Operator,
    local_dir: &Path,
    instance: InstanceHash,
    cache: Option<Arc<FilesystemCache>>
  ) -> anyhow::Result<Self> {
    let local_dir = local_dir.canonicalize().unwrap_or_else(|_| local_dir.to_path_buf());
    std::fs::create_dir_all(&local_dir)?;
    let manifest_store = ManifestStore::new(&local_dir)?;
    let manifest = manifest_store.load()?;
    Ok(Self {
      operator,
      local_dir,
      manifest_store,
      manifest: RwLock::new(manifest),
      instance,
      cache
    })
  }

  /// Borrow the underlying operator.
  #[must_use]
  pub const fn operator(&self) -> &Operator {
    &self.operator
  }

  /// Borrow the local directory root.
  #[must_use]
  pub fn local_dir(&self) -> &Path {
    &self.local_dir
  }

  /// Borrow the manifest store.
  #[must_use]
  pub const fn manifest_store(&self) -> &ManifestStore {
    &self.manifest_store
  }

  /// Read an object's bytes, going through the persistent cache when an ETag is available.
  ///
  /// On miss the bytes are fetched once and stored in the cache for the next mount.
  ///
  /// # Errors
  ///
  /// Propagates any error from the underlying operator read.
  pub async fn read_object_cached(&self, object_path: &str, etag: Option<&str>) -> anyhow::Result<Vec<u8>> {
    if let (Some(cache), Some(etag)) = (self.cache.as_ref(), etag) {
      let key = CacheKey {
        instance: &self.instance,
        path: object_path,
        version: etag
      };
      if let Some(bytes) = cache.get(key).await {
        return Ok(bytes);
      }
      let bytes = self.operator.read(object_path).await?.to_vec();
      if let Err(error) = cache.put(key, bytes.clone()).await {
        warn!(error = %error, object_path, "cache put after remote read failed");
      }
      return Ok(bytes);
    }
    Ok(self.operator.read(object_path).await?.to_vec())
  }

  fn cache_after_upload(&self, object_path: &str, etag: Option<&str>, bytes: &[u8]) {
    if let (Some(cache), Some(etag)) = (self.cache.as_ref(), etag) {
      let cache = Arc::clone(cache);
      let instance = self.instance.clone();
      let object_path = object_path.to_string();
      let etag = etag.to_string();
      let bytes = bytes.to_vec();
      tokio::spawn(async move {
        let key = CacheKey {
          instance: &instance,
          path: &object_path,
          version: &etag
        };
        if let Err(error) = cache.put(key, bytes).await {
          warn!(error = %error, path = %object_path, "cache put after upload failed");
        }
      });
    }
  }

  pub(crate) fn manifest_entry(&self, object_path: &str) -> Option<ObjectState> {
    self
      .manifest
      .read()
      .unwrap_or_else(PoisonError::into_inner)
      .entries
      .get(object_path)
      .cloned()
  }

  pub(crate) fn update_manifest(&self, object_path: &str, state: Option<ObjectState>) -> anyhow::Result<()> {
    let mut manifest = self.manifest.write().unwrap_or_else(PoisonError::into_inner);
    match state {
      Some(state) => {
        manifest.entries.insert(object_path.to_string(), state);
      }
      None => {
        manifest.entries.remove(object_path);
      }
    }
    self.manifest_store.save(&manifest)
  }

  pub(crate) fn replace_manifest(&self, manifest: S3Manifest) -> anyhow::Result<()> {
    self.manifest_store.save(&manifest)?;
    *self.manifest.write().unwrap_or_else(PoisonError::into_inner) = manifest;
    Ok(())
  }

  /// Initialize local files from remote S3 objects.
  ///
  /// # Errors
  ///
  /// Returns an error if remote state cannot be read at first init or local files cannot be
  /// materialized. When the manifest is non-empty and remote is unreachable, returns
  /// [`InitResult::Offline`] instead of failing.
  pub async fn initialize(&self) -> anyhow::Result<InitResult> {
    let remote = match self.remote_entries().await {
      Ok(remote) => remote,
      Err(error) => {
        warn!(error = %error, "s3 remote unavailable during init");
        if self
          .manifest
          .read()
          .unwrap_or_else(PoisonError::into_inner)
          .entries
          .is_empty()
        {
          return Err(error);
        }
        return Ok(InitResult::Offline);
      }
    };

    let had_manifest = !self
      .manifest
      .read()
      .unwrap_or_else(PoisonError::into_inner)
      .entries
      .is_empty();
    let changed = self.materialize_remote(remote).await?;
    if !had_manifest && changed > 0 {
      Ok(InitResult::Fresh)
    } else if changed == 0 {
      Ok(InitResult::UpToDate)
    } else {
      Ok(InitResult::Updated)
    }
  }

  pub(crate) async fn remote_entries(&self) -> anyhow::Result<BTreeMap<String, ObjectState>> {
    let mut entries = BTreeMap::new();
    let listing = self.operator.list_with("").recursive(true).await?;
    for entry in listing {
      if entry.metadata().mode() != EntryMode::FILE {
        continue;
      }
      let object_path = entry.path().trim_end_matches('/').to_string();
      if object_path.is_empty() {
        continue;
      }
      let object_path = ObjectPath::from_remote(&object_path)?;
      entries.insert(
        object_path.as_str().to_string(),
        object_state(object_path.as_str(), entry.metadata())
      );
    }
    Ok(entries)
  }

  async fn materialize_remote(&self, remote: BTreeMap<String, ObjectState>) -> anyhow::Result<usize> {
    let mut changed = 0;
    let mut manifest = S3Manifest::empty();

    for (object_path, state) in remote {
      let object_path = ObjectPath::from_remote(&object_path)?;
      let local_path = self.local_dir.join(object_path.to_local_path());
      if let Some(parent) = local_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
      }

      let bytes = self
        .read_object_cached(object_path.as_str(), state.etag.as_deref())
        .await?;
      let needs_write = local_bytes_differ(&local_path, &bytes).await?;
      if needs_write {
        tokio::fs::write(&local_path, &bytes).await?;
        changed += 1;
      }

      self.manifest_store.save_base(object_path.as_str(), &bytes)?;
      manifest.entries.insert(object_path.as_str().to_string(), state);
    }

    self.replace_manifest(manifest)?;
    Ok(changed)
  }
}

impl S3Session {
  /// Synchronize locally dirty files to S3.
  ///
  /// # Errors
  ///
  /// Returns an error for non-conflict, non-offline failures (IO, capability gaps, etc.).
  pub async fn sync_dirty(&self, dirty_files: &[PathBuf]) -> anyhow::Result<SyncResult> {
    let mut synced = 0;
    let mut conflicts = Vec::new();

    for path in dirty_files {
      match self.sync_one(path).await {
        Ok(true) => synced += 1,
        Ok(false) => {}
        Err(error) if classify_s3_error(&error) == omnifuse_core::Code::Conflict => {
          conflicts.push(path.clone());
        }
        Err(error) if classify_s3_error(&error) == omnifuse_core::Code::Offline => {
          return Ok(SyncResult::Offline);
        }
        Err(error) => return Err(error)
      }
    }

    if conflicts.is_empty() {
      Ok(SyncResult::Success { synced_files: synced })
    } else {
      Ok(SyncResult::Conflict {
        synced_files: synced,
        conflict_files: conflicts
      })
    }
  }

  async fn sync_one(&self, path: &Path) -> anyhow::Result<bool> {
    let object_path = ObjectPath::from_local(path)?;
    let local_path = self.local_dir.join(path);
    if is_local_dir(&local_path).await? {
      return Ok(false);
    }

    if local_path_exists(&local_path).await? {
      self.upload_or_merge(path, object_path.as_str(), &local_path).await?;
    } else {
      self.delete_remote(path, object_path.as_str()).await?;
    }

    Ok(true)
  }

  async fn upload_or_merge(&self, local_rel: &Path, object_path: &str, local_path: &Path) -> anyhow::Result<()> {
    for attempt in 0..MAX_PUT_RETRIES {
      let outcome = self.upload_or_merge_once(local_rel, object_path, local_path).await;
      match outcome {
        Err(error) if is_precondition_failed(&error) && attempt + 1 < MAX_PUT_RETRIES => {}
        other => return other
      }
    }

    Err(
      S3Error::Conflict {
        path: local_rel.to_path_buf()
      }
      .into()
    )
  }

  async fn upload_or_merge_once(&self, local_rel: &Path, object_path: &str, local_path: &Path) -> anyhow::Result<()> {
    let local_bytes = tokio::fs::read(local_path).await?;
    let manifest_state = self.manifest_entry(object_path);
    let remote_state = self.stat_object(object_path).await?;

    match (manifest_state.as_ref(), remote_state.as_ref()) {
      (Some(base_state), Some(current_state)) if !base_state.same_generation(current_state) => {
        self
          .merge_remote_drift(local_rel, object_path, &local_bytes, current_state)
          .await
      }
      (Some(_base_state), None) => {
        self
          .handle_remote_delete_during_upload(local_rel, object_path, &local_bytes)
          .await
      }
      (None, Some(remote_meta)) => {
        let remote_bytes = self
          .read_object_cached(object_path, remote_meta.etag.as_deref())
          .await?;
        if remote_bytes == local_bytes {
          let meta = self.operator.stat(object_path).await?;
          self.save_synced_state(object_path, object_state(object_path, &meta), &local_bytes)
        } else {
          Err(
            S3Error::Conflict {
              path: local_rel.to_path_buf()
            }
            .into()
          )
        }
      }
      (None, None) => self.put_create_and_record(local_rel, object_path, &local_bytes).await,
      (Some(base_state), Some(_)) => {
        self
          .put_update_and_record(local_rel, object_path, base_state.required_etag()?, &local_bytes)
          .await
      }
    }
  }

  async fn merge_remote_drift(
    &self,
    local_rel: &Path,
    object_path: &str,
    local_bytes: &[u8],
    remote_state: &ObjectState
  ) -> anyhow::Result<()> {
    let base_bytes = self.manifest_store.load_base(object_path).unwrap_or_default();
    let remote_bytes = self
      .read_object_cached(object_path, remote_state.etag.as_deref())
      .await?;

    let Some(base_text) = decode_utf8_text(&base_bytes) else {
      return Err(
        S3Error::Conflict {
          path: local_rel.to_path_buf()
        }
        .into()
      );
    };
    let Some(local_text) = decode_utf8_text(local_bytes) else {
      return Err(
        S3Error::Conflict {
          path: local_rel.to_path_buf()
        }
        .into()
      );
    };
    let Some(remote_text) = decode_utf8_text(&remote_bytes) else {
      return Err(
        S3Error::Conflict {
          path: local_rel.to_path_buf()
        }
        .into()
      );
    };

    match decide_text_merge(base_text, local_text, remote_text) {
      TextMergeDecision::UploadLocal => {
        self
          .put_update_and_record(local_rel, object_path, remote_state.required_etag()?, local_bytes)
          .await
      }
      TextMergeDecision::UploadMerged(merged) => {
        let merged_bytes = merged.into_bytes();
        tokio::fs::write(self.local_dir.join(local_rel), &merged_bytes).await?;
        self
          .put_update_and_record(local_rel, object_path, remote_state.required_etag()?, &merged_bytes)
          .await
      }
      TextMergeDecision::AcceptRemote(remote) => {
        let bytes = remote.into_bytes();
        tokio::fs::write(self.local_dir.join(local_rel), &bytes).await?;
        self.save_synced_state(object_path, remote_state.clone(), &bytes)
      }
      TextMergeDecision::AlreadySynced => self.save_synced_state(object_path, remote_state.clone(), local_bytes),
      TextMergeDecision::Conflict => Err(
        S3Error::Conflict {
          path: local_rel.to_path_buf()
        }
        .into()
      )
    }
  }

  async fn handle_remote_delete_during_upload(
    &self,
    local_rel: &Path,
    object_path: &str,
    local_bytes: &[u8]
  ) -> anyhow::Result<()> {
    let base_bytes = self.manifest_store.load_base(object_path).unwrap_or_default();
    if base_bytes == local_bytes {
      let local_path = self.local_dir.join(local_rel);
      let _ = tokio::fs::remove_file(local_path).await;
      self.update_manifest(object_path, None)?;
      self.manifest_store.remove_base(object_path);
      Ok(())
    } else {
      Err(
        S3Error::Conflict {
          path: local_rel.to_path_buf()
        }
        .into()
      )
    }
  }

  async fn put_create_and_record(&self, local_rel: &Path, object_path: &str, bytes: &[u8]) -> anyhow::Result<()> {
    let metadata = self
      .operator
      .write_with(object_path, bytes.to_vec())
      .if_not_exists(true)
      .await
      .map_err(|error| precondition_or_opendal(local_rel, error))?;
    let state = object_state(object_path, &metadata);
    self.cache_after_upload(object_path, state.etag.as_deref(), bytes);
    self.save_synced_state(object_path, state, bytes)
  }

  async fn put_update_and_record(
    &self,
    local_rel: &Path,
    object_path: &str,
    etag: &str,
    bytes: &[u8]
  ) -> anyhow::Result<()> {
    let metadata = self
      .operator
      .write_with(object_path, bytes.to_vec())
      .if_match(etag)
      .await
      .map_err(|error| precondition_or_opendal(local_rel, error))?;
    let state = object_state(object_path, &metadata);
    self.cache_after_upload(object_path, state.etag.as_deref(), bytes);
    self.save_synced_state(object_path, state, bytes)
  }

  async fn delete_remote(&self, local_rel: &Path, object_path: &str) -> anyhow::Result<()> {
    let manifest_state = self.manifest_entry(object_path);
    if manifest_state.is_none() {
      return Ok(());
    }

    let remote_state = self.stat_object(object_path).await?;
    if remote_state.as_ref() != manifest_state.as_ref() {
      return Err(
        S3Error::Conflict {
          path: local_rel.to_path_buf()
        }
        .into()
      );
    }

    self.operator.delete(object_path).await?;
    self.update_manifest(object_path, None)?;
    self.manifest_store.remove_base(object_path);
    Ok(())
  }

  pub(crate) async fn stat_object(&self, object_path: &str) -> anyhow::Result<Option<ObjectState>> {
    match self.operator.stat(object_path).await {
      Ok(meta) => Ok(Some(object_state(object_path, &meta))),
      Err(error) if error.kind() == opendal::ErrorKind::NotFound => Ok(None),
      Err(error) => Err(error.into())
    }
  }

  pub(crate) fn save_synced_state(&self, object_path: &str, state: ObjectState, bytes: &[u8]) -> anyhow::Result<()> {
    self.manifest_store.save_base(object_path, bytes)?;
    self.update_manifest(object_path, Some(state))
  }
}

#[derive(Debug, Clone, Eq, PartialEq)]
enum RemoteChange {
  Modified { object_path: String },
  Deleted { object_path: String }
}

impl RemoteChange {
  fn local_path(&self) -> PathBuf {
    match self {
      Self::Modified { object_path } | Self::Deleted { object_path } => PathBuf::from(object_path)
    }
  }
}

#[derive(Debug)]
enum ChangeDecision {
  Apply(ApplyDecision),
  Conflict(PathBuf)
}

#[derive(Debug)]
enum ApplyDecision {
  /// Write remote bytes to local, set base = remote, refresh manifest.
  WriteRemote {
    local_rel: PathBuf,
    object_path: String,
    bytes: Vec<u8>,
    state: ObjectState
  },
  /// Write merged bytes to local, set base = remote bytes, refresh manifest. Next sync uploads.
  WriteMerged {
    local_rel: PathBuf,
    object_path: String,
    merged: Vec<u8>,
    base_bytes: Vec<u8>,
    state: ObjectState
  },
  /// Remote metadata changed but local content already reflects it; refresh manifest only.
  RefreshState {
    local_rel: PathBuf,
    object_path: String,
    state: ObjectState
  },
  /// Remove local file, drop manifest entry and base.
  DeleteLocal { local_rel: PathBuf, object_path: String }
}

impl S3Session {
  /// Detect remote changes and apply safe updates locally.
  ///
  /// # Errors
  ///
  /// Returns an error if remote listing or reads fail.
  pub async fn refresh_remote(&self, request: RemoteRefresh<'_>) -> anyhow::Result<RemoteRefreshResult> {
    let remote = self.remote_entries().await?;
    let changes = self.diff_remote(&remote);
    if changes.is_empty() {
      return Ok(RemoteRefreshResult::Unchanged);
    }

    let affected = changes.iter().map(RemoteChange::local_path).collect::<Vec<_>>();
    let protected = affected
      .iter()
      .filter(|path| request.protected_paths.is_protected(path))
      .cloned()
      .collect::<Vec<_>>();

    if !protected.is_empty() {
      return Ok(RemoteRefreshResult::Deferred {
        affected: protected,
        reason: RemoteDeferReason::ProtectedLocalChange
      });
    }

    if matches!(request.mode, RemoteApplyMode::DetectOnly) {
      return Ok(RemoteRefreshResult::Deferred {
        affected,
        reason: RemoteDeferReason::DetectOnly
      });
    }

    self.apply_remote_changes(changes, remote).await
  }

  fn diff_remote(&self, remote: &BTreeMap<String, ObjectState>) -> Vec<RemoteChange> {
    let manifest = self.manifest.read().unwrap_or_else(PoisonError::into_inner);
    let mut changes = Vec::new();

    for (object_path, state) in remote {
      let unchanged = manifest
        .entries
        .get(object_path)
        .is_some_and(|base| base.same_generation(state));
      if !unchanged {
        changes.push(RemoteChange::Modified {
          object_path: object_path.clone()
        });
      }
    }

    for object_path in manifest.entries.keys() {
      if !remote.contains_key(object_path) {
        changes.push(RemoteChange::Deleted {
          object_path: object_path.clone()
        });
      }
    }

    changes
  }

  async fn apply_remote_changes(
    &self,
    changes: Vec<RemoteChange>,
    remote: BTreeMap<String, ObjectState>
  ) -> anyhow::Result<RemoteRefreshResult> {
    let mut decisions = Vec::with_capacity(changes.len());
    let mut conflicts = Vec::new();

    for change in &changes {
      match self.classify_remote_change(change, &remote).await? {
        ChangeDecision::Apply(decision) => decisions.push(decision),
        ChangeDecision::Conflict(path) => conflicts.push(path)
      }
    }

    if !conflicts.is_empty() {
      return Ok(RemoteRefreshResult::Deferred {
        affected: conflicts,
        reason: RemoteDeferReason::Conflict
      });
    }

    let mut changed = Vec::new();
    let mut deleted = Vec::new();

    for decision in decisions {
      match decision {
        ApplyDecision::WriteRemote {
          local_rel,
          object_path,
          bytes,
          state
        } => {
          self.write_local(&local_rel, &bytes).await?;
          self.save_synced_state(&object_path, state, &bytes)?;
          changed.push(local_rel);
        }
        ApplyDecision::WriteMerged {
          local_rel,
          object_path,
          merged,
          base_bytes,
          state
        } => {
          self.write_local(&local_rel, &merged).await?;
          self.manifest_store.save_base(&object_path, &base_bytes)?;
          self.update_manifest(&object_path, Some(state))?;
          changed.push(local_rel);
        }
        ApplyDecision::RefreshState {
          local_rel,
          object_path,
          state
        } => {
          self.update_manifest(&object_path, Some(state))?;
          changed.push(local_rel);
        }
        ApplyDecision::DeleteLocal { local_rel, object_path } => {
          let _ = tokio::fs::remove_file(self.local_dir.join(&local_rel)).await;
          self.update_manifest(&object_path, None)?;
          self.manifest_store.remove_base(&object_path);
          deleted.push(local_rel);
        }
      }
    }

    Ok(RemoteRefreshResult::Applied { changed, deleted })
  }

  async fn classify_remote_change(
    &self,
    change: &RemoteChange,
    remote: &BTreeMap<String, ObjectState>
  ) -> anyhow::Result<ChangeDecision> {
    match change {
      RemoteChange::Modified { object_path } => {
        let local_rel = PathBuf::from(object_path);
        let local_path = self.local_dir.join(&local_rel);
        let remote_state = remote.get(object_path).cloned().ok_or_else(|| S3Error::Conflict {
          path: local_rel.clone()
        })?;
        let remote_bytes = self
          .read_object_cached(object_path, remote_state.etag.as_deref())
          .await?;

        if !local_path_exists(&local_path).await? || !self.local_changed_from_base(object_path, &local_path).await {
          return Ok(ChangeDecision::Apply(ApplyDecision::WriteRemote {
            local_rel,
            object_path: object_path.clone(),
            bytes: remote_bytes,
            state: remote_state
          }));
        }

        let local_bytes = tokio::fs::read(&local_path).await?;
        let base_bytes = self.manifest_store.load_base(object_path).unwrap_or_default();
        let (Some(base_text), Some(local_text), Some(remote_text)) = (
          decode_utf8_text(&base_bytes),
          decode_utf8_text(&local_bytes),
          decode_utf8_text(&remote_bytes)
        ) else {
          return Ok(ChangeDecision::Conflict(local_rel));
        };

        match decide_text_merge(base_text, local_text, remote_text) {
          TextMergeDecision::UploadLocal => Ok(ChangeDecision::Apply(ApplyDecision::RefreshState {
            local_rel,
            object_path: object_path.clone(),
            state: remote_state
          })),
          TextMergeDecision::UploadMerged(merged) => Ok(ChangeDecision::Apply(ApplyDecision::WriteMerged {
            local_rel,
            object_path: object_path.clone(),
            merged: merged.into_bytes(),
            base_bytes: remote_bytes,
            state: remote_state
          })),
          TextMergeDecision::AcceptRemote(remote_text) => Ok(ChangeDecision::Apply(ApplyDecision::WriteRemote {
            local_rel,
            object_path: object_path.clone(),
            bytes: remote_text.into_bytes(),
            state: remote_state
          })),
          TextMergeDecision::AlreadySynced => Ok(ChangeDecision::Apply(ApplyDecision::RefreshState {
            local_rel,
            object_path: object_path.clone(),
            state: remote_state
          })),
          TextMergeDecision::Conflict => Ok(ChangeDecision::Conflict(local_rel))
        }
      }
      RemoteChange::Deleted { object_path } => {
        let local_rel = PathBuf::from(object_path);
        let local_path = self.local_dir.join(&local_rel);
        if local_path_exists(&local_path).await? && self.local_changed_from_base(object_path, &local_path).await {
          Ok(ChangeDecision::Conflict(local_rel))
        } else {
          Ok(ChangeDecision::Apply(ApplyDecision::DeleteLocal {
            local_rel,
            object_path: object_path.clone()
          }))
        }
      }
    }
  }

  async fn write_local(&self, local_rel: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let local_path = self.local_dir.join(local_rel);
    if let Some(parent) = local_path.parent() {
      tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(local_path, bytes).await?;
    Ok(())
  }

  async fn local_changed_from_base(&self, object_path: &str, local_path: &Path) -> bool {
    let Some(base) = self.manifest_store.load_base(object_path) else {
      return true;
    };
    tokio::fs::read(local_path).await.map_or(true, |local| local != base)
  }
}

async fn local_path_exists(path: &Path) -> anyhow::Result<bool> {
  match tokio::fs::metadata(path).await {
    Ok(_) => Ok(true),
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
    Err(error) => Err(error.into())
  }
}

async fn is_local_dir(path: &Path) -> anyhow::Result<bool> {
  match tokio::fs::metadata(path).await {
    Ok(metadata) => Ok(metadata.is_dir()),
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
    Err(error) => Err(error.into())
  }
}

async fn local_bytes_differ(path: &Path, bytes: &[u8]) -> anyhow::Result<bool> {
  match tokio::fs::read(path).await {
    Ok(existing) => Ok(existing != bytes),
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
    Err(error) => Err(error.into())
  }
}

fn is_precondition_failed(error: &anyhow::Error) -> bool {
  matches!(
    error.downcast_ref::<S3Error>(),
    Some(S3Error::PreconditionFailed { .. })
  )
}

fn precondition_or_opendal(local_rel: &Path, error: opendal::Error) -> S3Error {
  if error.kind() == opendal::ErrorKind::ConditionNotMatch {
    S3Error::PreconditionFailed {
      path: local_rel.to_path_buf()
    }
  } else {
    S3Error::OpenDal(error)
  }
}

pub(crate) fn object_state(path: &str, meta: &Metadata) -> ObjectState {
  ObjectState {
    object_path: path.to_string(),
    etag: meta.etag().map(ToString::to_string),
    version: meta.version().map(ToString::to_string),
    last_modified: meta.last_modified().map(|value| value.to_string()),
    content_length: meta.content_length()
  }
}

fn validate_required_capabilities(operator: &Operator) -> Result<(), S3Error> {
  let capability = operator.info().full_capability();
  let required = [
    (capability.stat, "stat"),
    (capability.read, "read"),
    (capability.write, "write"),
    (capability.delete, "delete"),
    (capability.list, "list"),
    (capability.list_with_recursive, "list_with_recursive"),
    (capability.write_with_if_match, "write_with_if_match"),
    (capability.write_with_if_not_exists, "write_with_if_not_exists"),
    // ETag is the drift fingerprint and the `if_match` token. OpenDAL 0.56 does not expose
    // a direct `stat_has_etag` capability bit; `stat_with_if_match` is the closest proxy
    // (a service that supports conditional stat must surface ETag). At runtime
    // `ObjectState::required_etag` catches any provider that still drops the ETag.
    (capability.stat_with_if_match, "stat_with_if_match")
  ];

  for (enabled, name) in required {
    if !enabled {
      return Err(S3Error::MissingCapability(name));
    }
  }

  Ok(())
}
