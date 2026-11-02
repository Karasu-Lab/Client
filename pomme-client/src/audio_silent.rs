//! The viewer's audio: the engine's surface with no output device behind it,
//! since a build without the `game` feature has neither OpenAL nor a decoder.

use std::path::Path;

use crate::assets::AssetIndex;
use crate::entity::components::Position;
use crate::resource_pack::ResourcePackManager;

/// Sound categories, matching the protocol `SoundSource` order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SoundCategory {
    Master = 0,
    Music = 1,
    Records = 2,
    Weather = 3,
    Blocks = 4,
    Hostile = 5,
    Neutral = 6,
    Players = 7,
    Ambient = 8,
    Voice = 9,
    Ui = 10,
}

pub const CATEGORY_BLOCKS: u8 = SoundCategory::Blocks as u8;
pub const CATEGORY_PLAYERS: u8 = SoundCategory::Players as u8;
pub const CATEGORY_NEUTRAL: u8 = SoundCategory::Neutral as u8;
pub const CATEGORY_AMBIENT: u8 = SoundCategory::Ambient as u8;

impl SoundCategory {
    pub const COUNT: usize = Self::Ui as usize + 1;

    pub fn from_index(index: u8) -> Self {
        match index {
            1 => Self::Music,
            2 => Self::Records,
            3 => Self::Weather,
            4 => Self::Blocks,
            5 => Self::Hostile,
            6 => Self::Neutral,
            7 => Self::Players,
            8 => Self::Ambient,
            9 => Self::Voice,
            10 => Self::Ui,
            _ => Self::Master,
        }
    }
}

/// A sound-event identifier.
#[derive(Clone)]
pub struct SoundRef(String);

impl SoundRef {
    pub fn event(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    pub fn from_id(id: &str) -> Self {
        Self::event(id.strip_prefix("minecraft:").unwrap_or(id))
    }

    pub fn resolve(
        holder: &azalea_registry::Holder<
            azalea_registry::builtin::SoundEvent,
            azalea_core::sound::CustomSound,
        >,
    ) -> Self {
        match holder {
            azalea_registry::Holder::Reference(event) => Self::from_id(event.to_str()),
            azalea_registry::Holder::Direct(custom) => Self::from_id(&custom.sound_id.to_string()),
        }
    }

    pub(crate) fn event_name(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug)]
pub struct EntitySoundTarget {
    pub id: i32,
    pub pos: Position,
}

pub struct QueuedSubtitle {
    pub key: String,
    pub pos: Position,
    pub range: f32,
}

#[derive(Default)]
pub struct AudioEngine;

impl AudioEngine {
    pub fn new(
        _jar_assets_dir: &Path,
        _asset_index: Option<AssetIndex>,
        _packs: &ResourcePackManager,
        _volumes: [f32; SoundCategory::COUNT],
    ) -> Self {
        Self
    }

    pub fn set_volumes(&mut self, _volumes: [f32; SoundCategory::COUNT]) {}

    pub fn reload_assets(&mut self, _packs: &ResourcePackManager) {}

    pub fn set_listener(&mut self, _pos: Position, _y_rot_deg: f32, _x_rot_deg: f32) {}

    pub fn set_subtitles_enabled(&mut self, _enabled: bool) {}

    pub fn take_subtitle_events(&mut self) -> Vec<QueuedSubtitle> {
        Vec::new()
    }

    pub fn play_ui_click(&self) {}

    pub fn play_ui_sound(&self, _event: &str, _volume: f32, _pitch: f32) {}

    pub fn play_world_sound(
        &self,
        _sound_ref: &SoundRef,
        _category: u8,
        _pos: Position,
        _volume: f32,
        _pitch: f32,
        _seed: u64,
    ) {
    }

    pub fn play_entity_sound(
        &mut self,
        _sound_ref: &SoundRef,
        _category: u8,
        _target: EntitySoundTarget,
        _volume: f32,
        _pitch: f32,
        _seed: u64,
    ) -> Option<u64> {
        None
    }

    pub fn stop(&self, _id: u64) {}

    pub fn update_entity_sound_position(&self, _entity_id: i32, _pos: Position) {}

    pub fn stop_entity_sounds(&mut self, _entity_id: i32) {}

    pub fn start_menu_music(&mut self) {}

    pub fn stop_menu_music(&mut self) {}

    pub fn stop_all_sounds(&mut self) {}

    pub fn stop_sounds(&mut self, _sound_id: Option<&str>, _category: Option<u8>) {}

    pub fn poll_events(&mut self) {}

    pub fn update_menu_music(&mut self, _dt: f32) {}
}
