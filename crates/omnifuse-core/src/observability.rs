//! Shared logging initialization.

use std::{
  fs::{File, OpenOptions},
  io,
  path::Path,
  sync::{Arc, Mutex, OnceLock}
};

use tracing_subscriber::{
  EnvFilter,
  fmt::writer::{BoxMakeWriter, MakeWriter}
};

use crate::LoggingConfig;

static LOGGING_INITIALIZED: OnceLock<()> = OnceLock::new();

/// Initialize shared logging for CLI and GUI.
///
/// Repeated calls are no-ops once a global subscriber has been installed.
///
/// # Errors
///
/// Returns an error if file sink initialization or subscriber installation fails.
pub fn init_logging(config: &LoggingConfig) -> anyhow::Result<()> {
  if LOGGING_INITIALIZED.get().is_some() {
    return Ok(());
  }

  let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(config.filter_directive()));

  let writer = make_writer(config)?;
  tracing_subscriber::fmt()
    .with_env_filter(env_filter)
    .with_writer(writer)
    .with_target(false)
    .compact()
    .try_init()
    .map_err(|error| anyhow::anyhow!("failed to initialize tracing subscriber: {error}"))?;

  let _ = LOGGING_INITIALIZED.set(());
  Ok(())
}

fn make_writer(config: &LoggingConfig) -> anyhow::Result<BoxMakeWriter> {
  match &config.log_file {
    Some(path) => Ok(BoxMakeWriter::new(SharedFileMakeWriter::new(path)?)),
    None => Ok(BoxMakeWriter::new(io::stderr))
  }
}

#[derive(Clone, Debug)]
struct SharedFileMakeWriter {
  file: Arc<Mutex<File>>
}

impl SharedFileMakeWriter {
  fn new(path: &Path) -> anyhow::Result<Self> {
    if let Some(parent) = path.parent() {
      std::fs::create_dir_all(parent)?;
    }

    let file = OpenOptions::new().create(true).append(true).open(path)?;
    Ok(Self {
      file: Arc::new(Mutex::new(file))
    })
  }
}

impl<'a> MakeWriter<'a> for SharedFileMakeWriter {
  type Writer = SharedFileWriter;

  fn make_writer(&'a self) -> Self::Writer {
    SharedFileWriter {
      file: Arc::clone(&self.file)
    }
  }
}

struct SharedFileWriter {
  file: Arc<Mutex<File>>
}

impl io::Write for SharedFileWriter {
  fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
    let mut file = self
      .file
      .lock()
      .map_err(|_| io::Error::other("log file lock poisoned"))?;
    io::Write::write(&mut *file, buf)
  }

  fn flush(&mut self) -> io::Result<()> {
    let mut file = self
      .file
      .lock()
      .map_err(|_| io::Error::other("log file lock poisoned"))?;
    io::Write::flush(&mut *file)
  }
}

#[cfg(test)]
mod tests {
  #![allow(clippy::expect_used)]

  use std::fs;

  use tempfile::tempdir;

  use super::*;

  #[test]
  fn test_file_sink_writes_problem_log_records() {
    let temp = tempdir().expect("create temp dir");
    let log_path = temp.path().join("logs").join("omnifuse.log");
    let config = LoggingConfig {
      level: "warn".to_string(),
      log_file: Some(log_path.clone())
    };

    let writer = make_writer(&config).expect("create file writer");
    let subscriber = tracing_subscriber::fmt()
      .with_env_filter(EnvFilter::new(config.filter_directive()))
      .with_writer(writer)
      .with_target(false)
      .compact()
      .finish();

    tracing::subscriber::with_default(subscriber, || {
      tracing::warn!(code = "offline", op = "poll_remote", "poll failed");
    });

    let contents = fs::read_to_string(&log_path).expect("read log file");
    assert!(
      contents.contains("poll failed"),
      "expected problem log in file sink, got: {contents}"
    );
    assert!(
      contents.contains("offline"),
      "expected structured fields in file sink, got: {contents}"
    );
  }
}
