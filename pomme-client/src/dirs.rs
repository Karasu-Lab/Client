use std::path::{Path, PathBuf};

use resourcepack::PackLocation;

/// The file in the game directory that holds where the resource packs are
/// kept, in [`PackLocation::to_setting`]'s form.
pub const RESOURCE_PACK_LOCATION_FILE: &str = "resource_pack_location.txt";

#[derive(Clone)]
pub struct DataDirs {
    pub assets_dir: PathBuf,
    pub versions_dir: PathBuf,
    pub indexes_dir: PathBuf,
    pub objects_dir: PathBuf,
    pub pomme_assets_dir: PathBuf,
    pub bedrock_assets_dir: PathBuf,
    pub jar_assets_dir: PathBuf,
    pub game_dir: PathBuf,
    pub cache_dir: PathBuf,
    /// Where the resource packs are kept; `resourcepacks` in `game_dir` unless
    /// changed.
    pub resource_pack_location: PackLocation,
}

impl DataDirs {
    pub fn resolve(
        version: &str,
        assets_dir: Option<&str>,
        versions_dir: Option<&str>,
        game_dir: Option<&str>,
    ) -> Self {
        let layout = pomme_dirs::layout();

        let assets_dir = assets_dir
            .map(PathBuf::from)
            .unwrap_or_else(|| layout.minecraft_assets());

        let game_dir = game_dir
            .map(PathBuf::from)
            .unwrap_or_else(|| layout.default_profile());

        let versions_dir = versions_dir
            .map(PathBuf::from)
            .unwrap_or_else(|| layout.minecraft_versions());

        let jar_assets_dir = versions_dir.join(version).join("extracted/assets");

        let indexes_dir = assets_dir.join("indexes");
        let objects_dir = assets_dir.join("objects");

        Self {
            assets_dir,
            versions_dir,
            indexes_dir,
            objects_dir,
            jar_assets_dir,
            pomme_assets_dir: layout.pomme_assets(),
            bedrock_assets_dir: layout.bedrock_assets(),
            game_dir,
            cache_dir: layout.cache(version),
            resource_pack_location: PackLocation::default(),
        }
    }

    pub fn with_resource_pack_location(mut self, location: PackLocation) -> Self {
        self.resource_pack_location = location;
        self
    }

    /// The location saved in `game_dir`, if there is a valid one.
    pub fn saved_resource_pack_location(game_dir: &Path) -> Option<PackLocation> {
        let path = game_dir.join(RESOURCE_PACK_LOCATION_FILE);
        let text = pomme_vfs::read_to_string(&path).ok()?;
        match PackLocation::parse(&text) {
            Ok(location) => Some(location),
            Err(error) => {
                tracing::warn!("{}: {error}", path.display());
                None
            }
        }
    }

    /// Saves `location` in `game_dir` for [`Self::saved_resource_pack_location`].
    pub fn save_resource_pack_location(
        game_dir: &Path,
        location: &PackLocation,
    ) -> std::io::Result<()> {
        pomme_vfs::create_dir_all(game_dir)?;
        pomme_vfs::write(
            game_dir.join(RESOURCE_PACK_LOCATION_FILE),
            location.to_setting().as_bytes(),
        )
    }

    pub fn ensure_game_dir(&self) -> std::io::Result<()> {
        pomme_dirs::create_profile(&self.game_dir)
    }

    /// `version`'s assets are downloaded, so the client can start on them.
    pub fn is_installed(&self, version: &str) -> bool {
        pomme_install::is_installed(&self.assets_dir, &self.versions_dir, version)
    }

    /// The downloaded Bedrock UI textures the Bedrock theme draws.
    pub fn bedrock_ui_dir(&self) -> PathBuf {
        pomme_install::bedrock::dir(&self.bedrock_assets_dir).join(pomme_install::bedrock::UI_DIR)
    }
}
