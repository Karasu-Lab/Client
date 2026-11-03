//! Resource packs served from a base URL, for the browser, which has no
//! `resourcepacks` folder to put them in.
//!
//! `<base>/packs.json` lists the files as a JSON array of paths relative to
//! `<base>` (`["my_pack/pack.mcmeta", "my_pack/assets/minecraft/..."]`, or zip
//! packs such as `"my_pack.zip"`). Each is fetched from `<base>/<path>` and
//! written under the same path in the destination directory.

use std::path::{Component, Path, PathBuf};

use futures_util::future::join_all;

use crate::{CONCURRENT_DOWNLOADS, fetch, get_json, with_retries, write_atomic};

/// Where `file`, a path from `packs.json`, lands under `dest`; none if it
/// would leave `dest` or isn't a plain relative path.
fn pack_file_path(dest: &Path, file: &str) -> Option<PathBuf> {
    let relative = Path::new(file);
    if file.is_empty()
        || !relative
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
    {
        return None;
    }
    Some(dest.join(relative))
}

/// Fetches every file `<base>/packs.json` lists into `dest`, returning how
/// many there were.
pub async fn sync_packs(base_url: &str, dest: &Path) -> Result<usize, String> {
    let base = base_url.trim_end_matches('/');
    let http = reqwest::Client::new();
    let files: Vec<String> = get_json(&http, &format!("{base}/packs.json")).await?;
    let mut targets = Vec::with_capacity(files.len());
    for file in &files {
        let path = pack_file_path(dest, file)
            .ok_or_else(|| format!("{base}/packs.json: `{file}` is not a relative path"))?;
        targets.push((format!("{base}/{file}"), path));
    }
    for batch in targets.chunks(CONCURRENT_DOWNLOADS) {
        let results = join_all(batch.iter().map(|(url, path)| {
            let http = &http;
            async move {
                let bytes = with_retries(url, || fetch(http, url)).await?;
                write_atomic(path, &bytes).await
            }
        }))
        .await;
        results.into_iter().collect::<Result<(), String>>()?;
    }
    Ok(targets.len())
}
