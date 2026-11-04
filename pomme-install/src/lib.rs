use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;
#[cfg(not(target_arch = "wasm32"))]
use std::time::Instant;

use futures_util::StreamExt;
use futures_util::stream::FuturesUnordered;
use serde::Deserialize;
#[cfg(target_arch = "wasm32")]
use web_time::Instant;

pub mod bedrock;
#[cfg(not(target_arch = "wasm32"))]
pub mod client;
pub mod packs;

const VERSION_MANIFEST_URL: &str =
    "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";
const RESOURCES_BASE_URL: &str = "https://resources.download.minecraft.net";
const CONCURRENT_DOWNLOADS: usize = 32;
const ATTEMPTS: u32 = 3;
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);
#[cfg(not(target_arch = "wasm32"))]
const USER_AGENT: &str = "pomme";

#[derive(Clone, Debug)]
pub struct Endpoints {
    pub version_manifest: String,
    pub resources_base: String,
    pub keep_object: Option<fn(&str) -> bool>,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            version_manifest: VERSION_MANIFEST_URL.to_owned(),
            resources_base: RESOURCES_BASE_URL.to_owned(),
            keep_object: None,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Progress {
    pub done: u32,
    pub total: u32,
    pub status: String,
    /// Bytes received of the one file the operation is currently
    /// downloading, and that file's size. Both are zero while no file
    /// download is in flight.
    pub file_done: u64,
    pub file_total: u64,
}

impl Progress {
    pub fn fraction(&self) -> f32 {
        if self.total == 0 {
            0.0
        } else {
            self.done as f32 / self.total as f32
        }
    }
}

#[derive(Deserialize)]
struct VersionManifest {
    versions: Vec<VersionEntry>,
}

#[derive(Deserialize)]
struct VersionEntry {
    id: String,
    url: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct VersionJson {
    asset_index: Download,
    downloads: Downloads,
}

#[derive(Deserialize)]
struct Downloads {
    client: Download,
}

#[derive(Deserialize)]
struct Download {
    url: String,
    sha1: String,
    size: u64,
}

#[derive(Deserialize)]
struct AssetIndexJson {
    objects: HashMap<String, AssetObject>,
}

#[derive(Deserialize)]
struct AssetObject {
    hash: String,
    size: u64,
}

fn index_path(assets_dir: &Path, version: &str) -> PathBuf {
    assets_dir.join("indexes").join(format!("{version}.json"))
}

fn extracted_dir(versions_dir: &Path, version: &str) -> PathBuf {
    versions_dir.join(version).join("extracted")
}

const EXTRACTED_MARKER: &str = ".extracted";

fn extracted_marker(versions_dir: &Path, version: &str) -> PathBuf {
    extracted_dir(versions_dir, version).join(EXTRACTED_MARKER)
}

pub fn install_markers(
    assets_dir: &Path,
    versions_dir: &Path,
    version: &str,
) -> (PathBuf, PathBuf) {
    (
        index_path(assets_dir, version),
        extracted_marker(versions_dir, version),
    )
}

pub fn is_installed(assets_dir: &Path, versions_dir: &Path, version: &str) -> bool {
    pomme_vfs::exists(&index_path(assets_dir, version))
        && pomme_vfs::exists(&extracted_marker(versions_dir, version))
}

pub async fn install(
    http: &reqwest::Client,
    assets_dir: &Path,
    versions_dir: &Path,
    version: &str,
    on_progress: &(dyn Fn(Progress) + Send + Sync),
) -> Result<(), String> {
    install_from(
        http,
        &Endpoints::default(),
        assets_dir,
        versions_dir,
        version,
        on_progress,
    )
    .await
}

pub async fn install_with(
    endpoints: &Endpoints,
    assets_dir: &Path,
    versions_dir: &Path,
    version: &str,
    on_progress: &(dyn Fn(Progress) + Send + Sync),
) -> Result<(), String> {
    install_from(
        &reqwest::Client::new(),
        endpoints,
        assets_dir,
        versions_dir,
        version,
        on_progress,
    )
    .await
}

pub async fn install_from(
    http: &reqwest::Client,
    endpoints: &Endpoints,
    assets_dir: &Path,
    versions_dir: &Path,
    version: &str,
    on_progress: &(dyn Fn(Progress) + Send + Sync),
) -> Result<(), String> {
    let report = reporter(on_progress);
    report(0, 1, "Fetching version info...");
    let manifest: VersionManifest = get_json(http, &endpoints.version_manifest).await?;
    let entry = manifest
        .versions
        .iter()
        .find(|v| v.id == version)
        .ok_or_else(|| format!("Version {version} not found"))?;
    let version_json: VersionJson = get_json(http, &entry.url).await?;

    let index_bytes = fetch_verified(
        http,
        &version_json.asset_index.url,
        &version_json.asset_index.sha1,
        Hash::Sha1,
    )
    .await?;
    let index: AssetIndexJson =
        serde_json::from_slice(&index_bytes).map_err(|e| format!("Asset index: {e}"))?;
    let objects_dir = assets_dir.join("objects");
    let objects = index
        .objects
        .iter()
        .filter(|(key, _)| endpoints.keep_object.is_none_or(|keep| keep(key)))
        .map(|(_, obj)| Fetch {
            url: format!(
                "{}/{}/{}",
                endpoints.resources_base,
                &obj.hash[..2],
                obj.hash
            ),
            path: object_path(&objects_dir, &obj.hash),
            size: obj.size,
            expected: obj.hash.clone(),
            hash: Hash::Sha1,
        })
        .collect();
    download_all(http, objects, on_progress).await?;
    write_atomic(&index_path(assets_dir, version), &index_bytes).await?;

    let out = extracted_dir(versions_dir, version);
    if !pomme_vfs::exists(&extracted_marker(versions_dir, version)) {
        let client = &version_json.downloads.client;
        let jar = fetch_verified_with_progress(
            http,
            &client.url,
            &client.sha1,
            Hash::Sha1,
            &|received| {
                on_progress(Progress {
                    done: 0,
                    total: 1,
                    status: "Downloading the client jar...".to_owned(),
                    file_done: received,
                    file_total: client.size,
                });
            },
        )
        .await?;
        let mut part = out.as_os_str().to_owned();
        part.push(".part");
        let part = PathBuf::from(part);
        let extract_report = |done, total| {
            report(
                done,
                total,
                &format!("Extracting the client jar ({done}/{total})"),
            )
        };
        blocking(|| {
            let _ = pomme_vfs::remove_dir_all(&part);
            extract_assets(&jar, &part, &extract_report)?;
            pomme_vfs::write(&part.join(EXTRACTED_MARKER), version.as_bytes())
                .map_err(|e| format!("{}: {e}", part.display()))?;
            let _ = pomme_vfs::remove_dir_all(&out);
            pomme_vfs::rename(&part, &out).map_err(|e| format!("{}: {e}", out.display()))
        })?;
    }
    Ok(())
}

fn blocking<R>(work: impl FnOnce() -> R) -> R {
    #[cfg(not(target_arch = "wasm32"))]
    return tokio::task::block_in_place(work);
    #[cfg(target_arch = "wasm32")]
    work()
}

async fn sleep(duration: Duration) {
    #[cfg(not(target_arch = "wasm32"))]
    tokio::time::sleep(duration).await;
    #[cfg(target_arch = "wasm32")]
    gloo_timers::future::sleep(duration).await;
}

#[derive(Clone, Copy)]
pub(crate) enum Hash {
    Sha1,
    GitBlob,
}

impl Hash {
    fn of(self, bytes: &[u8]) -> String {
        let mut sha1 = sha1_smol::Sha1::new();
        if let Self::GitBlob = self {
            sha1.update(format!("blob {}\0", bytes.len()).as_bytes());
        }
        sha1.update(bytes);
        sha1.digest().to_string()
    }
}

fn reporter(
    on_progress: &(dyn Fn(Progress) + Send + Sync),
) -> impl Fn(u32, u32, &str) + Send + Sync + '_ {
    move |done, total, status| {
        on_progress(Progress {
            done,
            total,
            status: status.to_owned(),
            file_done: 0,
            file_total: 0,
        })
    }
}

pub(crate) struct Fetch {
    pub url: String,
    pub path: PathBuf,
    pub size: u64,
    pub expected: String,
    pub hash: Hash,
}

/// A file counts as downloaded only when the bytes stored under its
/// path hash to the value the download expects. A file that merely has
/// the right size can still hold the wrong bytes — a write that was
/// corrupted or a store that changed under us — and is fetched again.
/// This is also what makes a stopped download resumable: everything
/// already fetched and verified stays, and the next run fetches only
/// what is missing or wrong.
fn is_present(file: &Fetch) -> bool {
    if pomme_vfs::len(&file.path) != Some(file.size) {
        return false;
    }
    match pomme_vfs::read(&file.path) {
        Ok(bytes) => file.hash.of(&bytes) == file.expected,
        Err(_) => false,
    }
}

/// The live state of a `download_all` run, shared by its download
/// tasks: how many bytes of each file have arrived so far. Progress
/// reports are built from it, so the one reported state is computed
/// from all the downloads together instead of each download reporting
/// over the others.
struct DownloadState {
    sizes: Vec<u64>,
    received: Vec<AtomicU64>,
    started: Vec<AtomicBool>,
    finished: Vec<AtomicBool>,
}

impl DownloadState {
    fn new(files: &[Fetch]) -> Self {
        Self {
            sizes: files.iter().map(|file| file.size).collect(),
            received: files.iter().map(|_| AtomicU64::new(0)).collect(),
            started: files.iter().map(|_| AtomicBool::new(false)).collect(),
            finished: files.iter().map(|_| AtomicBool::new(false)).collect(),
        }
    }

    fn bytes_done(&self) -> u64 {
        self.sizes
            .iter()
            .zip(&self.received)
            .map(|(size, received)| received.load(Ordering::Relaxed).min(*size))
            .sum()
    }

    /// The one file the per-file progress follows: the first file
    /// that started and has not finished yet.
    fn current_file(&self) -> (u64, u64) {
        for (index, started) in self.started.iter().enumerate() {
            if started.load(Ordering::Relaxed) && !self.finished[index].load(Ordering::Relaxed) {
                let size = self.sizes[index];
                return (self.received[index].load(Ordering::Relaxed).min(size), size);
            }
        }
        (0, 0)
    }
}

fn as_u32(bytes: u64) -> u32 {
    u32::try_from(bytes).unwrap_or(u32::MAX)
}

pub(crate) async fn download_all(
    http: &reqwest::Client,
    files: Vec<Fetch>,
    on_progress: &(dyn Fn(Progress) + Send + Sync),
) -> Result<(), String> {
    let missing: Vec<Fetch> = files.into_iter().filter(|file| !is_present(file)).collect();
    let total_bytes = missing.iter().map(|file| file.size).sum::<u64>();
    let state = DownloadState::new(&missing);
    let last_report: Mutex<Option<Instant>> = Mutex::new(None);
    let report = |force: bool| {
        let now = Instant::now();
        let mut last = last_report.lock().unwrap();
        if !force && last.is_some_and(|last| now.duration_since(last) < PROGRESS_INTERVAL) {
            return;
        }
        *last = Some(now);
        drop(last);
        let done = state.bytes_done();
        let (file_done, file_total) = state.current_file();
        on_progress(Progress {
            done: as_u32(done),
            total: as_u32(total_bytes),
            status: format!(
                "Downloading assets ({:.1}/{:.1} MB)",
                done as f64 / 1048576.0,
                total_bytes as f64 / 1048576.0
            ),
            file_done,
            file_total,
        });
    };
    let download = |index: usize, file: Fetch| {
        let http = http.clone();
        let state = &state;
        let report = &report;
        async move {
            state.started[index].store(true, Ordering::Relaxed);
            let on_bytes = |received: u64| {
                state.received[index].store(received, Ordering::Relaxed);
                report(false);
            };
            let bytes = fetch_verified_with_progress(
                &http,
                &file.url,
                &file.expected,
                file.hash,
                &on_bytes,
            )
            .await?;
            write_atomic(&file.path, &bytes).await?;
            state.received[index].store(file.size, Ordering::Relaxed);
            state.finished[index].store(true, Ordering::Relaxed);
            report(true);
            Ok::<(), String>(())
        }
    };
    let mut tasks = FuturesUnordered::new();
    let mut pending = missing.into_iter().enumerate();
    for (index, file) in pending.by_ref().take(CONCURRENT_DOWNLOADS) {
        tasks.push(download(index, file));
    }
    while let Some(result) = tasks.next().await {
        result?;
        if let Some((index, file)) = pending.next() {
            tasks.push(download(index, file));
        }
    }
    report(true);
    Ok(())
}

fn object_path(objects_dir: &Path, hash: &str) -> PathBuf {
    objects_dir.join(&hash[..2]).join(hash)
}

pub(crate) async fn get_json<T: serde::de::DeserializeOwned>(
    http: &reqwest::Client,
    url: &str,
) -> Result<T, String> {
    let bytes = with_retries(url, || fetch(http, url)).await?;
    serde_json::from_slice(&bytes).map_err(|e| format!("{url}: {e}"))
}

pub(crate) async fn get(http: &reqwest::Client, url: &str) -> Result<reqwest::Response, String> {
    let request = http.get(url);
    #[cfg(not(target_arch = "wasm32"))]
    let request = request.header(reqwest::header::USER_AGENT, USER_AGENT);
    request
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| e.to_string())
}

async fn fetch(http: &reqwest::Client, url: &str) -> Result<Vec<u8>, String> {
    fetch_with_progress(http, url, &|_| {}).await
}

/// Downloads `url`, calling `on_bytes` with the number of bytes
/// received so far after every chunk.
async fn fetch_with_progress(
    http: &reqwest::Client,
    url: &str,
    on_bytes: &(dyn Fn(u64) + Send + Sync),
) -> Result<Vec<u8>, String> {
    let response = get(http, url).await?;
    let mut bytes = Vec::with_capacity(response.content_length().unwrap_or(0) as usize);
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| e.to_string())?;
        bytes.extend_from_slice(&chunk);
        on_bytes(bytes.len() as u64);
    }
    Ok(bytes)
}

async fn fetch_verified(
    http: &reqwest::Client,
    url: &str,
    expected: &str,
    hash: Hash,
) -> Result<Vec<u8>, String> {
    fetch_verified_with_progress(http, url, expected, hash, &|_| {}).await
}

/// Downloads `url` until the bytes hash to `expected`, reporting the
/// bytes of each attempt through `on_bytes`. An attempt restarts the
/// count, so the reported progress always follows the attempt that
/// can still succeed.
async fn fetch_verified_with_progress(
    http: &reqwest::Client,
    url: &str,
    expected: &str,
    hash: Hash,
    on_bytes: &(dyn Fn(u64) + Send + Sync),
) -> Result<Vec<u8>, String> {
    with_retries(url, || async {
        on_bytes(0);
        let bytes = fetch_with_progress(http, url, on_bytes).await?;
        if hash.of(&bytes) == expected {
            Ok(bytes)
        } else {
            Err("the download didn't match its hash".into())
        }
    })
    .await
}

async fn with_retries<T, F, Fut>(url: &str, mut attempt: F) -> Result<T, String>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, String>>,
{
    let mut last_error = String::new();
    for n in 0..ATTEMPTS {
        if n > 0 {
            sleep(Duration::from_millis(500 * u64::from(n))).await;
        }
        match attempt().await {
            Ok(value) => return Ok(value),
            Err(e) => last_error = e,
        }
    }
    Err(format!("{url}: {last_error}"))
}

pub async fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        pomme_vfs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let mut part = path.as_os_str().to_owned();
    part.push(".part");
    let part = PathBuf::from(part);
    pomme_vfs::write(&part, bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    pomme_vfs::rename(&part, path).map_err(|e| format!("{}: {e}", path.display()))
}

fn extract_assets(jar: &[u8], out: &Path, report: &dyn Fn(u32, u32)) -> Result<(), String> {
    let mut archive =
        zip::ZipArchive::new(std::io::Cursor::new(jar)).map_err(|e| format!("Client jar: {e}"))?;
    let total = archive.len() as u32;
    let mut last_report = Instant::now();
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| format!("Client jar: {e}"))?;
        let Some(name) = entry.enclosed_name() else {
            continue;
        };
        if entry.is_dir() || !name.starts_with("assets") {
            continue;
        }
        let path = out.join(name);
        if let Some(dir) = path.parent() {
            pomme_vfs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let mut bytes = Vec::with_capacity(entry.size() as usize);
        std::io::copy(&mut entry, &mut bytes).map_err(|e| format!("{}: {e}", path.display()))?;
        pomme_vfs::write(&path, &bytes).map_err(|e| format!("{}: {e}", path.display()))?;
        if last_report.elapsed() >= PROGRESS_INTERVAL {
            report(i as u32 + 1, total);
            last_report = Instant::now();
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::{Fetch, Hash, extract_assets, is_installed, is_present, write_atomic};

    pub(crate) fn temp_dir(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("pomme-install-{label}-{}", std::process::id()))
    }

    fn jar(entries: &[(&str, &str)]) -> Vec<u8> {
        let entries: Vec<_> = entries
            .iter()
            .map(|&(name, body)| (name, body, 0o644))
            .collect();
        zip_of(&entries)
    }

    pub(crate) fn zip_of(entries: &[(&str, &str, u32)]) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for &(name, body, mode) in entries {
            let options = zip::write::SimpleFileOptions::default().unix_permissions(mode);
            zip.start_file(name, options).unwrap();
            zip.write_all(body.as_bytes()).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    #[test]
    fn sha1_is_lower_hex() {
        assert_eq!(
            Hash::Sha1.of(b"abc"),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
    }

    #[test]
    fn git_blob_hashes_match_git() {
        assert_eq!(
            Hash::GitBlob.of(b""),
            "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391"
        );
        assert_eq!(
            Hash::GitBlob.of(b"hello\n"),
            "ce013625030ba8dba906f756967f9e9ca394464a"
        );
    }

    #[test]
    fn only_assets_extract_and_escapes_are_refused() {
        let dir = temp_dir("extract");
        let jar = jar(&[
            ("assets/minecraft/lang/en_us.json", "{}"),
            ("net/minecraft/Main.class", "code"),
            ("assets/../../escaped.txt", "nope"),
        ]);
        extract_assets(&jar, &dir, &|_, _| {}).unwrap();
        assert!(dir.join("assets/minecraft/lang/en_us.json").exists());
        assert!(!dir.join("net").exists());
        assert!(!dir.parent().unwrap().join("escaped.txt").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn presence_needs_the_expected_bytes_not_just_the_size() {
        let dir = temp_dir("present");
        let path = dir.join("objects/aa/object");
        let fetch = Fetch {
            url: String::new(),
            path: path.clone(),
            size: 5,
            expected: Hash::Sha1.of(b"hello"),
            hash: Hash::Sha1,
        };
        assert!(!is_present(&fetch));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"hello").unwrap();
        assert!(is_present(&fetch));
        // The same size with different bytes is not the downloaded file:
        // the hash comparison fetches it again.
        std::fs::write(&path, b"hallo").unwrap();
        assert!(!is_present(&fetch));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn writes_land_whole_and_mark_the_install() {
        let dir = temp_dir("install");
        let (assets, versions) = (dir.join("assets"), dir.join("versions"));
        assert!(!is_installed(&assets, &versions, "26.2"));
        write_atomic(&assets.join("indexes/26.2.json"), b"{}")
            .await
            .unwrap();
        assert!(!is_installed(&assets, &versions, "26.2"));
        std::fs::create_dir_all(versions.join("26.2/extracted/assets")).unwrap();
        assert!(!is_installed(&assets, &versions, "26.2"));
        std::fs::write(versions.join("26.2/extracted/.extracted"), "26.2").unwrap();
        assert!(is_installed(&assets, &versions, "26.2"));
        assert!(!assets.join("indexes/26.2.json.part").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
