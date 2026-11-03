//! The OS filesystem.

use std::io;
use std::path::Path;

use crate::DirEntry;

pub fn read(path: &Path) -> io::Result<Vec<u8>> {
    std::fs::read(path)
}

pub fn exists(path: &Path) -> bool {
    path.exists()
}

pub fn is_file(path: &Path) -> bool {
    path.is_file()
}

pub fn is_dir(path: &Path) -> bool {
    path.is_dir()
}

pub fn len(path: &Path) -> Option<u64> {
    std::fs::metadata(path).ok().map(|m| m.len())
}

pub fn read_dir(dir: &Path) -> io::Result<Vec<DirEntry>> {
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        entries.push(DirEntry {
            is_dir: entry.file_type().is_ok_and(|kind| kind.is_dir()),
            path: entry.path(),
        });
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(entries)
}

pub fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    std::fs::write(path, bytes)
}

pub fn create_dir_all(dir: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dir)
}

pub fn remove_file(path: &Path) -> io::Result<()> {
    std::fs::remove_file(path)
}

pub fn remove_dir_all(dir: &Path) -> io::Result<()> {
    std::fs::remove_dir_all(dir)
}

pub fn rename(from: &Path, to: &Path) -> io::Result<()> {
    std::fs::rename(from, to)
}
