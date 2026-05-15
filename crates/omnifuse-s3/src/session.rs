//! S3 synchronization session.

use std::{
  collections::BTreeMap,
  path::{Path, PathBuf},
  sync::{PoisonError, RwLock}
};

use omnifuse_core::{InitResult, SyncResult, TextMergeDecision, decide_text_merge, decode_utf8_text};
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
  manifest: RwLock<S3Manifest>
}

impl S3Session {
  /// Attach session using S3 config.
  ///
  /// # Errors
  ///
  /// Returns an error if the operator cannot be built, required capabilities are missing,
  /// or local manifest storage cannot be prepared.
  pub fn attach(config: &S3Config, local_dir: &Path) -> anyhow::Result<Self> {
    let operator = build_operator(config)?;
    validate_required_capabilities(&operator)?;
    Self::attach_unchecked(operator, local_dir)
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
    Self::attach_unchecked(operator, local_dir)
  }

  fn attach_unchecked(operator: Operator, local_dir: &Path) -> anyhow::Result<Self> {
    let local_dir = local_dir.canonicalize().unwrap_or_else(|_| local_dir.to_path_buf());
    std::fs::create_dir_all(&local_dir)?;
    let manifest_store = ManifestStore::new(&local_dir)?;
    let manifest = manifest_store.load()?;
    Ok(Self {
      operator,
      local_dir,
      manifest_store,
      manifest: RwLock::new(manifest)
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
        std::fs::create_dir_all(parent)?;
      }

      let bytes = self.operator.read(object_path.as_str()).await?.to_vec();
      let needs_write = std::fs::read(&local_path).map_or(true, |existing| existing != bytes);
      if needs_write {
        std::fs::write(&local_path, &bytes)?;
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
        Err(error) if classify_s3_error(&error) == Some(omnifuse_core::Code::Conflict) => {
          conflicts.push(path.clone());
        }
        Err(error) if classify_s3_error(&error) == Some(omnifuse_core::Code::Offline) => {
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
    if local_path.is_dir() {
      return Ok(false);
    }

    if local_path.exists() {
      self.upload_or_merge(path, object_path.as_str(), &local_path).await?;
    } else {
      self.delete_remote(path, object_path.as_str()).await?;
    }

    Ok(true)
  }

  async fn upload_or_merge(&self, local_rel: &Path, object_path: &str, local_path: &Path) -> anyhow::Result<()> {
    for attempt in 0..MAX_PUT_RETRIES {
      match self.upload_or_merge_once(local_rel, object_path, local_path).await {
        Err(error) if is_precondition_failed(&error) && attempt + 1 < MAX_PUT_RETRIES => continue,
        result => return result
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
      (None, Some(_)) => {
        let remote_bytes = self.operator.read(object_path).await?.to_vec();
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
    let remote_bytes = self.operator.read(object_path).await?.to_vec();

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
        std::fs::write(self.local_dir.join(local_rel), &merged_bytes)?;
        self
          .put_update_and_record(local_rel, object_path, remote_state.required_etag()?, &merged_bytes)
          .await
      }
      TextMergeDecision::AcceptRemote(remote) => {
        let bytes = remote.into_bytes();
        std::fs::write(self.local_dir.join(local_rel), &bytes)?;
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
      let _ = std::fs::remove_file(local_path);
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
    self.save_synced_state(object_path, object_state(object_path, &metadata), bytes)
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
    self.save_synced_state(object_path, object_state(object_path, &metadata), bytes)
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
