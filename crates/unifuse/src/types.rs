//! Кроссплатформенные типы файловой системы.
//!
//! Общее подмножество POSIX stat и Windows `FILE_INFO`.
//! Platform adapters (rfuse3, winfsp) маппят из/в платформо-специфичные типы.

use std::time::SystemTime;

/// Тип файлового объекта.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileType {
  /// Обычный файл.
  RegularFile,
  /// Директория.
  Directory,
  /// Символическая ссылка.
  Symlink,
  /// Блочное устройство.
  BlockDevice,
  /// Символьное устройство.
  CharDevice,
  /// Именованный канал (FIFO).
  NamedPipe,
  /// Unix socket.
  Socket
}

/// Атрибуты файла (кроссплатформенные).
///
/// Общее подмножество POSIX stat и Windows `FILE_INFO`.
/// Platform adapters маппят из/в платформо-специфичные типы.
#[derive(Debug, Clone)]
pub struct FileAttr {
  /// Размер файла в байтах.
  pub size: u64,
  /// Размер в блоках (512 байт).
  pub blocks: u64,
  /// Время последнего доступа.
  pub atime: SystemTime,
  /// Время последней модификации.
  pub mtime: SystemTime,
  /// Время последнего изменения метаданных.
  pub ctime: SystemTime,
  /// Время создания (macOS/Windows).
  pub crtime: SystemTime,
  /// Тип файла.
  pub kind: FileType,
  /// Права доступа (Unix mode: 0o644, 0o755). На Windows → readonly маппинг.
  pub perm: u16,
  /// Количество жёстких ссылок.
  pub nlink: u32,
  /// User ID (Unix). На Windows = 0.
  pub uid: u32,
  /// Group ID (Unix). На Windows = 0.
  pub gid: u32,
  /// Device ID (для специальных файлов).
  pub rdev: u32,
  /// Флаги (BSD flags на macOS / `FILE_ATTRIBUTE_*` на Windows).
  pub flags: u32
}

impl FileAttr {
  /// Создать атрибуты для обычного файла с типичными значениями.
  #[must_use]
  pub fn regular(size: u64, perm: u16) -> Self {
    let now = SystemTime::now();
    Self {
      size,
      blocks: size.div_ceil(512),
      atime: now,
      mtime: now,
      ctime: now,
      crtime: now,
      kind: FileType::RegularFile,
      perm,
      nlink: 1,
      uid: 0,
      gid: 0,
      rdev: 0,
      flags: 0
    }
  }

  /// Создать атрибуты для директории.
  #[must_use]
  pub fn directory(perm: u16) -> Self {
    let now = SystemTime::now();
    Self {
      size: 0,
      blocks: 0,
      atime: now,
      mtime: now,
      ctime: now,
      crtime: now,
      kind: FileType::Directory,
      perm,
      nlink: 2,
      uid: 0,
      gid: 0,
      rdev: 0,
      flags: 0
    }
  }
}

/// Хэндл открытого файла.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileHandle(pub u64);

/// Элемент директории.
#[derive(Debug, Clone)]
pub struct DirEntry {
  /// Имя файла (без пути).
  pub name: String,
  /// Тип файла.
  pub kind: FileType
}

/// Флаги открытия файла.
#[derive(Debug, Clone, Copy)]
#[allow(clippy::struct_excessive_bools)]
pub struct OpenFlags {
  /// Открытие на чтение.
  pub read: bool,
  /// Открытие на запись.
  pub write: bool,
  /// Режим дописывания.
  pub append: bool,
  /// Обрезать файл до нулевой длины.
  pub truncate: bool,
  /// Создать файл если не существует.
  pub create: bool,
  /// Ошибка если файл уже существует (вместе с create).
  pub exclusive: bool
}

impl OpenFlags {
  /// Флаги для чтения.
  #[must_use]
  pub const fn read_only() -> Self {
    Self {
      read: true,
      write: false,
      append: false,
      truncate: false,
      create: false,
      exclusive: false
    }
  }

  /// Флаги для записи.
  #[must_use]
  pub const fn write_only() -> Self {
    Self {
      read: false,
      write: true,
      append: false,
      truncate: false,
      create: false,
      exclusive: false
    }
  }

  /// Флаги для чтения и записи.
  #[must_use]
  pub const fn read_write() -> Self {
    Self {
      read: true,
      write: true,
      append: false,
      truncate: false,
      create: false,
      exclusive: false
    }
  }
}

/// Информация о файловой системе.
#[derive(Debug, Clone)]
pub struct StatFs {
  /// Общее количество блоков.
  pub blocks: u64,
  /// Свободные блоки.
  pub bfree: u64,
  /// Доступные блоки (для непривилегированных пользователей).
  pub bavail: u64,
  /// Общее количество inodes.
  pub files: u64,
  /// Свободные inodes.
  pub ffree: u64,
  /// Размер блока в байтах.
  pub bsize: u32,
  /// Максимальная длина имени файла.
  pub namelen: u32
}

/// Ошибки FUSE-операций.
#[derive(Debug, thiserror::Error)]
pub enum FsError {
  /// Файл или директория не найдены.
  #[error("not found")]
  NotFound,
  /// Нет прав доступа.
  #[error("permission denied")]
  PermissionDenied,
  /// Файл или директория уже существует.
  #[error("already exists")]
  AlreadyExists,
  /// Ожидалась директория, но это не она.
  #[error("not a directory")]
  NotADirectory,
  /// Путь является директорией (когда ожидался файл).
  #[error("is a directory")]
  IsADirectory,
  /// Директория не пуста (при удалении).
  #[error("directory not empty")]
  NotEmpty,
  /// Операция не поддерживается.
  #[error("not supported")]
  NotSupported,
  /// Ошибка ввода-вывода.
  #[error(transparent)]
  Io(#[from] std::io::Error),
  /// Прочая ошибка.
  #[error("{0}")]
  Other(String)
}

impl FsError {
  /// Преобразование в libc errno для FUSE.
  #[cfg(unix)]
  #[must_use]
  pub fn to_errno(&self) -> i32 {
    match self {
      Self::NotFound => libc::ENOENT,
      Self::PermissionDenied => libc::EACCES,
      Self::AlreadyExists => libc::EEXIST,
      Self::NotADirectory => libc::ENOTDIR,
      Self::IsADirectory => libc::EISDIR,
      Self::NotEmpty => libc::ENOTEMPTY,
      Self::NotSupported => libc::ENOSYS,
      Self::Io(e) => e.raw_os_error().unwrap_or(libc::EIO),
      Self::Other(_) => libc::EIO
    }
  }
}
