use std::path::{Path, PathBuf};

use cache::Entry;
pub use cache::{cache_name, key};

use crate::memory::global::with;

const PERSIST_BATCH: usize = 64;

fn read(path: &Path) -> Result<Entry, String> {
    Ok(Entry {
        path: path.to_path_buf(),
        bytes: crate::read(path).map_err(|e| format!("{}: {e}", path.display()))?,
    })
}

fn write(entries: Vec<Entry>) -> Result<usize, String> {
    let count = entries.len();
    for entry in entries {
        with(|fs| fs.write(&entry.path, &entry.bytes)).map_err(|e| e.to_string())?;
    }
    Ok(count)
}

async fn save(namespace: &str, paths: &[PathBuf]) -> Result<(), String> {
    let entries = paths
        .iter()
        .map(|path| read(path))
        .collect::<Result<Vec<_>, _>>()?;
    cache::save(namespace, &entries)
        .await
        .map_err(|e| e.to_string())
}

pub async fn persist(namespace: &str, path: &Path) -> Result<(), String> {
    save(namespace, &[path.to_path_buf()]).await
}

pub async fn persist_dir(namespace: &str, dir: &Path) -> Result<(), String> {
    persist_dir_with(namespace, dir, &[], &|_, _| {}).await
}

pub async fn persist_dir_with(
    namespace: &str,
    dir: &Path,
    last: &[PathBuf],
    on_progress: &dyn Fn(u32, u32),
) -> Result<(), String> {
    let paths: Vec<PathBuf> = with(|fs| {
        fs.files()
            .map(|(path, _)| path.to_path_buf())
            .filter(|path| path.starts_with(dir) && !last.contains(path))
            .collect()
    });
    let last: Vec<PathBuf> = last
        .iter()
        .filter(|path| path.starts_with(dir))
        .cloned()
        .collect();
    let total = (paths.len() + last.len()) as u32;
    let mut done = 0;
    on_progress(done, total);
    for batch in paths.chunks(PERSIST_BATCH) {
        save(namespace, batch).await?;
        done += batch.len() as u32;
        on_progress(done, total);
    }
    for path in last {
        save(namespace, &[path]).await?;
        done += 1;
        on_progress(done, total);
    }
    Ok(())
}

pub async fn persist_files(
    namespace: &str,
    paths: &[PathBuf],
    on_progress: &dyn Fn(u32, u32),
) -> Result<(), String> {
    let total = paths.len() as u32;
    on_progress(0, total);
    for (index, path) in paths.iter().enumerate() {
        save(namespace, std::slice::from_ref(path)).await?;
        on_progress(index as u32 + 1, total);
    }
    Ok(())
}

pub async fn restore_paths(namespace: &str, paths: &[PathBuf]) -> Result<usize, String> {
    write(
        cache::load_paths(namespace, paths)
            .await
            .map_err(|e| e.to_string())?,
    )
}

pub async fn restore(namespace: &str) -> Result<usize, String> {
    write(cache::load(namespace).await.map_err(|e| e.to_string())?)
}
