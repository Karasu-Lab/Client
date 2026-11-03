use std::path::{Path, PathBuf};

use resourcepack::{PackLocation, PackStack};

use crate::assets::AssetIndex;

pub const CURRENT_PACK_FORMAT: u32 = 84;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PackCompat {
    Compatible,
    TooOld,
    TooNew,
}

#[derive(Clone)]
pub struct PackInfo {
    pub name: String,
    pub description: String,
    pub compat: PackCompat,
    pub source: PackSource,
    pub enabled: bool,
}

#[derive(Clone, PartialEq, Eq)]
pub enum PackSource {
    Server,
    Local,
}

struct ActivePack {
    id: String,
    source: PackSource,
    dir: PathBuf,
    info: PackInfo,
}

/// The id of the built-in layer, which sits below every pack.
const BUILTIN_ID: &str = "builtin";

/// The jar's `assets/` and the asset index: one built-in pack, the index first.
struct BuiltinAssets {
    jar_assets_dir: PathBuf,
    asset_index: Option<AssetIndex>,
}

impl BuiltinAssets {
    fn resolve(&self, asset_key: &str) -> Option<PathBuf> {
        crate::assets::builtin_asset(&self.jar_assets_dir, &self.asset_index, asset_key)
    }
}

/// One layer of the stack: the built-in resources at the bottom, then packs.
enum Layer {
    Builtin(BuiltinAssets),
    Pack(ActivePack),
}

impl Layer {
    fn pack(&self) -> Option<&ActivePack> {
        match self {
            Layer::Pack(pack) => Some(pack),
            Layer::Builtin(_) => None,
        }
    }
}

pub struct ResourcePackManager {
    packs_dir: PathBuf,
    server_cache_dir: PathBuf,
    active_packs: PackStack<Layer>,
    available_local: Vec<PackInfo>,
}

impl ResourcePackManager {
    /// Packs live in `resourcepacks` inside `instance_dir`.
    pub fn new(instance_dir: &Path) -> Self {
        Self::with_location(instance_dir, &PackLocation::Default)
    }

    /// Packs live where `location` says; a relative directory is taken from
    /// `instance_dir`.
    pub fn with_location(instance_dir: &Path, location: &PackLocation) -> Self {
        let packs_dir = location.resolve(instance_dir);
        let server_cache_dir = packs_dir.join(".server_cache");
        let _ = pomme_vfs::create_dir_all(&packs_dir);
        let _ = pomme_vfs::create_dir_all(&server_cache_dir);
        let mut mgr = Self {
            packs_dir,
            server_cache_dir,
            active_packs: PackStack::new(),
            available_local: Vec::new(),
        };
        mgr.scan_local_packs();
        mgr
    }

    pub fn packs_dir(&self) -> &Path {
        &self.packs_dir
    }

    /// The active packs, from the lowest priority to the highest; the built-in
    /// layer is not one of them.
    fn packs_ascending(&self) -> impl DoubleEndedIterator<Item = &ActivePack> {
        self.active_packs
            .ascending()
            .filter_map(|layer| layer.pack.pack())
    }

    /// Makes the jar's assets and the asset index the layer below every pack,
    /// replacing the one set before.
    pub fn set_builtin(&mut self, jar_assets_dir: PathBuf, asset_index: Option<AssetIndex>) {
        self.active_packs.insert(
            BUILTIN_ID,
            i32::MIN,
            Layer::Builtin(BuiltinAssets {
                jar_assets_dir,
                asset_index,
            }),
        );
    }

    /// The path of the pack that wins over every pack below it; the built-in
    /// layer is not asked.
    pub fn resolve_asset(&self, asset_key: &str) -> Option<PathBuf> {
        if !crate::assets::valid_asset_key(asset_key) {
            return None;
        }
        self.packs_ascending().rev().find_map(|pack| {
            let path = pack.dir.join("assets").join(asset_key);
            pomme_vfs::exists(&path).then_some(path)
        })
    }

    /// The path of the topmost layer holding `asset_key`, the built-in layer
    /// included. `builtin` stands in for that layer while none is set.
    pub fn resolve_layered(
        &self,
        asset_key: &str,
        builtin: impl FnOnce() -> Option<PathBuf>,
    ) -> Option<PathBuf> {
        let found = self.active_packs.descending().find_map(|layer| match &layer.pack {
            Layer::Builtin(builtin) => builtin.resolve(asset_key),
            Layer::Pack(pack) => {
                let path = pack.dir.join("assets").join(asset_key);
                pomme_vfs::exists(&path).then_some(path)
            }
        });
        found.or_else(|| (!self.has_builtin()).then(builtin).flatten())
    }

    /// Every layer's path for `asset_key`, from the built-in layer up to the
    /// highest pack. `builtin` stands in for the built-in layer while none is
    /// set.
    pub fn layered_paths(
        &self,
        asset_key: &str,
        builtin: impl FnOnce() -> Option<PathBuf>,
    ) -> Vec<PathBuf> {
        let mut paths: Vec<PathBuf> = (!self.has_builtin())
            .then(builtin)
            .flatten()
            .into_iter()
            .collect();
        paths.extend(self.active_packs.ascending().filter_map(|layer| {
            match &layer.pack {
                Layer::Builtin(builtin) => builtin.resolve(asset_key),
                Layer::Pack(pack) => {
                    let path = pack.dir.join("assets").join(asset_key);
                    pomme_vfs::is_file(&path).then_some(path)
                }
            }
        }));
        paths
    }

    fn has_builtin(&self) -> bool {
        self.active_packs.get(BUILTIN_ID).is_some()
    }

    /// Active pack roots in low-to-high priority order, matching the order in
    /// which stacked resources are registered by Vanilla.
    pub fn active_pack_dirs(&self) -> impl Iterator<Item = &Path> {
        self.packs_ascending().map(|pack| pack.dir.as_path())
    }

    /// Moves an active pack to `priority`; a higher one overrides a lower one,
    /// and among equal priorities the pack enabled last wins. The built-in
    /// layer stays at the bottom.
    pub fn set_pack_priority(&mut self, id: &str, priority: i32) -> bool {
        id != BUILTIN_ID && self.active_packs.set_priority(id, priority)
    }

    pub fn pack_priority(&self, id: &str) -> Option<i32> {
        self.active_packs.priority(id)
    }

    /// Moves the pack `by` places along [`Self::active_pack_info`]'s order,
    /// towards the highest priority when positive. `id` is the pack's id, or
    /// the name its info shows. False if there is no such pack or the place is
    /// past either end.
    pub fn move_pack(&mut self, id: &str, by: isize) -> bool {
        let mut ids: Vec<String> = self.packs_ascending().map(|pack| pack.id.clone()).collect();
        let Some(from) = ids
            .iter()
            .position(|candidate| candidate == id)
            .or_else(|| self.packs_ascending().position(|pack| pack.info.name == id))
        else {
            return false;
        };
        let Some(to) = from
            .checked_add_signed(by)
            .filter(|&to| to < ids.len())
        else {
            return false;
        };
        let moved = ids.remove(from);
        ids.insert(to, moved);
        for (priority, id) in ids.iter().enumerate() {
            self.active_packs.set_priority(id, priority as i32);
        }
        true
    }

    /// Resolve a resource-pack asset together with the metadata sidecar that
    /// vanilla would expose for that resource. Metadata may come from the same
    /// pack or a higher-priority pack, but never from below the pack that
    /// supplied the resource itself.
    pub fn resolve_asset_with_metadata(
        &self,
        asset_key: &str,
    ) -> Option<(PathBuf, Option<PathBuf>)> {
        let metadata_key = format!("{asset_key}.mcmeta");
        let layers: Vec<_> = self.packs_ascending().collect();
        for (source_index, pack) in layers.iter().enumerate().rev() {
            let path = pack.dir.join("assets").join(asset_key);
            if !pomme_vfs::exists(&path) {
                continue;
            }
            let metadata = layers[source_index..]
                .iter()
                .rev()
                .map(|candidate| candidate.dir.join("assets").join(&metadata_key))
                .find(|candidate| pomme_vfs::exists(&candidate));
            return Some((path, metadata));
        }
        None
    }

    fn server_pack_dir(&self, hash: &str) -> PathBuf {
        self.server_cache_dir.join(hash)
    }

    #[cfg(feature = "game")]
    pub fn download_server_pack(
        server_cache_dir: &Path,
        url: &str,
        hash: &str,
    ) -> Result<PathBuf, PackError> {
        let dir = server_cache_dir.join(hash);
        if !pomme_vfs::is_dir(&dir) {
            let data = crate::net::http::blocking_builder()
                .build()
                .and_then(|client| client.get(url).send())
                .map_err(|e| PackError::Download(e.to_string()))?
                .bytes()
                .map_err(|e| PackError::Download(e.to_string()))?;
            tracing::info!("Downloaded {} bytes", data.len());
            validate_hash(&data, hash)?;
            extract_zip(&data, &dir)?;
        }
        Ok(dir)
    }

    pub fn apply_server_pack(&mut self, id: uuid::Uuid, hash: &str) {
        let pack_id = id.to_string();
        let dir = self.server_pack_dir(hash);
        let info = parse_pack_meta_dir(&dir, hash);
        self.active_packs.push(
            pack_id.clone(),
            Layer::Pack(ActivePack {
                id: pack_id,
                source: PackSource::Server,
                dir,
                info: PackInfo {
                    enabled: true,
                    source: PackSource::Server,
                    ..info
                },
            }),
        );
        tracing::info!("Applied server resource pack {id} (hash: {hash})");
    }

    pub fn remove_server_pack(&mut self, id: &uuid::Uuid) -> bool {
        let id_str = id.to_string();
        let before = self.active_packs.len();
        self.active_packs.retain(|l| {
            l.pack
                .pack()
                .is_none_or(|p| !(p.id == id_str && p.source == PackSource::Server))
        });
        let removed = self.active_packs.len() < before;
        if removed {
            tracing::info!("Removed server resource pack {id}");
        }
        removed
    }

    pub fn clear_server_packs(&mut self) -> bool {
        let before = self.active_packs.len();
        self.active_packs
            .retain(|l| l.pack.pack().is_none_or(|p| p.source != PackSource::Server));
        let removed = self.active_packs.len() != before;
        if removed {
            tracing::info!("Cleared all server resource packs");
        }
        removed
    }

    pub fn scan_local_packs(&mut self) {
        self.available_local.clear();
        let entries = match pomme_vfs::read_dir(&self.packs_dir) {
            Ok(e) => e,
            Err(_) => return,
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path == self.server_cache_dir {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();

            if pomme_vfs::is_dir(&path) {
                if pomme_vfs::exists(path.join("pack.mcmeta")) {
                    let already_active = self
                        .packs_ascending()
                        .any(|p| p.source == PackSource::Local && p.dir == path);
                    let mut info = parse_pack_meta_dir(&path, &name);
                    info.enabled = already_active;
                    info.source = PackSource::Local;
                    self.available_local.push(info);
                }
            } else if path.extension().is_some_and(|e| e == "zip")
                && let Some(mut info) = parse_pack_meta_zip(&path, &name)
            {
                let already_active = self
                    .packs_ascending()
                    .any(|p| p.source == PackSource::Local && p.id == name);
                info.enabled = already_active;
                info.source = PackSource::Local;
                self.available_local.push(info);
            }
        }
    }

    pub fn enable_local_pack(&mut self, name: &str) {
        let path = self.packs_dir.join(name);
        if pomme_vfs::is_dir(&path) && pomme_vfs::exists(path.join("pack.mcmeta")) {
            let info = parse_pack_meta_dir(&path, name);
            self.active_packs.push(
                name,
                Layer::Pack(ActivePack {
                    id: name.to_owned(),
                    source: PackSource::Local,
                    dir: path,
                    info: PackInfo {
                        enabled: true,
                        source: PackSource::Local,
                        ..info
                    },
                }),
            );
            tracing::info!("Enabled local resource pack: {name}");
        } else if path.extension().is_some_and(|e| e == "zip")
            && let Ok(data) = pomme_vfs::read(&path)
        {
            let extract_dir = self.server_cache_dir.join(format!("_local_{name}"));
            if let Err(e) = extract_zip(&data, &extract_dir) {
                tracing::error!("Failed to extract zip pack {name}: {e}");
                return;
            }
            let info = parse_pack_meta_dir(&extract_dir, name);
            self.active_packs.push(
                name,
                Layer::Pack(ActivePack {
                    id: name.to_owned(),
                    source: PackSource::Local,
                    dir: extract_dir,
                    info: PackInfo {
                        enabled: true,
                        source: PackSource::Local,
                        ..info
                    },
                }),
            );
            tracing::info!("Enabled local resource pack: {name}");
        }
        self.scan_local_packs();
    }

    pub fn disable_local_pack(&mut self, name: &str) {
        self.active_packs.retain(|l| {
            l.pack
                .pack()
                .is_none_or(|p| !(p.id == name && p.source == PackSource::Local))
        });
        tracing::info!("Disabled local resource pack: {name}");
        self.scan_local_packs();
    }

    pub fn active_pack_info(&self) -> Vec<PackInfo> {
        self.packs_ascending().map(|p| p.info.clone()).collect()
    }

    pub fn available_local_packs(&self) -> &[PackInfo] {
        &self.available_local
    }

    pub fn server_cache_dir(&self) -> &Path {
        &self.server_cache_dir
    }
}

/// Why [`prepare_location`] could not make a pack location ready.
#[derive(Debug)]
pub enum LocationError {
    /// This platform has no such place: a base URL and a storage namespace are
    /// for the browser.
    Unsupported(PackLocation),
    Fetch(String),
    Storage(String),
}

impl std::fmt::Display for LocationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported(location) => write!(
                f,
                "the resource pack location `{}` only works in the browser",
                location.to_setting()
            ),
            Self::Fetch(e) => write!(f, "fetching the resource packs failed: {e}"),
            Self::Storage(e) => write!(f, "the browser's pack storage failed: {e}"),
        }
    }
}

impl std::error::Error for LocationError {}

/// Makes the directory `location` resolves to ready for a
/// [`ResourcePackManager`], and returns it.
///
/// A directory is created if it is missing. In the browser a storage namespace
/// is read back into the file tree, and a base URL is fetched into it (see
/// `pomme_install::packs`), falling back on what an earlier visit stored when
/// the fetch fails. Elsewhere those two are an error.
pub async fn prepare_location(
    location: &PackLocation,
    game_dir: &Path,
) -> Result<PathBuf, LocationError> {
    let dir = location.resolve(game_dir);
    match location {
        PackLocation::Default | PackLocation::Directory(_) => {
            pomme_vfs::create_dir_all(&dir).map_err(|e| LocationError::Storage(e.to_string()))?;
            Ok(dir)
        }
        #[cfg(not(target_arch = "wasm32"))]
        PackLocation::Namespace(_) | PackLocation::Url(_) => {
            Err(LocationError::Unsupported(location.clone()))
        }
        #[cfg(target_arch = "wasm32")]
        PackLocation::Namespace(namespace) => {
            pomme_vfs::browser::restore(namespace)
                .await
                .map_err(LocationError::Storage)?;
            pomme_vfs::create_dir_all(&dir).map_err(|e| LocationError::Storage(e.to_string()))?;
            Ok(dir)
        }
        #[cfg(target_arch = "wasm32")]
        PackLocation::Url(base) => {
            let cache = format!("packs:{base}");
            match pomme_install::packs::sync_packs(base, &dir).await {
                Ok(_) => pomme_vfs::browser::persist_dir(&cache, &dir)
                    .await
                    .map_err(LocationError::Storage)?,
                Err(fetch_error) => {
                    let stored = pomme_vfs::browser::restore(&cache).await.unwrap_or(0);
                    if stored == 0 {
                        return Err(LocationError::Fetch(fetch_error));
                    }
                }
            }
            pomme_vfs::create_dir_all(&dir).map_err(|e| LocationError::Storage(e.to_string()))?;
            Ok(dir)
        }
    }
}

#[derive(Debug)]
pub enum PackError {
    Download(String),
    HashMismatch,
    Extract(String),
}

impl std::fmt::Display for PackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Download(e) => write!(f, "download failed: {e}"),
            Self::HashMismatch => write!(f, "SHA-1 hash mismatch"),
            Self::Extract(e) => write!(f, "extraction failed: {e}"),
        }
    }
}

#[cfg(feature = "game")]
fn validate_hash(data: &[u8], expected: &str) -> Result<(), PackError> {
    if expected.is_empty() {
        return Ok(());
    }
    let actual = sha1_smol::Sha1::from(data).digest().to_string();
    if actual != expected {
        tracing::error!("Hash mismatch: expected {expected}, got {actual}");
        return Err(PackError::HashMismatch);
    }
    Ok(())
}

fn parse_meta_value(v: &serde_json::Value, fallback: &str) -> (String, String, PackCompat) {
    let pack = v.get("pack");

    let description = pack
        .and_then(|p| p.get("description"))
        .and_then(|d| d.as_str())
        .unwrap_or(fallback)
        .to_owned();

    let name = pack
        .and_then(|p| p.get("name"))
        .and_then(|n| n.as_str())
        .unwrap_or(fallback)
        .to_owned();

    let compat = pack
        .map(|p| {
            let (min, max) = parse_format_range(p);
            if CURRENT_PACK_FORMAT < min {
                PackCompat::TooNew
            } else if CURRENT_PACK_FORMAT > max {
                PackCompat::TooOld
            } else {
                PackCompat::Compatible
            }
        })
        .unwrap_or(PackCompat::Compatible);

    (name, description, compat)
}

fn parse_pack_meta_dir(dir: &Path, fallback_name: &str) -> PackInfo {
    let meta_path = dir.join("pack.mcmeta");
    let Some(v) = pomme_vfs::read_to_string(&meta_path)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
    else {
        return PackInfo {
            name: fallback_name.to_owned(),
            description: fallback_name.to_owned(),
            compat: PackCompat::Compatible,
            source: PackSource::Local,
            enabled: false,
        };
    };

    let (name, description, compat) = parse_meta_value(&v, fallback_name);
    PackInfo {
        name,
        description,
        compat,
        source: PackSource::Local,
        enabled: false,
    }
}

fn parse_pack_meta_zip(path: &Path, fallback_name: &str) -> Option<PackInfo> {
    let data = pomme_vfs::read(path).ok()?;
    let cursor = std::io::Cursor::new(data);
    let mut archive = zip::ZipArchive::new(cursor).ok()?;
    let mut mcmeta = archive.by_name("pack.mcmeta").ok()?;
    let mut contents = String::new();
    std::io::Read::read_to_string(&mut mcmeta, &mut contents).ok()?;
    let v: serde_json::Value = serde_json::from_str(&contents).ok()?;

    let (name, description, compat) = parse_meta_value(&v, fallback_name);
    Some(PackInfo {
        name,
        description,
        compat,
        source: PackSource::Local,
        enabled: false,
    })
}

fn parse_format_range(pack: &serde_json::Value) -> (u32, u32) {
    if let (Some(min), Some(max)) = (pack.get("min_format"), pack.get("max_format")) {
        let min_v = format_value(min);
        let max_v = format_value(max);
        if min_v > 0 && max_v > 0 {
            return (min_v, max_v);
        }
    }

    if let Some(supported) = pack.get("supported_formats") {
        if let Some(arr) = supported.as_array()
            && arr.len() == 2
        {
            return (
                arr[0].as_u64().unwrap_or(0) as u32,
                arr[1].as_u64().unwrap_or(0) as u32,
            );
        }
        if let Some(obj) = supported.as_object() {
            let min = obj
                .get("min_inclusive")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32;
            let max = obj
                .get("max_inclusive")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32;
            return (min, max);
        }
        if let Some(n) = supported.as_u64() {
            return (n as u32, n as u32);
        }
    }

    if let Some(fmt) = pack.get("pack_format").and_then(|v| v.as_u64()) {
        return (fmt as u32, fmt as u32);
    }

    (0, u32::MAX)
}

fn format_value(v: &serde_json::Value) -> u32 {
    if let Some(n) = v.as_u64() {
        return n as u32;
    }
    if let Some(arr) = v.as_array()
        && let Some(major) = arr.first().and_then(|v| v.as_u64())
    {
        return major as u32;
    }
    0
}

fn extract_zip(data: &[u8], dest: &Path) -> Result<(), PackError> {
    let _ = pomme_vfs::create_dir_all(dest);
    let cursor = std::io::Cursor::new(data);
    let mut archive =
        zip::ZipArchive::new(cursor).map_err(|e| PackError::Extract(e.to_string()))?;

    for i in 0..archive.len() {
        let mut file = archive
            .by_index(i)
            .map_err(|e| PackError::Extract(e.to_string()))?;

        let Some(enclosed) = file.enclosed_name() else {
            continue;
        };
        let out_path = dest.join(enclosed);

        if file.is_dir() {
            let _ = pomme_vfs::create_dir_all(&out_path);
        } else {
            if let Some(parent) = out_path.parent() {
                let _ = pomme_vfs::create_dir_all(parent);
            }
            let mut out =
                std::fs::File::create(&out_path).map_err(|e| PackError::Extract(e.to_string()))?;
            std::io::copy(&mut file, &mut out).map_err(|e| PackError::Extract(e.to_string()))?;
        }
    }

    tracing::info!("Extracted resource pack to {}", dest.display());
    Ok(())
}
