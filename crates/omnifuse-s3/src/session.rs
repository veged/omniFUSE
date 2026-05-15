//! S3 synchronization session.

use std::{
  collections::BTreeMap,
  path::{Path, PathBuf},
  sync::{PoisonError, RwLock}
};

use omnifuse_core::InitResult;
use opendal::{EntryMode, Metadata, Operator};
use tracing::warn;

use crate::{
  S3Config, S3Error,
  manifest::{ManifestStore, ObjectState, S3Manifest},
  operator::build_operator,
  path::ObjectPath
};

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

  pub(crate) fn manifest_snapshot(&self) -> S3Manifest {
    self.manifest.read().unwrap_or_else(PoisonError::into_inner).clone()
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
