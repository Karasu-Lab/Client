//! A filesystem in memory, which is the whole filesystem on the web.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use crate::DirEntry;

/// Files by path, and the directories that hold them (or were made empty).
#[derive(Default)]
pub struct MemoryFs {
    files: BTreeMap<PathBuf, Arc<Vec<u8>>>,
    dirs: BTreeSet<PathBuf>,
}

/// `.` and `..` resolved, so one file has one key.
fn normal(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn not_found(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::NotFound,
        format!("{}: no such file or directory", path.display()),
    )
}

impl MemoryFs {
    pub fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        self.files
            .get(&normal(path))
            .map(|bytes| bytes.as_ref().clone())
            .ok_or_else(|| not_found(path))
    }

    pub fn exists(&self, path: &Path) -> bool {
        self.is_file(path) || self.is_dir(path)
    }

    pub fn is_file(&self, path: &Path) -> bool {
        self.files.contains_key(&normal(path))
    }

    pub fn is_dir(&self, path: &Path) -> bool {
        let path = normal(path);
        self.dirs.contains(&path)
            || self
                .files
                .keys()
                .any(|file| file.starts_with(&path) && *file != path)
    }

    pub fn len(&self, path: &Path) -> Option<u64> {
        self.files
            .get(&normal(path))
            .map(|bytes| bytes.len() as u64)
    }

    pub fn read_dir(&self, dir: &Path) -> io::Result<Vec<DirEntry>> {
        let dir = normal(dir);
        if !self.is_dir(&dir) {
            return Err(not_found(&dir));
        }
        let mut entries: BTreeMap<PathBuf, bool> = BTreeMap::new();
        let below = self
            .files
            .keys()
            .map(|path| (path, false))
            .chain(self.dirs.iter().map(|path| (path, true)));
        for (path, is_dir) in below {
            let Ok(rest) = path.strip_prefix(&dir) else {
                continue;
            };
            let mut parts = rest.components();
            let Some(first) = parts.next() else {
                continue;
            };
            let child = dir.join(first.as_os_str());
            let child_is_dir = is_dir || parts.next().is_some();
            let slot = entries.entry(child).or_insert(false);
            *slot |= child_is_dir;
        }
        Ok(entries
            .into_iter()
            .map(|(path, is_dir)| DirEntry { path, is_dir })
            .collect())
    }

    pub fn write(&mut self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        let path = normal(path);
        if let Some(parent) = path.parent() {
            self.create_dir_all(parent)?;
        }
        self.files.insert(path, Arc::new(bytes.to_vec()));
        Ok(())
    }

    pub fn create_dir_all(&mut self, dir: &Path) -> io::Result<()> {
        let mut path = PathBuf::new();
        for component in normal(dir).components() {
            path.push(component.as_os_str());
            self.dirs.insert(path.clone());
        }
        Ok(())
    }

    pub fn remove_file(&mut self, path: &Path) -> io::Result<()> {
        self.files
            .remove(&normal(path))
            .map(|_| ())
            .ok_or_else(|| not_found(path))
    }

    pub fn remove_dir_all(&mut self, dir: &Path) -> io::Result<()> {
        let dir = normal(dir);
        if !self.is_dir(&dir) {
            return Err(not_found(&dir));
        }
        self.files.retain(|path, _| !path.starts_with(&dir));
        self.dirs.retain(|path| !path.starts_with(&dir));
        Ok(())
    }

    /// Moves a file, or a directory with everything under it.
    pub fn rename(&mut self, from: &Path, to: &Path) -> io::Result<()> {
        let (from, to) = (normal(from), normal(to));
        if !self.exists(&from) {
            return Err(not_found(&from));
        }
        if let Some(parent) = to.parent() {
            self.create_dir_all(parent)?;
        }
        let moved: Vec<_> = self
            .files
            .keys()
            .filter(|path| path.starts_with(&from))
            .cloned()
            .collect();
        for path in moved {
            let bytes = self.files.remove(&path).expect("listed above");
            let target = to.join(path.strip_prefix(&from).expect("starts with from"));
            self.files.insert(target, bytes);
        }
        let dirs: Vec<_> = self
            .dirs
            .iter()
            .filter(|path| path.starts_with(&from))
            .cloned()
            .collect();
        for path in dirs {
            self.dirs.remove(&path);
            self.dirs
                .insert(to.join(path.strip_prefix(&from).expect("starts with from")));
        }
        Ok(())
    }

    /// Every file, for persisting.
    pub fn files(&self) -> impl Iterator<Item = (&Path, &Arc<Vec<u8>>)> {
        self.files
            .iter()
            .map(|(path, bytes)| (path.as_path(), bytes))
    }
}

/// The one tree a wasm build shares.
#[cfg(target_arch = "wasm32")]
pub(crate) mod global {
    use std::io;
    use std::path::Path;
    use std::sync::{Mutex, MutexGuard, PoisonError};

    use super::MemoryFs;
    use crate::DirEntry;

    static FS: Mutex<Option<MemoryFs>> = Mutex::new(None);

    pub(crate) fn with<R>(f: impl FnOnce(&mut MemoryFs) -> R) -> R {
        let mut guard: MutexGuard<'_, Option<MemoryFs>> =
            FS.lock().unwrap_or_else(PoisonError::into_inner);
        f(guard.get_or_insert_with(MemoryFs::default))
    }

    pub fn read(path: &Path) -> io::Result<Vec<u8>> {
        with(|fs| fs.read(path))
    }
    pub fn exists(path: &Path) -> bool {
        with(|fs| fs.exists(path))
    }
    pub fn is_file(path: &Path) -> bool {
        with(|fs| fs.is_file(path))
    }
    pub fn is_dir(path: &Path) -> bool {
        with(|fs| fs.is_dir(path))
    }
    pub fn len(path: &Path) -> Option<u64> {
        with(|fs| fs.len(path))
    }
    pub fn read_dir(dir: &Path) -> io::Result<Vec<DirEntry>> {
        with(|fs| fs.read_dir(dir))
    }
    pub fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
        with(|fs| fs.write(path, bytes))
    }
    pub fn create_dir_all(dir: &Path) -> io::Result<()> {
        with(|fs| fs.create_dir_all(dir))
    }
    pub fn remove_file(path: &Path) -> io::Result<()> {
        with(|fs| fs.remove_file(path))
    }
    pub fn remove_dir_all(dir: &Path) -> io::Result<()> {
        with(|fs| fs.remove_dir_all(dir))
    }
    pub fn rename(from: &Path, to: &Path) -> io::Result<()> {
        with(|fs| fs.rename(from, to))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn written_files_list_under_their_directories() {
        let mut fs = MemoryFs::default();
        fs.write(Path::new("/a/b/c.txt"), b"c").unwrap();
        fs.write(Path::new("/a/d.txt"), b"d").unwrap();
        let entries = fs.read_dir(Path::new("/a")).unwrap();
        assert_eq!(
            entries,
            [
                DirEntry {
                    path: "/a/b".into(),
                    is_dir: true
                },
                DirEntry {
                    path: "/a/d.txt".into(),
                    is_dir: false
                },
            ]
        );
        assert!(fs.is_dir(Path::new("/a/b")));
        assert!(fs.is_file(Path::new("/a/b/c.txt")));
    }

    #[test]
    fn rename_moves_a_whole_tree() {
        let mut fs = MemoryFs::default();
        fs.write(Path::new("/x.part/f"), b"1").unwrap();
        fs.rename(Path::new("/x.part"), Path::new("/x")).unwrap();
        assert_eq!(fs.read(Path::new("/x/f")).unwrap(), b"1");
        assert!(!fs.exists(Path::new("/x.part")));
    }

    #[test]
    fn dot_dot_does_not_make_a_second_key() {
        let mut fs = MemoryFs::default();
        fs.write(Path::new("/a/../b"), b"1").unwrap();
        assert!(fs.is_file(Path::new("/b")));
    }
}
