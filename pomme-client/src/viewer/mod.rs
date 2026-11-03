//! The viewer: the world renderer driven with no game around it. It draws one
//! level from a [`WorldSource`] through a fixed camera, with the UI off unless
//! asked for. The chunk path is the game's — columns are loaded, lit, meshed
//! and uploaded the way `GameState` does it — minus everything that needs a
//! connection, an account or a player.

pub mod assets;
mod overlay;
#[cfg(feature = "steel-world")]
pub mod steel;
#[cfg(target_arch = "wasm32")]
pub mod web;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

use debugger::{DebugInfoProvider, DebugScreen, DebuggerError, RendererInfo, RendererInfoSource};
use ui::{UiFrameLayer, UiSwitch};
use winit::dpi::PhysicalSize;
use winit::window::Window;

use self::overlay::{DebugOverlay, OverlayFrame};
use crate::assets::AssetIndex;
use crate::entity::components::{LookDirection, Position};
use crate::position::ChunkPos;
use crate::renderer::chunk::mesh_columns::MeshColumns;
use crate::renderer::chunk::mesh_source::MeshSource;
use crate::renderer::chunk::mesher::{BiomeClimate, ChunkMeshData, MeshDispatcher};
use crate::renderer::chunk::occlusion_graph::{self, VisibilitySet};
use crate::renderer::pipelines::entity_shadow::ShadowScene;
use crate::renderer::pipelines::held_item::HandRenderSelection;
use crate::renderer::pipelines::lightmap::LightmapInfo;
use crate::renderer::{CloudMode, Renderer, RendererError, SkyState};
use crate::resource_pack::ResourcePackManager;
use crate::ui::font::{FontOptions, FontSources};
use crate::world::block::block_registry::{BlockDefinitions, BlockRegistry};
use crate::world::block::model::CardinalLightType;
use crate::world::chunk::ChunkStore;
use crate::world::environment::{BiomeWeights, Environment, EnvironmentProbe};
use crate::world::generated;
use crate::world::light::{LevelLightEngine, LightDirty, LightTask, section_entries};

pub use crate::renderer::rhi::backend::Context as GpuContext;

/// One game tick, in seconds.
const TICK_SECONDS: f32 = 1.0 / 20.0;

/// The day time the sky shows, noon: a fixed view keeps its sky fixed.
const DAY_TIME: u64 = 6000;

/// The sections a column can have meshes for.
const MAX_SECTIONS: i32 = 32;

/// Vanilla's `VIS_MARGIN`-free default: the camera 8-block bucket the
/// occlusion walk reruns on.
const VISIBILITY_BUCKET: f64 = 8.0;

#[derive(Debug, thiserror::Error)]
pub enum ViewerError {
    #[error(transparent)]
    Renderer(#[from] RendererError),
    #[error("couldn't start a chunk generator: {0}")]
    Thread(#[from] thread::ThreadError),
}

/// The dimension a [`WorldSource`] draws.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Dimension {
    pub min_y: i32,
    pub height: u32,
    pub has_skylight: bool,
    /// The dimension's directional light, vanilla `Lighting.updateLevel`.
    pub cardinal_light: CardinalLightType,
    pub ambient_light: f32,
    /// A flat world's horizon is at its bottom.
    pub flat: bool,
}

/// Where the viewer's world comes from. Generating a column blocks, so
/// `chunk_packet` runs on a worker.
pub trait WorldSource: Send + Sync + 'static {
    fn dimension(&self) -> Dimension;

    /// How many biomes the biome registry has, which sections name them by.
    fn biome_count(&self) -> usize;

    /// The temperature and downfall behind each biome id.
    fn biome_climate(&self) -> HashMap<u32, BiomeClimate>;

    /// The dimension's attribute layers.
    fn environment(&self) -> Environment;

    /// The body of column `(x, z)`'s `level_chunk_with_light` packet, which
    /// `world::generated::decode` reads.
    ///
    /// # Errors
    /// Fails if the column can't be generated.
    fn chunk_packet(&self, x: i32, z: i32) -> Result<Vec<u8>, String>;
}

/// Where the viewer's assets and caches live. On the web these are paths in
/// the virtual file system.
#[derive(Clone, Debug)]
pub struct ViewerDirs {
    pub jar_assets_dir: PathBuf,
    pub game_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub panorama_dir: PathBuf,
    pub bedrock_ui_dir: PathBuf,
}

/// The camera's pose: feet-relative block position and the look angles in
/// degrees, as the settings crate holds them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ViewerCamera {
    pub position: [f64; 3],
    pub yaw_deg: f32,
    pub pitch_deg: f32,
}

#[derive(Clone)]
pub struct ViewerConfig {
    pub camera: ViewerCamera,
    pub render_distance: u32,
    pub dirs: ViewerDirs,
    pub asset_index: Option<AssetIndex>,
    pub vsync: bool,
    /// Whether the UI is drawn; the viewer starts with it off.
    pub ui_enabled: bool,
    /// The wire protocol whose block states the world is in.
    pub protocol: i32,
    /// Where the resource packs are kept, ready (see
    /// [`assets::prepare`]).
    pub resource_pack_location: resourcepack::PackLocation,
}

/// What the F3 screen lists beyond the renderer.
#[derive(Default)]
struct Stats {
    position: [f64; 3],
    yaw_deg: f32,
    pitch_deg: f32,
    loaded_chunks: usize,
    pending_chunks: usize,
    chunk_meshes: u32,
    sections_drawn: u32,
    screen: (u32, u32),
}

struct StatsProvider(Rc<std::cell::RefCell<Stats>>);

impl DebugInfoProvider for StatsProvider {
    fn lines(&self) -> Result<Vec<String>, DebuggerError> {
        let stats = self.0.borrow();
        Ok(vec![
            format!(
                "XYZ: {:.3} / {:.5} / {:.3}",
                stats.position[0], stats.position[1], stats.position[2]
            ),
            format!("Facing: {:.1} / {:.1}", stats.yaw_deg, stats.pitch_deg),
            format!(
                "Chunks: {} loaded, {} generating",
                stats.loaded_chunks, stats.pending_chunks
            ),
            format!(
                "Meshes: {} columns, {} sections drawn",
                stats.chunk_meshes, stats.sections_drawn
            ),
            format!("Display: {}x{}", stats.screen.0, stats.screen.1),
        ])
    }
}

struct RendererProvider(Rc<std::cell::RefCell<RendererInfo>>);

impl RendererInfoSource for RendererProvider {
    fn renderer_info(&self) -> RendererInfo {
        self.0.borrow().clone()
    }
}

/// A generated column on its way from a worker.
type Generated = (ChunkPos, Result<Vec<u8>, String>);

pub struct Viewer {
    renderer: Renderer,
    window: Arc<Window>,
    source: Arc<dyn WorldSource>,
    dimension: Dimension,
    biome_count: usize,
    render_distance: u32,
    camera_position: Position,
    camera_look: LookDirection,

    chunk_store: ChunkStore,
    light_engine: LevelLightEngine,
    block_registry: BlockRegistry,
    mesh_dispatcher: MeshDispatcher,
    mesh_columns: MeshColumns,
    /// The sections each column has a mesh for.
    compiled: HashMap<ChunkPos, u32>,
    section_vis: HashMap<(ChunkPos, i32), VisibilitySet>,
    section_vis_epoch: HashMap<(ChunkPos, i32), u64>,
    vis_task: Option<crossbeam_channel::Receiver<HashMap<ChunkPos, u32>>>,
    vis_valid: bool,
    last_vis_cam: (i32, i32, i32),
    pending_load_rescan: bool,

    requested: HashSet<ChunkPos>,
    generated_tx: Sender<Generated>,
    generated_rx: Receiver<Generated>,
    max_in_flight: usize,

    environment: Environment,
    environment_probe: EnvironmentProbe,
    biome_weights: BiomeWeights,
    block_light_flicker: f32,
    sky_state: SkyState,
    tick_accumulator: f32,

    overlay: UiFrameLayer<OverlayFrame>,
    debug: DebugScreen,
    stats: Rc<std::cell::RefCell<Stats>>,
    renderer_info: Rc<std::cell::RefCell<RendererInfo>>,
}

impl Viewer {
    /// A viewer drawing `source` into `window` through `ctx`.
    ///
    /// # Errors
    /// Fails if the renderer can't be brought up.
    pub fn new(
        ctx: GpuContext,
        window: Arc<Window>,
        config: ViewerConfig,
        source: Arc<dyn WorldSource>,
    ) -> Result<Self, ViewerError> {
        let ViewerConfig {
            camera,
            render_distance,
            dirs,
            asset_index,
            vsync,
            ui_enabled,
            protocol,
            resource_pack_location,
        } = config;
        let packs = ResourcePackManager::with_location(&dirs.game_dir, &resource_pack_location);
        let mut renderer = Renderer::with_context(
            ctx,
            Arc::clone(&window),
            FontSources {
                jar_assets_dir: &dirs.jar_assets_dir,
                asset_index: &asset_index,
                packs: &packs,
                options: FontOptions::default(),
            },
            &dirs.game_dir,
            &dirs.cache_dir,
            vsync,
            &dirs.panorama_dir,
            &dirs.bedrock_ui_dir,
        )?;

        let dimension = source.dimension();
        let block_registry = BlockRegistry::new(
            Rc::new(BlockDefinitions::new()),
            renderer.shared_render_assets(),
            protocol,
        );
        let climate = Arc::new(source.biome_climate());
        let mesh_dispatcher = renderer.create_mesh_dispatcher(
            block_registry.owned_render_view(),
            Arc::clone(&climate),
            Some(&packs),
            dimension.cardinal_light,
        );
        let chunk_store =
            ChunkStore::new_with_dimension(render_distance, dimension.height, dimension.min_y);
        let light_engine =
            LevelLightEngine::new(dimension.height, dimension.min_y, dimension.has_skylight);

        let camera_position = Position::new(camera.position[0], camera.position[1], camera.position[2]);
        let camera_look = LookDirection::new(camera.yaw_deg, camera.pitch_deg);
        renderer.reset_camera(camera_position, camera_look);

        let environment = source.environment();
        let mut sky_state = SkyState::default_day();
        sky_state.day_time = DAY_TIME;
        sky_state.game_time = DAY_TIME;
        sky_state.cardinal_light = dimension.cardinal_light;
        sky_state.skybox = environment.skybox;
        let environment_probe = EnvironmentProbe::new(environment.resolve(
            DAY_TIME as i64,
            &BiomeWeights::default(),
            0.0,
            0.0,
        ));

        let (generated_tx, generated_rx) = channel();
        let renderer_info = Rc::new(std::cell::RefCell::new(renderer.renderer_info()));
        let stats = Rc::new(std::cell::RefCell::new(Stats::default()));
        let mut debug = DebugScreen::new();
        debug.add_provider(Box::new(debugger::RendererInfoProvider(RendererProvider(
            Rc::clone(&renderer_info),
        ))));
        debug.add_provider(Box::new(StatsProvider(Rc::clone(&stats))));

        let mut viewer = Self {
            renderer,
            window,
            biome_count: source.biome_count(),
            source,
            dimension,
            render_distance,
            camera_position,
            camera_look,
            chunk_store,
            light_engine,
            block_registry,
            mesh_dispatcher,
            mesh_columns: MeshColumns::default(),
            compiled: HashMap::new(),
            section_vis: HashMap::new(),
            section_vis_epoch: HashMap::new(),
            vis_task: None,
            vis_valid: false,
            last_vis_cam: (i32::MIN, i32::MIN, i32::MIN),
            pending_load_rescan: false,
            requested: HashSet::new(),
            generated_tx,
            generated_rx,
            max_in_flight: (thread::available_parallelism() / 2).clamp(2, 8),
            environment,
            environment_probe,
            biome_weights: BiomeWeights::default(),
            block_light_flicker: 0.0,
            sky_state,
            tick_accumulator: 0.0,
            overlay: UiFrameLayer::new(UiSwitch::new(ui_enabled), Box::new(DebugOverlay)),
            debug,
            stats,
            renderer_info,
        };
        viewer
            .chunk_store
            .set_center(ChunkPos::new(
                (camera.position[0] as i32).div_euclid(16),
                (camera.position[2] as i32).div_euclid(16),
            ));
        Ok(viewer)
    }

    /// Whether the UI is drawn.
    pub fn ui_enabled(&self) -> bool {
        self.overlay.switch().enabled()
    }

    pub fn set_ui_enabled(&mut self, enabled: bool) {
        self.overlay.switch_mut().set(enabled);
    }

    /// F3: shows or hides the debug screen, which the UI switch gates.
    pub fn toggle_debug_screen(&mut self) -> bool {
        self.debug.switch_mut().toggle()
    }

    /// The lines the F3 screen lists, empty while it is off.
    pub fn debug_lines(&self) -> Vec<String> {
        self.debug.lines().unwrap_or_default()
    }

    /// The renderer's graphics API, GPU and driver, as F3 shows them.
    pub fn renderer_info(&self) -> RendererInfo {
        self.renderer_info.borrow().clone()
    }

    /// Resizes the drawing surface to the window's new size.
    pub fn resize(&mut self, width: u32, height: u32) {
        self.renderer.resize(PhysicalSize::new(width.max(1), height.max(1)));
    }

    /// The columns around the camera, nearest first.
    fn wanted_columns(&self) -> Vec<ChunkPos> {
        let centre = self.camera_chunk();
        let distance = self.render_distance as i32;
        let mut columns: Vec<ChunkPos> = (-distance..=distance)
            .flat_map(|dz| (-distance..=distance).map(move |dx| (dx, dz)))
            .map(|(dx, dz)| ChunkPos::new(centre.x + dx, centre.z + dz))
            .collect();
        columns.sort_by_key(|pos| {
            let dx = pos.x - centre.x;
            let dz = pos.z - centre.z;
            dx * dx + dz * dz
        });
        columns
    }

    fn camera_chunk(&self) -> ChunkPos {
        ChunkPos::new(
            (self.camera_position.x as i32).div_euclid(16),
            (self.camera_position.z as i32).div_euclid(16),
        )
    }

    /// Asks the workers for the nearest columns not generated yet.
    fn request_columns(&mut self) -> Result<(), ViewerError> {
        if self.requested.len() >= self.chunk_store.loaded_positions().count() + self.max_in_flight
        {
            return Ok(());
        }
        let mut in_flight = self
            .requested
            .len()
            .saturating_sub(self.chunk_store.loaded_positions().count());
        for pos in self.wanted_columns() {
            if in_flight >= self.max_in_flight {
                break;
            }
            if !self.requested.insert(pos) {
                continue;
            }
            in_flight += 1;
            let source = Arc::clone(&self.source);
            let tx = self.generated_tx.clone();
            thread::spawn("chunk-generator", move || {
                let packet = source.chunk_packet(pos.x, pos.z);
                let _ = tx.send((pos, packet));
            })?;
        }
        Ok(())
    }

    /// Loads the columns the workers finished, as `GameState::load_chunk`
    /// does for a chunk packet.
    fn load_generated(&mut self) {
        while let Ok((pos, packet)) = self.generated_rx.try_recv() {
            let chunk = packet.and_then(|payload| generated::decode(&payload));
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(error) => {
                    tracing::error!("Column [{}, {}] didn't generate: {error}", pos.x, pos.z);
                    continue;
                }
            };
            let table = self.block_registry.block_state_table();
            if let Err(error) = self.chunk_store.load_chunk(
                chunk.pos,
                &chunk.sections,
                &chunk.heightmaps,
                self.biome_count,
                table,
            ) {
                tracing::error!("Failed to load chunk [{}, {}]: {error}", pos.x, pos.z);
                continue;
            }
            self.light_engine
                .on_chunk_loaded(&mut self.chunk_store, table, (pos.x, pos.z));
            let count = self.light_engine.light_section_count();
            let light = &chunk.light;
            self.light_engine.queue_task(LightTask::ApplyLight {
                pos: (pos.x, pos.z),
                sky: section_entries(
                    count,
                    &light.sky_y_mask.clone().into(),
                    &light.empty_sky_y_mask.clone().into(),
                    &light.sky_updates[..],
                ),
                block: section_entries(
                    count,
                    &light.block_y_mask.clone().into(),
                    &light.empty_block_y_mask.clone().into(),
                    &light.block_updates[..],
                ),
                enable: true,
            });
        }
    }

    fn mark_column_dirty(&mut self, pos: ChunkPos) {
        let full = section_mask(self.chunk_store.section_count());
        self.mesh_columns.mark(pos, full, false);
    }

    /// Runs one light update and turns the resulting dirty scope into remesh
    /// work, as `GameState::update_light` does.
    fn update_light(&mut self) {
        let mut dirty = LightDirty::default();
        self.light_engine.poll_and_run(
            &mut self.chunk_store,
            self.block_registry.block_state_table(),
            &mut dirty,
        );
        if dirty.columns.is_empty() && dirty.sections.is_empty() {
            return;
        }
        let mut bumped: Vec<ChunkPos> = Vec::new();
        for &(x, z) in &dirty.columns {
            for p in crate::world::chunk::mesh_neighborhood(ChunkPos::new(x, z)) {
                if self.chunk_store.get_chunk(&p).is_some() && !bumped.contains(&p) {
                    bumped.push(p);
                }
            }
        }
        for &pos in &bumped {
            self.mark_column_dirty(pos);
        }
        if !bumped.is_empty() {
            self.pending_load_rescan = true;
        }
        let min_section_y = self.chunk_store.min_y() >> 4;
        let section_count = self.chunk_store.section_count();
        for key in &dirty.sections {
            let si = key.y - min_section_y;
            let col = ChunkPos::new(key.x, key.z);
            if si < 0 || si >= section_count || bumped.contains(&col) {
                continue;
            }
            if self.chunk_store.get_chunk(&col).is_none() {
                continue;
            }
            self.mesh_columns.mark(col, section_bit(si), true);
        }
    }

    /// Publishes every loaded column's dirty sections to the mesh workers.
    fn rescan_mesh_jobs(&mut self) {
        let loaded: Vec<ChunkPos> = self.chunk_store.loaded_positions().collect();
        for &pos in &loaded {
            if self.mesh_columns.incarnation(pos).is_none() {
                self.mark_column_dirty(pos);
            }
        }
        let camera = *self.camera_position;
        let dispatcher = &self.mesh_dispatcher;
        let requests = self.mesh_columns.requests(
            &loaded,
            camera,
            section_mask(self.chunk_store.section_count()),
            |_| 0,
            |pos| dispatcher.claimed_generation(pos),
        );
        if let Some(requests) = requests {
            self.mesh_dispatcher
                .publish(&self.chunk_store, camera, requests);
        }
    }

    /// The frame's ready meshes, uploaded in one transfer.
    fn upload_meshes(&mut self) {
        let results: Vec<ChunkMeshData> = self.mesh_dispatcher.drain_results().collect();
        let mut batch = Vec::with_capacity(results.len());
        for mut mesh in results {
            let stale = self.chunk_store.get_chunk(&mesh.pos).is_none()
                || self.mesh_columns.incarnation(mesh.pos) != Some(mesh.incarnation);
            if stale {
                continue;
            }
            let pos = mesh.pos;
            for (si, vis) in std::mem::take(&mut mesh.visibility) {
                let epoch = self.section_vis_epoch.entry((pos, si)).or_insert(0);
                if mesh.upload_epoch >= *epoch {
                    *epoch = mesh.upload_epoch;
                    self.section_vis.insert((pos, si), vis);
                }
            }
            let meshed = section_bits(mesh.replaced.clone());
            self.mesh_columns.meshed(pos, mesh.content_gen, meshed);
            *self.compiled.entry(pos).or_default() |= meshed;
            batch.push(mesh);
        }
        if batch.is_empty() {
            return;
        }
        self.pending_load_rescan = true;
        let dropped = self.renderer.upload_chunk_meshes(&batch);
        for (pos, sections) in dropped {
            let retired = section_bits(sections);
            if let Some(mask) = self.compiled.get_mut(&pos) {
                *mask &= !retired;
            }
            self.mesh_columns.remark(pos, retired);
        }
    }

    /// Drives the cave-cull occlusion walk, as `GameState::update_visibility`
    /// does: apply a finished walk, then schedule the next on 8-block
    /// movement or chunk loads.
    fn update_visibility(&mut self, loaded_meshes: bool) -> Result<(), ViewerError> {
        let player_chunk = self.camera_chunk();
        if let Some(bfs) = self.vis_task.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.vis_task = None;
            self.apply_visibility(&bfs);
        }
        let eye = self.renderer.camera_render_position();
        let cam_bucket = (
            (eye.x / VISIBILITY_BUCKET).floor() as i32,
            (eye.y / VISIBILITY_BUCKET).floor() as i32,
            (eye.z / VISIBILITY_BUCKET).floor() as i32,
        );
        let loads = loaded_meshes || std::mem::take(&mut self.pending_load_rescan);
        if self.vis_task.is_some() || (self.vis_valid && cam_bucket == self.last_vis_cam && !loads)
        {
            return Ok(());
        }
        self.last_vis_cam = cam_bucket;
        let section_vis = self.section_vis.clone();
        let min_y = self.chunk_store.min_y();
        let sections = self.chunk_store.section_count();
        let cam_si = ((eye.y - f64::from(min_y)) / 16.0).floor() as i32;
        let radius = self
            .chunk_store
            .loaded_positions()
            .map(|p| (p.x - player_chunk.x).abs().max((p.z - player_chunk.z).abs()))
            .max()
            .unwrap_or(0);
        let (tx, rx) = crossbeam_channel::bounded(1);
        thread::spawn("occlusion-walk", move || {
            let walk = occlusion_graph::compute_visible_mask(
                &section_vis,
                player_chunk,
                cam_si,
                eye,
                min_y,
                sections,
                radius,
            );
            let _ = tx.send(walk);
        })?;
        self.vis_task = Some(rx);
        Ok(())
    }

    /// Combines a finished walk with the near columns into per-column draw
    /// masks and pushes them to the GPU cull.
    fn apply_visibility(&mut self, walk: &HashMap<ChunkPos, u32>) {
        let eye = self.renderer.camera_render_position();
        let full = section_mask(self.chunk_store.section_count());
        let masks: HashMap<ChunkPos, u32> = self
            .chunk_store
            .loaded_positions()
            .map(|pos| {
                let mask = if crate::renderer::chunk::buffer::column_is_near(pos, eye) {
                    full
                } else {
                    walk.get(&pos).copied().unwrap_or(0)
                };
                (pos, mask)
            })
            .collect();
        self.vis_valid = true;
        self.renderer.set_chunk_visibility(masks);
    }

    /// Runs the ticks `dt` seconds make due: texture animations, the block
    /// light's flicker, the sky's attributes and the lightmap.
    fn tick(&mut self, dt: f32) {
        self.tick_accumulator = (self.tick_accumulator + dt).min(1.0);
        let ticked = self.tick_accumulator >= TICK_SECONDS;
        if ticked {
            self.renderer.tick_texture_animations();
        }
        while self.tick_accumulator >= TICK_SECONDS {
            self.block_light_flicker = tick_block_light_flicker(self.block_light_flicker);
            let plains = self.environment.plains;
            let chunks = &self.chunk_store;
            self.biome_weights = BiomeWeights::sample(
                self.renderer.camera_render_position() * 0.25,
                |x, y, z| chunks.noise_biome(x, y, z).unwrap_or(plains),
            );
            self.environment_probe.tick(self.environment.resolve(
                self.sky_state.day_time as i64,
                &self.biome_weights,
                self.sky_state.rain(),
                self.sky_state.thunder(),
            ));
            self.tick_accumulator -= TICK_SECONDS;
        }
        if ticked {
            let attributes = self.environment_probe.at(self.partial_tick());
            let with = |rgb: glam::Vec3, w: f32| rgb.extend(w).to_array();
            self.renderer.tick_lightmap(LightmapInfo {
                block_light_tint: with(attributes.block_light_tint, self.block_light_flicker + 1.4),
                sky_light_color: with(attributes.sky_light_color, attributes.sky_light_factor),
                ambient_color: with(attributes.ambient_light_color, 0.5),
                night_vision_color: with(attributes.night_vision_color, 0.0),
                ..bytemuck::Zeroable::zeroed()
            });
        }
    }

    fn partial_tick(&self) -> f32 {
        (self.tick_accumulator / TICK_SECONDS).clamp(0.0, 1.0)
    }

    /// Advances the viewer by `dt` seconds and draws a frame.
    ///
    /// # Errors
    /// Fails if a worker can't start. A frame that fails to draw is logged
    /// and the next one tries again.
    pub fn frame(&mut self, dt: f32) -> Result<(), ViewerError> {
        self.request_columns()?;
        self.load_generated();
        self.update_light();
        self.rescan_mesh_jobs();
        let meshed_before = self.compiled.len();
        self.upload_meshes();
        self.update_visibility(self.compiled.len() != meshed_before)?;
        self.tick(dt);
        self.draw();
        Ok(())
    }

    fn draw(&mut self) {
        let partial_tick = self.partial_tick();
        let min_y = self.chunk_store.min_y();
        let attributes = self.environment_probe.at(partial_tick);
        let horizon = if self.dimension.flat {
            f64::from(min_y)
        } else {
            63.0
        };
        let eye = self.renderer.camera_render_position();
        let sky = SkyState {
            partial_tick,
            attributes,
            skybox: self.environment.skybox,
            void_darkness: crate::renderer::fog::void_darkness(eye.y, min_y, self.dimension.flat),
            dark_disc: eye.y < horizon,
            cardinal_light: self.dimension.cardinal_light,
            ..self.sky_state.clone()
        };

        let table = self.block_registry.block_state_table();
        let hand_light = {
            let at = [eye.x.floor() as i32, eye.y.floor() as i32, eye.z.floor() as i32];
            crate::renderer::chunk::block_lighter::LightLevels::new(
                self.chunk_store.get_sky_light(at[0], at[1], at[2]),
                self.chunk_store.get_block_light(at[0], at[1], at[2]),
            )
        };
        let overlay = self.overlay_elements();
        self.refresh_stats();
        let result = self.renderer.render_world(
            self.block_registry.render_view(),
            &self.window,
            false,
            overlay,
            [0.0, 0.0],
            [None, None],
            [None, None],
            true,
            hand_light,
            HandRenderSelection::NONE,
            None,
            false,
            None,
            None,
            false,
            sky,
            ShadowScene {
                casters: &[],
                chunks: &self.chunk_store,
                block_state_table: table,
                sky_darken: attributes.sky_darken(),
                ambient_light: self.dimension.ambient_light,
            },
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
            CloudMode::default(),
            self.render_distance,
            None,
            None,
            false,
        );
        if let Err(error) = result {
            tracing::error!("Render error: {error}");
        }
    }

    fn refresh_stats(&mut self) {
        let mut stats = self.stats.borrow_mut();
        stats.position = [
            self.camera_position.x,
            self.camera_position.y,
            self.camera_position.z,
        ];
        stats.yaw_deg = self.camera_look.y_rot_deg();
        stats.pitch_deg = self.camera_look.x_rot_deg();
        stats.loaded_chunks = self.chunk_store.loaded_positions().count();
        stats.pending_chunks = self.requested.len().saturating_sub(stats.loaded_chunks);
        stats.chunk_meshes = self.renderer.loaded_chunk_count();
        stats.sections_drawn = self.renderer.sections_drawn();
        stats.screen = (self.renderer.screen_width(), self.renderer.screen_height());
    }

    /// The UI elements of this frame, none while the UI is off.
    fn overlay_elements(&mut self) -> Vec<crate::renderer::pipelines::menu_overlay::MenuElement> {
        let (width, height) = (
            self.renderer.screen_width() as f32,
            self.renderer.screen_height() as f32,
        );
        let scale = crate::ui::scale::gui_scale(width, height, 0, false);
        let font_size = crate::ui::common::FONT_SIZE * scale;
        let left = self.debug_lines();
        let widths: HashMap<String, f32> = left
            .iter()
            .map(|line| (line.clone(), self.renderer.menu_text_width(line, font_size)))
            .collect();
        let mut frame = OverlayFrame {
            scale,
            font_size,
            lines: left,
            widths,
            elements: Vec::new(),
        };
        if let Err(error) = self.overlay.render(&mut frame) {
            tracing::error!("UI error: {error}");
        }
        frame.elements
    }
}

/// Vanilla `LightmapRenderStateExtractor.tick`: the block light's random walk.
fn tick_block_light_flicker(flicker: f32) -> f32 {
    let r = fastrand::f32;
    (flicker + (r() - r()) * r() * r() * 0.1) * 0.9
}

/// Bit for one section index, 0 outside a column's addressable sections.
fn section_bit(si: i32) -> u32 {
    if (0..MAX_SECTIONS).contains(&si) {
        1u32 << si
    } else {
        0
    }
}

fn section_bits(indices: impl IntoIterator<Item = i32>) -> u32 {
    indices
        .into_iter()
        .fold(0u32, |mask, si| mask | section_bit(si))
}

/// Full mask for an `n`-section column (bits `0..n` set).
fn section_mask(n: i32) -> u32 {
    if n >= MAX_SECTIONS {
        u32::MAX
    } else {
        (1u32 << n) - 1
    }
}
