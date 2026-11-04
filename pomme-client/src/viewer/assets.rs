pub use pomme_install::{
    Endpoints, Fetch, Hash, HttpTransfer, Progress, Stage, Transfer, Verified,
};
use resourcepack::PackLocation;

use super::ViewerDirs;
use crate::assets::AssetIndex;
use crate::dirs::DataDirs;
use crate::resource_pack::prepare_location;

#[cfg(target_arch = "wasm32")]
pub use cache::VIEWER_ASSETS as ASSETS_NAMESPACE;
#[cfg(target_arch = "wasm32")]
use cache::VIEWER_VERSIONS as VERSIONS_NAMESPACE;

fn is_drawn(key: &str) -> bool {
    !key.starts_with("minecraft/sounds") && !key.starts_with("minecraft/records")
}

pub struct ViewerAssets {
    pub dirs: ViewerDirs,
    pub asset_index: Option<AssetIndex>,
    pub resource_pack_location: PackLocation,
}

/// How often the objects downloaded so far are snapshotted into the
/// persistent store while an install runs.
#[cfg(target_arch = "wasm32")]
const SNAPSHOT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

/// Keeps the objects an install has already downloaded in the
/// persistent store while the install still runs, so closing the page
/// mid-download does not lose them: the next visit restores them, and
/// the install's hash comparison downloads only what is still
/// missing. The index is deliberately not snapshotted — it is saved
/// last by the persist at the end of `prepare`, and its absence is
/// what makes the next visit check the objects at all.
#[cfg(target_arch = "wasm32")]
struct ObjectSnapshot {
    objects_dir: std::path::PathBuf,
    persisted: std::sync::Mutex<std::collections::HashSet<std::path::PathBuf>>,
    last: std::sync::Mutex<Option<web_time::Instant>>,
    running: std::sync::atomic::AtomicBool,
}

#[cfg(target_arch = "wasm32")]
impl ObjectSnapshot {
    fn new(objects_dir: std::path::PathBuf) -> Self {
        Self {
            objects_dir,
            persisted: std::sync::Mutex::new(std::collections::HashSet::new()),
            last: std::sync::Mutex::new(None),
            running: std::sync::atomic::AtomicBool::new(false),
        }
    }

    fn maybe_persist(self: &std::sync::Arc<Self>) {
        {
            let mut last = self.last.lock().unwrap();
            let now = web_time::Instant::now();
            if last.is_some_and(|last| now.duration_since(last) < SNAPSHOT_INTERVAL) {
                return;
            }
            *last = Some(now);
        }
        if self
            .running
            .swap(true, std::sync::atomic::Ordering::Relaxed)
        {
            return;
        }
        let snapshot = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            snapshot.persist_new().await;
            snapshot
                .running
                .store(false, std::sync::atomic::Ordering::Relaxed);
        });
    }

    async fn persist_new(&self) {
        let fresh: Vec<std::path::PathBuf> = object_files(&self.objects_dir)
            .into_iter()
            .filter(|path| !self.persisted.lock().unwrap().contains(path))
            .collect();
        for path in fresh {
            if pomme_vfs::browser::persist(ASSETS_NAMESPACE, &path)
                .await
                .is_ok()
            {
                self.persisted.lock().unwrap().insert(path);
            }
        }
    }
}

/// The object files under `objects_dir` (one folder per hash prefix),
/// skipping the `.part` files of downloads still in flight. A file at
/// its final path is always whole: downloads land there by rename.
#[cfg(target_arch = "wasm32")]
fn object_files(objects_dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    let Ok(prefixes) = pomme_vfs::read_dir(objects_dir) else {
        return files;
    };
    for prefix in prefixes.flatten() {
        let Ok(entries) = pomme_vfs::read_dir(prefix.path()) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "part") {
                continue;
            }
            if pomme_vfs::is_file(&path) {
                files.push(path);
            }
        }
    }
    files
}

#[cfg(target_arch = "wasm32")]
async fn restore_install(marker: &std::path::Path) -> Result<bool, String> {
    pomme_vfs::browser::restore_paths(VERSIONS_NAMESPACE, &[marker.to_path_buf()]).await?;
    let extracted = pomme_vfs::exists(marker);
    if extracted {
        pomme_vfs::browser::restore(VERSIONS_NAMESPACE).await?;
    }
    pomme_vfs::browser::restore(ASSETS_NAMESPACE).await?;
    Ok(extracted)
}

pub async fn prepare(
    version: &str,
    endpoints: Endpoints,
    resource_pack_location: PackLocation,
    on_progress: &(dyn Fn(Progress) + Send + Sync),
) -> Result<ViewerAssets, String> {
    prepare_with(
        &HttpTransfer::default(),
        version,
        endpoints,
        resource_pack_location,
        on_progress,
    )
    .await
}

pub async fn prepare_with<T: Transfer>(
    transfer: &T,
    version: &str,
    mut endpoints: Endpoints,
    resource_pack_location: PackLocation,
    on_progress: &(dyn Fn(Progress) + Send + Sync),
) -> Result<ViewerAssets, String> {
    let data = DataDirs::resolve(version, None, None, None);
    #[cfg(target_arch = "wasm32")]
    let (index, marker) =
        pomme_install::install_markers(&data.assets_dir, &data.versions_dir, version);
    #[cfg(target_arch = "wasm32")]
    let extracted = restore_install(&marker).await?;
    if !data.is_installed(version) {
        endpoints.keep_object.get_or_insert(is_drawn);
        #[cfg(target_arch = "wasm32")]
        let snapshot = (!transfer.stores_externally())
            .then(|| std::sync::Arc::new(ObjectSnapshot::new(data.objects_dir.clone())));
        #[cfg(target_arch = "wasm32")]
        let install_progress = {
            let snapshot = snapshot.clone();
            move |progress: Progress| {
                if let Some(snapshot) = &snapshot {
                    snapshot.maybe_persist();
                }
                on_progress(progress);
            }
        };
        #[cfg(target_arch = "wasm32")]
        let on_progress = &install_progress;
        pomme_install::install_via(
            transfer,
            &endpoints,
            &data.assets_dir,
            &data.versions_dir,
            version,
            on_progress,
        )
        .await?;
        #[cfg(target_arch = "wasm32")]
        {
            let saving = |done: u32, total: u32| {
                on_progress(Progress {
                    stage: Stage::Save,
                    done,
                    total,
                    status: format!("Saving the assets ({done}/{total})"),
                    file_done: 0,
                    file_total: 0,
                })
            };
            if !extracted {
                pomme_vfs::browser::persist_dir_with(
                    VERSIONS_NAMESPACE,
                    &data.versions_dir,
                    std::slice::from_ref(&marker),
                    &saving,
                )
                .await?;
            }
            if transfer.stores_externally() {
                pomme_vfs::browser::persist_files(ASSETS_NAMESPACE, &[index], &saving).await?;
            } else {
                pomme_vfs::browser::persist_dir_with(
                    ASSETS_NAMESPACE,
                    &data.assets_dir,
                    &[index],
                    &saving,
                )
                .await?;
            }
        }
    }
    let asset_index = AssetIndex::load(&data.indexes_dir, &data.objects_dir, version);
    prepare_location(&resource_pack_location, &data.game_dir)
        .await
        .map_err(|error| error.to_string())?;
    Ok(ViewerAssets {
        dirs: ViewerDirs {
            jar_assets_dir: data.jar_assets_dir.clone(),
            panorama_dir: data.jar_assets_dir.clone(),
            bedrock_ui_dir: data.bedrock_ui_dir(),
            game_dir: data.game_dir,
            cache_dir: data.cache_dir,
        },
        asset_index,
        resource_pack_location,
    })
}
