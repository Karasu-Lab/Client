//! The files the client reads and writes, behind one set of functions.
//!
//! On native targets they are `std::fs`. The browser has no filesystem, so
//! there they work on an in-memory tree ([`MemoryFs`]) that [`browser`] fills
//! from, and writes through to, the caches the page keeps.

use std::io;
use std::path::{Path, PathBuf};

#[cfg(target_arch = "wasm32")]
pub mod browser;
mod memory;
#[cfg(not(target_arch = "wasm32"))]
mod native;

pub use memory::MemoryFs;
#[cfg(target_arch = "wasm32")]
use memory::global as backend;
#[cfg(not(target_arch = "wasm32"))]
use native as backend;

/// One entry of a directory listing, as `std::fs::DirEntry` offers it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirEntry {
    pub(crate) path: PathBuf,
    pub(crate) is_dir: bool,
}

/// What a directory entry is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileType {
    is_dir: bool,
}

impl FileType {
    pub fn is_dir(&self) -> bool {
        self.is_dir
    }

    pub fn is_file(&self) -> bool {
        !self.is_dir
    }
}

impl DirEntry {
    pub fn path(&self) -> PathBuf {
        self.path.clone()
    }

    pub fn file_name(&self) -> std::ffi::OsString {
        self.path.file_name().unwrap_or_default().to_os_string()
    }

    pub fn file_type(&self) -> io::Result<FileType> {
        Ok(FileType {
            is_dir: self.is_dir,
        })
    }
}

/// The entries of a directory, which `std::fs::ReadDir` gives one `Result`
/// each.
pub struct ReadDir(std::vec::IntoIter<DirEntry>);

impl Iterator for ReadDir {
    type Item = io::Result<DirEntry>;

    fn next(&mut self) -> Option<Self::Item> {
        self.0.next().map(Ok)
    }
}

pub fn read(path: impl AsRef<Path>) -> io::Result<Vec<u8>> {
    backend::read(path.as_ref())
}

pub fn read_to_string(path: impl AsRef<Path>) -> io::Result<String> {
    String::from_utf8(read(path)?)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))
}

pub fn exists(path: impl AsRef<Path>) -> bool {
    backend::exists(path.as_ref())
}

pub fn is_file(path: impl AsRef<Path>) -> bool {
    backend::is_file(path.as_ref())
}

pub fn is_dir(path: impl AsRef<Path>) -> bool {
    backend::is_dir(path.as_ref())
}

/// The size of the file at `path`.
pub fn len(path: impl AsRef<Path>) -> Option<u64> {
    backend::len(path.as_ref())
}

/// The entries of `dir`, sorted by name.
pub fn read_dir(dir: impl AsRef<Path>) -> io::Result<ReadDir> {
    backend::read_dir(dir.as_ref()).map(|entries| ReadDir(entries.into_iter()))
}

pub fn write(path: impl AsRef<Path>, bytes: &[u8]) -> io::Result<()> {
    backend::write(path.as_ref(), bytes)
}

pub fn create_dir_all(dir: impl AsRef<Path>) -> io::Result<()> {
    backend::create_dir_all(dir.as_ref())
}

pub fn remove_file(path: impl AsRef<Path>) -> io::Result<()> {
    backend::remove_file(path.as_ref())
}

pub fn remove_dir_all(dir: impl AsRef<Path>) -> io::Result<()> {
    backend::remove_dir_all(dir.as_ref())
}

pub fn rename(from: impl AsRef<Path>, to: impl AsRef<Path>) -> io::Result<()> {
    backend::rename(from.as_ref(), to.as_ref())
}
