//! omnifuse-s3 - S3-compatible backend for `OmniFuse` powered by OpenDAL.

#![warn(missing_docs)]
#![warn(clippy::pedantic)]

pub mod config;
pub mod error;
pub mod manifest;
pub mod operator;
pub mod path;
pub mod session;

pub use config::S3Config;
pub use error::{S3Error, classify_s3_error};
