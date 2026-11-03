//! The assets a viewer draws with: fetched once into the virtual file system
//! and, in the browser, kept in its storage for the next visit.

pub use pomme_install::{Endpoints, Progress};
use resourcepack::PackLocation;

use super::ViewerDirs;
use crate::assets::AssetIndex;
use crate::dirs::DataDirs;
use crate::resource_pack::prepare_location;

/// Where the browser keeps the downloaded assets between visits.
#[cfg(target_arch = "wasm32")]
const ASSETS_NAMESPACE: &str = "viewer-assets";
#[cfg(target_arch = "wasm32")]
const VERSIONS_NAMESPACE: &str = "viewer-versions";

/// The asset objects a viewer needs. Sounds are most of the rest, and a viewer
/// plays none.
fn is_drawn(key: &str) -> bool {
    !key.starts_with("minecraft/sounds") && !key.starts_with("minecraft/records")
}

/// Where a viewer's assets are, once they are there.
pub struct ViewerAssets {
    pub dirs: ViewerDirs,
    pub asset_index: Option<AssetIndex>,
    /// The resource packs' location, ready to be read.
    pub resource_pack_location: PackLocation,
}

/// Makes `version`'s assets and the resource packs at `location` available:
/// restored from the browser's storage or downloaded from `endpoints`,
/// reporting through `on_progress`.
///
/// # Errors
/// Fails if the assets can't be downloaded or stored.
pub async fn prepare(
    version: &str,
    mut endpoints: Endpoints,
    resource_pack_location: PackLocation,
    on_progress: &(dyn Fn(Progress) + Send + Sync),
) -> Result<ViewerAssets, String> {
    let data = DataDirs::resolve(version, None, None, None);
    #[cfg(target_arch = "wasm32")]
    {
        pomme_vfs::browser::restore(ASSETS_NAMESPACE).await?;
        pomme_vfs::browser::restore(VERSIONS_NAMESPACE).await?;
    }
    if !data.is_installed(version) {
        endpoints.keep_object.get_or_insert(is_drawn);
        pomme_install::install_with(
            &endpoints,
            &data.assets_dir,
            &data.versions_dir,
            version,
            on_progress,
        )
        .await?;
        #[cfg(target_arch = "wasm32")]
        {
            pomme_vfs::browser::persist_dir(ASSETS_NAMESPACE, &data.assets_dir).await?;
            pomme_vfs::browser::persist_dir(VERSIONS_NAMESPACE, &data.versions_dir).await?;
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
