//! Application-level mount preparation for `OmniFuse`.

mod environment;
mod mount_layout;

pub use environment::{MountEnvironment, StdMountEnvironment};
pub use mount_layout::{CacheKey, MountLayout};
