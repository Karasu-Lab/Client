
use std::collections::HashMap;

use azalea_core::registry_holder::{RegistryHolder, RegistryType};
use azalea_registry::identifier::Identifier;
use pomme_singleplayer::generated::GeneratedWorld;
use simdnbt::owned::{NbtCompound, NbtTag};

use super::{Dimension, WorldSource};
use crate::renderer::chunk::mesher::BiomeClimate;
use crate::world::block::model::CardinalLightType;
use crate::world::environment::Environment;
use crate::world::{environment_wire, registry_wire};

pub use pomme_singleplayer::generated::{PROTOCOL, ThreadSpawner, install_thread_spawner};

pub const VERSION: &str = "26.3";

pub struct SteelWorld {
    world: GeneratedWorld,
    generation: std::sync::Mutex<()>,
    dimension: Dimension,
    environment: Environment,
    climate: HashMap<u32, BiomeClimate>,
    biome_count: usize,
}

impl SteelWorld {
    pub fn open(seed: i64, threads: Option<usize>) -> Result<Self, String> {
        let world = GeneratedWorld::open_overworld(seed, threads)?;
        let registries = world.registries()?;

        let mut holder = RegistryHolder::default();
        let mut timelines = RegistryType::<NbtCompound>::default();
        for (id, nbt) in registries.timelines {
            timelines.map.insert(Identifier::new(&id), nbt);
        }
        holder
            .extra
            .insert(environment_wire::TIMELINE_REGISTRY.into(), timelines);
        let mut biomes = RegistryType::<NbtCompound>::default();
        for (id, nbt) in registries.biomes {
            biomes.map.insert(Identifier::new(&id), nbt);
        }
        holder
            .extra
            .insert(Identifier::new("minecraft:worldgen/biome"), biomes);

        let extra: HashMap<String, NbtTag> = registries
            .dimension_type
            .iter()
            .map(|(key, tag)| (key.to_str().into_owned(), tag.clone()))
            .collect();
        let string = |key: &str| {
            extra
                .get(key)
                .and_then(NbtTag::string)
                .map(|value| value.to_str().into_owned())
        };
        let flag = |key: &str| extra.get(key).and_then(NbtTag::byte).map(|b| b != 0);
        let has_skylight = world.has_skylight();
        let has_weather = has_skylight && !flag("has_ceiling").unwrap_or(false);
        let dimension = Dimension {
            min_y: world.min_y(),
            height: u32::try_from(world.height()).map_err(|_| "negative height".to_owned())?,
            has_skylight,
            cardinal_light: match string("cardinal_light").as_deref() {
                Some("nether") => CardinalLightType::Nether,
                _ => CardinalLightType::Default,
            },
            ambient_light: extra
                .get("ambient_light")
                .and_then(NbtTag::float)
                .unwrap_or(0.0),
            flat: false,
        };
        let environment = environment_wire::environment(&extra, &holder, has_weather);
        Ok(Self {
            world,
            generation: std::sync::Mutex::new(()),
            dimension,
            environment,
            climate: registry_wire::extract_biome_climate(&holder),
            biome_count: registry_wire::biome_count(&holder),
        })
    }
}

impl WorldSource for SteelWorld {
    fn dimension(&self) -> Dimension {
        self.dimension
    }

    fn biome_count(&self) -> usize {
        self.biome_count
    }

    fn biome_climate(&self) -> HashMap<u32, BiomeClimate> {
        self.climate.clone()
    }

    fn environment(&self) -> Environment {
        self.environment.clone()
    }

    fn chunk_packet(&self, x: i32, z: i32) -> Result<Vec<u8>, String> {
        let _one_at_a_time = self
            .generation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.world.chunk_packet(x, z)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::environment::Skybox;
    use crate::world::generated;

    #[test]
    #[ignore = "generates a full chunk"]
    fn steel_world_covers_the_viewer() {
        let source = SteelWorld::open(8_675_309, None).expect("the world opens");

        let dimension = source.dimension();
        assert_eq!(dimension.min_y, -64);
        assert_eq!(dimension.height, 384);
        assert!(dimension.has_skylight);

        let biome_count = source.biome_count();
        assert!(biome_count > 0, "the biome registry is not empty");
        let climate = source.biome_climate();
        assert_eq!(climate.len(), biome_count);
        for id in 0..biome_count as u32 {
            assert!(climate.contains_key(&id), "biome {id} has no climate");
        }

        let environment = source.environment();
        assert_eq!(environment.skybox, Skybox::Overworld);
        assert!(environment.has_weather);
        assert!((environment.plains as usize) < biome_count);

        let packet = source.chunk_packet(0, 0).expect("the column generates");
        let chunk = generated::decode(&packet).expect("the packet decodes");
        assert_eq!(chunk.pos.x, 0);
        assert_eq!(chunk.pos.z, 0);
        assert!(!chunk.sections.is_empty());
    }
}
