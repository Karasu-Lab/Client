//! Terrain generated from a seed alone, without a server or a connection.

use simdnbt::ToNbtTag;
use simdnbt::owned::{NbtCompound, NbtList, NbtTag};
use steel_core::headless::HeadlessWorld;
pub use steel_registry::packets::CURRENT_MC_PROTOCOL as PROTOCOL;
use steel_registry::{REGISTRY, RegistryExt, TaggedRegistryExt};
pub use steel_utils::threading::{ThreadSpawner, install_thread_spawner};
use steel_utils::{BlockStateId, ChunkPos, Identifier};

/// The overworld, generated chunk by chunk on demand.
pub struct GeneratedWorld {
    world: HeadlessWorld,
}

impl GeneratedWorld {
    /// Opens the overworld for `seed` with `threads` generation workers
    /// (`None` for the default). On wasm32 a thread spawner must be installed
    /// with [`install_thread_spawner`] first.
    ///
    /// # Errors
    /// Returns an error if the world cannot be built.
    pub fn open_overworld(seed: i64, threads: Option<usize>) -> Result<Self, String> {
        let world = HeadlessWorld::open(&Identifier::vanilla_static("overworld"), seed, threads)?;
        Ok(Self { world })
    }

    /// The lowest block y of the dimension.
    #[must_use]
    pub const fn min_y(&self) -> i32 {
        self.world.min_y()
    }

    /// The dimension's height in blocks.
    #[must_use]
    pub const fn height(&self) -> i32 {
        self.world.height()
    }

    /// Whether the dimension has a sky-light layer.
    #[must_use]
    pub const fn has_skylight(&self) -> bool {
        self.world.has_skylight()
    }

    /// Generates the column `(x, z)` and returns its level-chunk packet body,
    /// which `pomme-client`'s `world::generated::decode` reads.
    ///
    /// Blocks until done, so call it from a worker.
    ///
    /// # Errors
    /// Returns an error if generation fails.
    pub fn chunk_packet(&self, x: i32, z: i32) -> Result<Vec<u8>, String> {
        self.world.chunk_packet(ChunkPos::new(x, z))
    }
}

/// What the registry sync tells a client about the overworld: the entries a
/// client reads the dimension's environment and its biomes from.
pub struct DimensionRegistries {
    /// The dimension type, its `timelines` tag resolved to the ids it holds.
    pub dimension_type: NbtCompound,
    /// Every timeline, by id.
    pub timelines: Vec<(String, NbtCompound)>,
    /// Every biome in protocol-id order, which chunk sections name them by.
    pub biomes: Vec<(String, NbtCompound)>,
}

fn compound(tag: NbtTag, what: &str) -> Result<NbtCompound, String> {
    match tag {
        NbtTag::Compound(compound) => Ok(compound),
        _ => Err(format!("{what} is not a compound")),
    }
}

impl GeneratedWorld {
    /// The overworld's registry entries, as SteelMC would sync them.
    ///
    /// # Errors
    /// Returns an error if the dimension type is missing or an entry is not a
    /// compound.
    pub fn registries(&self) -> Result<DimensionRegistries, String> {
        let key = Identifier::vanilla_static("overworld");
        let dimension = REGISTRY
            .dimension_types
            .by_key(&key)
            .ok_or("the overworld has no dimension type")?;
        let mut dimension_type = compound(dimension.to_nbt_tag(), "the dimension type")?;
        if let Some(NbtTag::String(reference)) = dimension_type.get("timelines") {
            let reference = reference.to_string();
            if let Some(tag) = reference.strip_prefix('#') {
                let tag: Identifier = tag.parse().map_err(|_| format!("bad tag {tag}"))?;
                let ids = REGISTRY
                    .timelines
                    .iter_tag(&tag)
                    .map(|timeline| timeline.key.to_string().as_str().into())
                    .collect();
                dimension_type.insert("timelines", NbtTag::List(NbtList::String(ids)));
            }
        }
        let timelines = REGISTRY
            .timelines
            .iter()
            .map(|(_, timeline)| {
                Ok((
                    timeline.key.to_string(),
                    compound(timeline.to_nbt_tag(), "a timeline")?,
                ))
            })
            .collect::<Result<_, String>>()?;
        let biomes = REGISTRY
            .biomes
            .iter()
            .map(|(_, biome)| {
                Ok((
                    biome.key.to_string(),
                    compound(biome.to_nbt_tag(), "a biome")?,
                ))
            })
            .collect::<Result<_, String>>()?;
        Ok(DimensionRegistries {
            dimension_type,
            timelines,
            biomes,
        })
    }
}

/// What a block state id stands for: its block and the property values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateDescription {
    pub namespace: String,
    pub path: String,
    pub properties: Vec<(String, String)>,
}

impl GeneratedWorld {
    /// Generates the column `(x, z)` and returns every block state id in it,
    /// indexed x fastest, then z, then y upwards from [`Self::min_y`].
    ///
    /// # Errors
    /// Returns an error if generation fails.
    pub fn block_states(&self, x: i32, z: i32) -> Result<Vec<u16>, String> {
        let states = self.world.block_states(ChunkPos::new(x, z))?;
        Ok(states.into_iter().map(|state| state.0).collect())
    }

    /// The block and properties behind a state id from [`Self::block_states`].
    #[must_use]
    pub fn describe(&self, state: u16) -> Option<StateDescription> {
        let id = BlockStateId(state);
        let block = REGISTRY.blocks.by_state_id(id)?;
        Some(StateDescription {
            namespace: block.key.namespace.to_string(),
            path: block.key.path.to_string(),
            properties: REGISTRY
                .blocks
                .properties(id)
                .map(|(name, value)| (name.to_owned(), value.to_owned()))
                .collect(),
        })
    }
}
