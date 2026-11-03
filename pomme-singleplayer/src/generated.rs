//! Terrain generated from a seed alone, without a server or a connection.

use steel_core::headless::HeadlessWorld;
use steel_registry::REGISTRY;
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
                .get_properties(id)
                .into_iter()
                .map(|(name, value)| (name.to_owned(), value.to_owned()))
                .collect(),
        })
    }
}
