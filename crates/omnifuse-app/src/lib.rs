//! Application-level mount preparation for `OmniFuse`.

// Test support lives beside the production environment abstraction; allow
// fixture-only visibility/layout lints in test builds.
#![cfg_attr(test, allow(clippy::items_after_test_module, clippy::redundant_pub_crate))]

pub mod daemon;
mod environment;
mod mount_layout;
mod mount_service;

pub use environment::{MountEnvironment, StdMountEnvironment};
pub use mount_layout::{CacheKey, MountLayout};
pub use mount_service::{GitMountArgs, MountDefaults, MountService, PreparedMount, S3MountArgs, WikiMountArgs};
