use std::collections::{HashMap, VecDeque};
use std::mem::offset_of;

use glam::DVec3;

use super::mesher::{
    ChunkAABB, ChunkMeshData, PackVertex, PackedVertex, SectionMesh, TerrainFormat,
};
use crate::position::ChunkPos;
use crate::renderer::rhi::IndirectDrawFeatures;
use crate::renderer::{MAX_FRAMES_IN_FLIGHT, rhi, shader, util};

const BUCKET_VERTICES: u32 = 32768;
const BUCKET_INDICES: u32 = 49152;
const INDEX_SIZE: u64 = size_of::<u32>() as u64;
const BYTES_PER_BUCKET: u64 =
    BUCKET_VERTICES as u64 * size_of::<PackedVertex>() as u64 + BUCKET_INDICES as u64 * INDEX_SIZE;
const MIN_BUCKETS: u32 = 128;
const MAX_BUCKETS: u32 = 2048;
const VRAM_BUDGET_FRACTION: f64 = 0.25;

const CULL_BINDINGS: &[rhi::BindGroupLayoutEntry] = &[
    rhi::BindGroupLayoutEntry::storage(rhi::ShaderStages::COMPUTE),
    rhi::BindGroupLayoutEntry::uniform(rhi::ShaderStages::COMPUTE),
    rhi::BindGroupLayoutEntry::storage(rhi::ShaderStages::COMPUTE),
    rhi::BindGroupLayoutEntry::storage(rhi::ShaderStages::COMPUTE),
    rhi::BindGroupLayoutEntry::storage(rhi::ShaderStages::COMPUTE),
    rhi::BindGroupLayoutEntry::storage(rhi::ShaderStages::COMPUTE),
];
const COUNT_SIZE: u64 = 4;
const FRUSTUM_SIZE: u64 = size_of::<FrustumData>() as u64;
const FADE_DURATION_MS: f32 = 1000.0;
const NEARBY_DIST_SQ: f32 = 768.0;

pub fn column_is_near(pos: ChunkPos, eye: DVec3) -> bool {
    let dx = pos.x as f64 * 16.0 + 8.0 - eye.x;
    let dz = pos.z as f64 * 16.0 + 8.0 - eye.z;
    dx * dx + dz * dz < NEARBY_DIST_SQ as f64
}

struct FreeList {
    capacity: u32,
    free: Vec<(u32, u32)>,
}

impl FreeList {
    fn new(capacity: u32) -> Self {
        Self {
            capacity,
            free: vec![(0, capacity)],
        }
    }

    fn reset(&mut self) {
        self.free.clear();
        self.free.push((0, self.capacity));
    }

    fn alloc(&mut self, n: u32) -> Option<u32> {
        for i in 0..self.free.len() {
            let (off, len) = self.free[i];
            if len >= n {
                if len == n {
                    self.free.remove(i);
                } else {
                    self.free[i] = (off + n, len - n);
                }
                return Some(off);
            }
        }
        None
    }

    fn free_region(&mut self, off: u32, n: u32) {
        let pos = self.free.partition_point(|&(o, _)| o < off);
        self.free.insert(pos, (off, n));
        if pos + 1 < self.free.len() {
            let (o, l) = self.free[pos];
            let (no, nl) = self.free[pos + 1];
            if o + l == no {
                self.free[pos] = (o, l + nl);
                self.free.remove(pos + 1);
            }
        }
        if pos > 0 {
            let (po, pl) = self.free[pos - 1];
            let (o, l) = self.free[pos];
            if po + pl == o {
                self.free[pos - 1] = (po, pl + l);
                self.free.remove(pos);
            }
        }
    }
}

fn compute_bucket_count(device_local_bytes: u64) -> u32 {
    let budget = (device_local_bytes as f64 * VRAM_BUDGET_FRACTION) as u64;
    let buckets = (budget / BYTES_PER_BUCKET) as u32;
    let count = buckets.clamp(MIN_BUCKETS, MAX_BUCKETS);
    tracing::info!(
        "GPU VRAM: {} MB, chunk budget: {} MB, buckets: {}",
        device_local_bytes / (1024 * 1024),
        (count as u64 * BYTES_PER_BUCKET) / (1024 * 1024),
        count
    );
    count
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct ChunkMeta {
    aabb_min: [f32; 4],
    aabb_max: [f32; 4],
    index_count: u32,
    first_index: u32,
    vertex_offset: i32,
    visibility: u32,
    origin: [i32; 3],
    solid_index_count: u32,
}

impl ChunkMeta {
    pub(crate) fn instance(origin: [i32; 3]) -> Self {
        Self {
            origin,
            visibility: 1.0f32.to_bits(),
            ..bytemuck::Zeroable::zeroed()
        }
    }
}

const VERTEX_ATTRIBUTES: [rhi::VertexAttribute; 5] = {
    const POS: u32 = offset_of!(PackedVertex, pos) as u32;
    [
        rhi::VertexAttribute {
            location: 0,
            format: rhi::VertexFormat::Unorm16x2,
            offset: POS,
        },
        rhi::VertexAttribute {
            location: 1,
            format: rhi::VertexFormat::Unorm16,
            offset: POS + 4,
        },
        rhi::VertexAttribute {
            location: 2,
            format: rhi::VertexFormat::Uint32,
            offset: offset_of!(PackedVertex, uv) as u32,
        },
        rhi::VertexAttribute {
            location: 3,
            format: rhi::VertexFormat::Uint16,
            offset: offset_of!(PackedVertex, sprite) as u32,
        },
        rhi::VertexAttribute {
            location: 4,
            format: rhi::VertexFormat::Unorm8x4,
            offset: offset_of!(PackedVertex, light_tint) as u32,
        },
    ]
};

const PACK_VERTEX_ATTRIBUTES: [rhi::VertexAttribute; 7] = {
    let [pos_xy, pos_z, uv, sprite, light_tint] = VERTEX_ATTRIBUTES;
    [
        pos_xy,
        pos_z,
        uv,
        sprite,
        light_tint,
        rhi::VertexAttribute {
            location: 7,
            format: rhi::VertexFormat::Unorm8x4,
            offset: offset_of!(PackVertex, shade) as u32,
        },
        rhi::VertexAttribute {
            location: 8,
            format: rhi::VertexFormat::Uint32,
            offset: offset_of!(PackVertex, entity) as u32,
        },
    ]
};

pub const CHUNK_VERTEX_BUFFERS: [rhi::VertexBufferLayout<'static>; 2] =
    chunk_vertex_buffers(size_of::<PackedVertex>(), &VERTEX_ATTRIBUTES);

pub const PACK_CHUNK_VERTEX_BUFFERS: [rhi::VertexBufferLayout<'static>; 2] =
    chunk_vertex_buffers(size_of::<PackVertex>(), &PACK_VERTEX_ATTRIBUTES);

const fn chunk_vertex_buffers(
    stride: usize,
    attributes: &'static [rhi::VertexAttribute],
) -> [rhi::VertexBufferLayout<'static>; 2] {
    [
        rhi::VertexBufferLayout {
            stride: stride as u32,
            step_mode: rhi::VertexStepMode::Vertex,
            attributes,
        },
        rhi::VertexBufferLayout {
            stride: size_of::<ChunkMeta>() as u32,
            step_mode: rhi::VertexStepMode::Instance,
            attributes: &[
                rhi::VertexAttribute {
                    location: 5,
                    format: rhi::VertexFormat::Sint32x3,
                    offset: offset_of!(ChunkMeta, origin) as u32,
                },
                rhi::VertexAttribute {
                    location: 6,
                    format: rhi::VertexFormat::Float32,
                    offset: offset_of!(ChunkMeta, visibility) as u32,
                },
            ],
        },
    ]
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct DrawCommand {
    index_count: u32,
    instance_count: u32,
    first_index: u32,
    vertex_offset: i32,
    first_instance: u32,
}

fn aabb_in_frustum(aabb: &ChunkAABB, origin: [i32; 3], planes: &[[f32; 4]; 6], eye: DVec3) -> bool {
    let base = (origin_dvec(origin) - eye).as_vec3();
    let mn = [
        base.x + aabb.min[0],
        base.y + aabb.min[1],
        base.z + aabb.min[2],
    ];
    let mx = [
        base.x + aabb.max[0],
        base.y + aabb.max[1],
        base.z + aabb.max[2],
    ];
    for p in planes {
        let d = p[0] * if p[0] >= 0.0 { mx[0] } else { mn[0] }
            + p[1] * if p[1] >= 0.0 { mx[1] } else { mn[1] }
            + p[2] * if p[2] >= 0.0 { mx[2] } else { mn[2] }
            + p[3];
        if d < 0.0 {
            return false;
        }
    }
    true
}

fn origin_dvec(origin: [i32; 3]) -> DVec3 {
    DVec3::new(origin[0] as f64, origin[1] as f64, origin[2] as f64)
}

fn column_nearby(fade_enabled: bool, pos: ChunkPos, eye: DVec3) -> bool {
    !fade_enabled || column_is_near(pos, eye)
}

pub(crate) fn build_meta(
    chunks: &HashMap<ChunkPos, ChunkAlloc>,
    chunk_visibility: Option<&HashMap<ChunkPos, u32>>,
    fade_enabled: bool,
    eye: DVec3,
    now: crate::clock::Instant,
    out: &mut Vec<ChunkMeta>,
) {
    out.clear();
    for (pos, alloc) in chunks.iter() {
        let nearby = column_nearby(fade_enabled, *pos, eye);

        let col_vis = chunk_visibility
            .and_then(|v| v.get(pos).copied())
            .unwrap_or(u32::MAX);

        for sec in &alloc.sections {
            if col_vis & (1u32 << sec.section_index) == 0 {
                continue;
            }
            let vis = ChunkBufferStore::section_visibility(nearby, sec, now);
            out.push(ChunkMeta {
                aabb_min: sec.aabb.min,
                aabb_max: sec.aabb.max,
                index_count: sec.index_count,
                first_index: sec.first_index,
                vertex_offset: sec.vertex_offset,
                visibility: vis.to_bits(),
                origin: sec.origin,
                solid_index_count: sec.solid_index_count,
            });
        }
    }
}

pub(crate) fn sort_meta(meta: &mut [ChunkMeta], eye: DVec3) {
    let center_dist_sq = |m: &ChunkMeta| {
        let center = DVec3::new(
            ((m.aabb_min[0] + m.aabb_max[0]) * 0.5) as f64,
            ((m.aabb_min[1] + m.aabb_max[1]) * 0.5) as f64,
            ((m.aabb_min[2] + m.aabb_max[2]) * 0.5) as f64,
        );
        (origin_dvec(m.origin) + center - eye).length_squared()
    };
    meta.sort_unstable_by(|a, b| {
        center_dist_sq(a)
            .partial_cmp(&center_dist_sq(b))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct FrustumData {
    planes: [[f32; 4]; 6],
    chunk_count: u32,
    cam_block: [i32; 3],
    frac: [f32; 3],
    _pad: f32,
}

pub(crate) struct SectionAlloc {
    section_index: i32,
    aabb: ChunkAABB,
    origin: [i32; 3],
    first_index: u32,
    index_count: u32,
    solid_index_count: u32,
    water_first_index: u32,
    water_index_count: u32,
    idx_len: u32,
    vertex_offset: i32,
    vtx_len: u32,
    uploaded_at: crate::clock::Instant,
    epoch: u64,
}

pub(crate) struct ChunkAlloc {
    sections: Vec<SectionAlloc>,
}

#[cfg(any(test, feature = "cpu-bench"))]
impl ChunkAlloc {
    pub(crate) fn synthetic(
        pos: ChunkPos,
        min_y: i32,
        section_indices: impl IntoIterator<Item = i32>,
        uploaded_at: crate::clock::Instant,
    ) -> Self {
        let sections = section_indices
            .into_iter()
            .map(|si| SectionAlloc {
                section_index: si,
                aabb: ChunkAABB {
                    min: [0.0, 0.0, 0.0, 0.0],
                    max: [16.0, 16.0, 16.0, 0.0],
                },
                origin: [pos.x * 16, min_y + si * 16, pos.z * 16],
                first_index: 0,
                index_count: 6,
                solid_index_count: 6,
                water_first_index: 0,
                water_index_count: 0,
                idx_len: 6,
                vertex_offset: 0,
                vtx_len: 4,
                uploaded_at,
                epoch: 0,
            })
            .collect();
        Self { sections }
    }
}

pub struct ChunkBufferStore {
    indirect_draw: IndirectDrawFeatures,
    max_meta: usize,
    vertex_buffer: rhi::backend::Buffer,
    index_buffer: rhi::backend::Buffer,
    staging_buffer: rhi::backend::Buffer,
    staging_size: u64,
    use_staging: bool,
    format: TerrainFormat,
    pool_vertices: u32,

    vtx_free: FreeList,
    idx_free: FreeList,
    chunks: HashMap<ChunkPos, ChunkAlloc>,
    chunk_visibility: HashMap<ChunkPos, u32>,
    meta_dirty: bool,
    fade_until: crate::clock::Instant,
    last_sort_cam: DVec3,

    compute_pipeline: Option<rhi::backend::ComputePipeline>,
    compute_layout: Option<rhi::backend::PipelineLayout>,
    cull_layout: Option<rhi::backend::BindGroupLayout>,

    camera: DrawList,
    shadow: Option<DrawList>,
    shadow_dirty: bool,
    count_readbacks: Vec<rhi::backend::Buffer>,
    fade_enabled: bool,
    last_draw_count: u32,

    frame_seq: u64,
    pending_free: VecDeque<(u64, (u32, u32, u32, u32))>,
}

impl ChunkBufferStore {
    pub fn new(device: &rhi::backend::Device, indirect_draw: IndirectDrawFeatures) -> Self {
        let total_buckets = compute_bucket_count(device.device_local_bytes);
        let pool_vertices = total_buckets * BUCKET_VERTICES;
        let format = TerrainFormat::Vanilla;
        let vertex_size = u64::from(pool_vertices) * format.vertex_size();
        let index_size = total_buckets as u64 * BUCKET_INDICES as u64 * INDEX_SIZE;

        let use_staging = device.discrete;

        let vertex_buffer = create_pool(
            device,
            use_staging,
            "vertex_pool",
            vertex_size,
            rhi::BufferUsages::VERTEX,
        );
        let index_buffer = create_pool(
            device,
            use_staging,
            "index_pool",
            index_size,
            rhi::BufferUsages::INDEX,
        );

        let staging_size = if use_staging {
            BYTES_PER_BUCKET * 16
        } else {
            BYTES_PER_BUCKET * 4
        };
        let staging_buffer = rhi::backend::Buffer::new(
            device,
            "staging",
            staging_size,
            rhi::BufferUsages::COPY_SRC,
            rhi::MemoryLocation::Host,
        );

        tracing::info!(
            "Chunk buffers: {} (vertex={} MB, index={} MB, staging={} KB)",
            if use_staging {
                "DEVICE_LOCAL + staging"
            } else {
                "HOST_VISIBLE"
            },
            vertex_size / (1024 * 1024),
            index_size / (1024 * 1024),
            staging_size / 1024,
        );

        let vtx_free = FreeList::new(pool_vertices);
        let idx_free = FreeList::new(total_buckets * BUCKET_INDICES);

        let max_meta = (total_buckets * 16).max(8192) as usize;
        let meta_size = (max_meta * size_of::<ChunkMeta>()) as u64;
        let indirect_size = (max_meta * size_of::<DrawCommand>()) as u64;

        let count_readbacks = (0..MAX_FRAMES_IN_FLIGHT)
            .filter(|_| indirect_draw.gpu_cull)
            .map(|_| {
                rhi::backend::Buffer::new(
                    device,
                    "draw_count_readback",
                    2 * COUNT_SIZE,
                    rhi::BufferUsages::COPY_DST,
                    rhi::MemoryLocation::Readback,
                )
            })
            .collect();

        let cull_layout = indirect_draw
            .gpu_cull
            .then(|| rhi::backend::create_bind_group_layout(device, CULL_BINDINGS));
        let camera = DrawList::new(device, cull_layout, meta_size, indirect_size);
        let compute_layout =
            cull_layout.map(|layout| rhi::backend::create_pipeline_layout(device, &[layout], None));

        let compute_pipeline = compute_layout.map(|layout| {
            rhi::backend::create_compute_pipeline(
                device,
                layout,
                "cull",
                shader::include_shader!("cull.comp"),
            )
        });

        Self {
            indirect_draw,
            max_meta,
            vertex_buffer,
            index_buffer,
            staging_buffer,
            staging_size,
            use_staging,
            format,
            pool_vertices,
            vtx_free,
            idx_free,
            chunks: HashMap::new(),
            chunk_visibility: HashMap::new(),
            meta_dirty: true,
            fade_until: crate::clock::Instant::now(),
            last_sort_cam: DVec3::MAX,
            compute_pipeline,
            compute_layout,
            cull_layout,
            camera,
            shadow: None,
            shadow_dirty: true,
            count_readbacks,
            fade_enabled: false,
            last_draw_count: 0,
            frame_seq: 0,
            pending_free: VecDeque::new(),
        }
    }

    pub fn sections_drawn(&self) -> u32 {
        self.last_draw_count
    }

    fn flush_transfer(
        &mut self,
        device: &rhi::backend::Device,
        copy_v: &[rhi::BufferCopy],
        copy_i: &[rhi::BufferCopy],
    ) {
        if copy_v.is_empty() && copy_i.is_empty() {
            return;
        }
        device.submit(|encoder| {
            for (dst, copies) in [(&self.vertex_buffer, copy_v), (&self.index_buffer, copy_i)] {
                if !copies.is_empty() {
                    encoder.copy_buffer(self.staging_buffer.handle(), dst.handle(), copies);
                }
            }
        });
    }

    pub fn upload_batch(
        &mut self,
        device: &rhi::backend::Device,
        meshes: &[ChunkMeshData],
    ) -> Vec<(ChunkPos, Vec<i32>)> {
        let mut needs_remesh: Vec<(ChunkPos, Vec<i32>)> = Vec::new();
        if meshes.is_empty() {
            return needs_remesh;
        }

        struct Plan<'a> {
            section_index: i32,
            verts: &'a [u8],
            vertex_count: u32,
            indices: &'a [u32],
            water_indices: &'a [u32],
            vtx_off: u32,
            idx_off: u32,
            solid_index_count: u32,
            aabb: ChunkAABB,
            origin: [i32; 3],
        }

        impl Plan<'_> {
            fn write_indices(&self, buffer: &mut rhi::backend::Buffer, off: usize) -> usize {
                let opaque: &[u8] = bytemuck::cast_slice(self.indices);
                buffer.write_at(off, opaque);
                buffer.write_at(off + opaque.len(), bytemuck::cast_slice(self.water_indices));
                opaque.len() + size_of_val(self.water_indices)
            }
        }

        const PENDING_FREE_DRAIN_THRESHOLD: usize = 8192;
        if self.pending_free.len() > PENDING_FREE_DRAIN_THRESHOLD {
            device.wait_idle();
            while let Some((_, slice)) = self.pending_free.pop_front() {
                self.free_slice(slice);
            }
        }

        let staging_half = self.staging_size as usize / 2;
        let mut copy_v: Vec<rhi::BufferCopy> = Vec::new();
        let mut copy_i: Vec<rhi::BufferCopy> = Vec::new();
        let mut stg_v = 0usize;
        let mut stg_i = 0usize;

        for mesh in meshes {
            let accepted: std::collections::HashSet<i32> = mesh
                .replaced
                .clone()
                .filter(|si| {
                    let stored = self
                        .chunks
                        .get(&mesh.pos)
                        .and_then(|c| c.sections.iter().find(|s| s.section_index == *si))
                        .map(|s| s.epoch)
                        .unwrap_or(0);
                    mesh.upload_epoch >= stored
                })
                .collect();

            let mut freed: Vec<(u32, u32, u32, u32)> = Vec::new();
            let mut was_present: std::collections::HashSet<i32> = std::collections::HashSet::new();
            if let Some(entry) = self.chunks.get_mut(&mesh.pos) {
                entry.sections.retain(|s| {
                    if accepted.contains(&s.section_index) {
                        was_present.insert(s.section_index);
                        freed.push((s.vertex_offset as u32, s.vtx_len, s.first_index, s.idx_len));
                        false
                    } else {
                        true
                    }
                });
            }
            self.retire_slices(freed.iter().copied());
            self.meta_dirty = true;

            let upload_secs: Vec<&SectionMesh> = mesh
                .sections
                .iter()
                .filter(|s| accepted.contains(&s.section_index))
                .filter(|s| s.vertices.format() == self.format)
                .collect();

            if upload_secs.is_empty() {
                if self
                    .chunks
                    .get(&mesh.pos)
                    .is_some_and(|c| c.sections.is_empty())
                {
                    self.chunks.remove(&mesh.pos);
                }
                continue;
            }

            if self.use_staging {
                let v_bytes: usize = upload_secs.iter().map(|s| s.vertices.bytes().len()).sum();
                let i_bytes: usize = upload_secs
                    .iter()
                    .map(|s| (s.indices.len() + s.water_indices.len()) * INDEX_SIZE as usize)
                    .sum();
                if v_bytes > staging_half || i_bytes > staging_half {
                    tracing::warn!(
                        "Chunk {:?} too large for staging ({} v / {} i bytes), skipping",
                        mesh.pos,
                        v_bytes,
                        i_bytes,
                    );
                    continue;
                }
            }

            let mut plans: Vec<Plan> = Vec::with_capacity(upload_secs.len());
            let mut taken: Vec<(u32, u32, u32, u32)> = Vec::new();
            let mut pool_full = false;
            for sec in &upload_secs {
                let vcount = sec.vertices.len() as u32;
                let icount = (sec.indices.len() + sec.water_indices.len()) as u32;
                if vcount == 0 || icount == 0 {
                    continue;
                }
                let Some(vtx_off) = self.vtx_free.alloc(vcount) else {
                    self.free_slices(&taken);
                    tracing::debug!("Vertex pool full, skipping {:?}", mesh.pos);
                    pool_full = true;
                    break;
                };
                let Some(idx_off) = self.idx_free.alloc(icount) else {
                    self.vtx_free.free_region(vtx_off, vcount);
                    self.free_slices(&taken);
                    tracing::debug!("Index pool full, skipping {:?}", mesh.pos);
                    pool_full = true;
                    break;
                };
                debug_assert_eq!(
                    sec.vertices.bytes().len() as u64,
                    u64::from(vcount) * self.format.vertex_size()
                );
                taken.push((vtx_off, vcount, idx_off, icount));
                plans.push(Plan {
                    section_index: sec.section_index,
                    verts: sec.vertices.bytes(),
                    vertex_count: vcount,
                    indices: &sec.indices,
                    water_indices: &sec.water_indices,
                    vtx_off,
                    idx_off,
                    solid_index_count: sec.solid_index_count,
                    aabb: sec.aabb,
                    origin: [
                        mesh.pos.x * 16,
                        mesh.min_y + sec.section_index * 16,
                        mesh.pos.z * 16,
                    ],
                });
            }
            if pool_full {
                needs_remesh.push((mesh.pos, accepted.iter().copied().collect()));
                continue;
            }
            if plans.is_empty() {
                continue;
            }

            if self.use_staging {
                let mv: usize = plans.iter().map(|p| p.verts.len()).sum();
                let mi: usize = plans
                    .iter()
                    .map(|p| (p.indices.len() + p.water_indices.len()) * INDEX_SIZE as usize)
                    .sum();
                if stg_v + mv > staging_half || stg_i + mi > staging_half {
                    self.flush_transfer(device, &copy_v, &copy_i);
                    copy_v.clear();
                    copy_i.clear();
                    stg_v = 0;
                    stg_i = 0;
                }
                for p in &plans {
                    self.staging_buffer.write_at(stg_v, p.verts);
                    let vbytes = p.verts.len();
                    copy_v.push(rhi::BufferCopy {
                        src_offset: stg_v as u64,
                        dst_offset: u64::from(p.vtx_off) * self.format.vertex_size(),
                        size: vbytes as u64,
                    });
                    stg_v += vbytes;

                    let off = staging_half + stg_i;
                    let ibytes = p.write_indices(&mut self.staging_buffer, off);
                    copy_i.push(rhi::BufferCopy {
                        src_offset: off as u64,
                        dst_offset: p.idx_off as u64 * INDEX_SIZE,
                        size: ibytes as u64,
                    });
                    stg_i += ibytes;
                }
            } else {
                for p in &plans {
                    let base = u64::from(p.vtx_off) * self.format.vertex_size();
                    self.vertex_buffer.write_at(base as usize, p.verts);
                    p.write_indices(
                        &mut self.index_buffer,
                        p.idx_off as usize * INDEX_SIZE as usize,
                    );
                }
            }

            let now = crate::clock::Instant::now();
            let new_sections = plans.iter().map(|p| SectionAlloc {
                section_index: p.section_index,
                aabb: p.aabb,
                origin: p.origin,
                first_index: p.idx_off,
                index_count: p.indices.len() as u32,
                solid_index_count: p.solid_index_count,
                water_first_index: p.idx_off + p.indices.len() as u32,
                water_index_count: p.water_indices.len() as u32,
                idx_len: (p.indices.len() + p.water_indices.len()) as u32,
                vertex_offset: p.vtx_off as i32,
                vtx_len: p.vertex_count,
                uploaded_at: if was_present.contains(&p.section_index) {
                    now.checked_sub(std::time::Duration::from_secs(2))
                        .unwrap_or(now)
                } else {
                    now
                },
                epoch: mesh.upload_epoch,
            });

            let revealed = plans
                .iter()
                .any(|p| !was_present.contains(&p.section_index));
            if revealed && !column_nearby(self.fade_enabled, mesh.pos, self.last_sort_cam) {
                let dur = std::time::Duration::from_secs_f32(FADE_DURATION_MS / 1000.0);
                self.fade_until = self.fade_until.max(now + dur);
            }

            self.chunks
                .entry(mesh.pos)
                .or_insert_with(|| ChunkAlloc {
                    sections: Vec::new(),
                })
                .sections
                .extend(new_sections);
        }

        if self.use_staging {
            self.flush_transfer(device, &copy_v, &copy_i);
        }

        let total_sections: usize = self.chunks.values().map(|c| c.sections.len()).sum();
        self.ensure_meta_capacity(device, total_sections);

        needs_remesh
    }

    fn ensure_meta_capacity(&mut self, device: &rhi::backend::Device, needed: usize) {
        if needed <= self.max_meta {
            return;
        }
        let new_max = (needed.saturating_mul(3) / 2)
            .next_power_of_two()
            .max(self.max_meta * 2);

        device.wait_idle();

        let meta_size = (new_max * size_of::<ChunkMeta>()) as u64;
        let indirect_size = (new_max * size_of::<DrawCommand>()) as u64;
        for list in std::iter::once(&mut self.camera).chain(&mut self.shadow) {
            list.regrow(device, self.cull_layout, meta_size, indirect_size);
        }

        self.max_meta = new_max;
    }

    fn free_slice(&mut self, (vo, vl, io, il): (u32, u32, u32, u32)) {
        self.vtx_free.free_region(vo, vl);
        self.idx_free.free_region(io, il);
    }

    fn free_slices(&mut self, slices: &[(u32, u32, u32, u32)]) {
        for &slice in slices {
            self.free_slice(slice);
        }
    }

    fn retire_slices(&mut self, slices: impl IntoIterator<Item = (u32, u32, u32, u32)>) {
        let retire_at = self.frame_seq + MAX_FRAMES_IN_FLIGHT as u64;
        for slice in slices {
            self.pending_free.push_back((retire_at, slice));
        }
    }

    pub fn begin_frame(&mut self) {
        self.frame_seq += 1;
        while self
            .pending_free
            .front()
            .is_some_and(|&(retire_at, _)| retire_at <= self.frame_seq)
        {
            let (_, slice) = self.pending_free.pop_front().unwrap();
            self.free_slice(slice);
        }
    }

    pub fn remove(&mut self, pos: &ChunkPos) {
        if let Some(alloc) = self.chunks.remove(pos) {
            self.retire_slices(alloc.sections.iter().map(|sec| {
                (
                    sec.vertex_offset as u32,
                    sec.vtx_len,
                    sec.first_index,
                    sec.idx_len,
                )
            }));
            self.meta_dirty = true;
        }
    }

    pub fn set_format(&mut self, device: &rhi::backend::Device, format: TerrainFormat) {
        self.clear();
        if format == self.format {
            return;
        }
        self.format = format;
        std::mem::take(&mut self.vertex_buffer).destroy(device);
        self.vertex_buffer = create_pool(
            device,
            self.use_staging,
            "vertex_pool",
            u64::from(self.pool_vertices) * format.vertex_size(),
            rhi::BufferUsages::VERTEX,
        );
    }

    pub fn format(&self) -> TerrainFormat {
        self.format
    }

    pub fn clear(&mut self) {
        self.chunks.clear();
        self.vtx_free.reset();
        self.idx_free.reset();
        self.pending_free.clear();
        self.camera.meta.clear();
        self.meta_dirty = true;
        self.fade_enabled = false;
    }

    #[cfg(feature = "test-harness")]
    pub fn slice_overlap(&self) -> Option<String> {
        let mut vertex = self.vtx_free.free.clone();
        let mut index = self.idx_free.free.clone();
        let live = self
            .chunks
            .values()
            .flat_map(|c| &c.sections)
            .map(|s| (s.vertex_offset as u32, s.vtx_len, s.first_index, s.idx_len));
        let retired = self.pending_free.iter().map(|&(_, slice)| slice);
        for (vtx_off, vtx_len, idx_off, idx_len) in live.chain(retired) {
            vertex.push((vtx_off, vtx_len));
            index.push((idx_off, idx_len));
        }
        [("vertex", vertex), ("index", index)]
            .into_iter()
            .find_map(|(pool, mut ranges)| {
                ranges.sort_unstable();
                ranges
                    .windows(2)
                    .find(|w| w[0].0 + w[0].1 > w[1].0)
                    .map(|w| format!("{pool} ranges {:?} and {:?} overlap", w[0], w[1]))
            })
    }

    pub fn chunk_count(&self) -> u32 {
        self.chunks.len() as u32
    }

    pub fn set_chunk_visibility(&mut self, vis: HashMap<ChunkPos, u32>) {
        self.chunk_visibility = vis;
        self.meta_dirty = true;
    }

    pub fn dispatch_cull(
        &mut self,
        encoder: &rhi::backend::CommandEncoder,
        frame: usize,
        frustum: &[[f32; 4]; 6],
        anchor: DVec3,
        eye: DVec3,
    ) {
        if self.chunks.is_empty() {
            return;
        }

        let now = crate::clock::Instant::now();
        const SORT_RECAM_SQ: f64 = 64.0;

        let any_fading = self.fade_enabled && now < self.fade_until;
        let content_changed = self.meta_dirty || any_fading;

        if content_changed {
            build_meta(
                &self.chunks,
                Some(&self.chunk_visibility),
                self.fade_enabled,
                eye,
                now,
                &mut self.camera.meta,
            );
            self.meta_dirty = false;
            self.shadow_dirty = true;
        }

        let cam_moved = (eye - self.last_sort_cam).length_squared() > SORT_RECAM_SQ;
        if content_changed || cam_moved {
            sort_meta(&mut self.camera.meta, eye);
            self.last_sort_cam = eye;
            self.camera.upload_pending = MAX_FRAMES_IN_FLIGHT as u32;
        }

        if let Some(pipeline) = &self.compute_pipeline {
            self.last_draw_count = self.count_readbacks[frame].read(|bytes| {
                let counts: [u32; 2] = bytemuck::pod_read_unaligned(&bytes[..8]);
                counts[0] + counts[1]
            });
            self.camera.cull(
                encoder,
                pipeline,
                frame,
                frustum,
                anchor,
                eye,
                !self.indirect_draw.count,
            );
            encoder.transition_buffers(rhi::BufferUse::StorageWrite, rhi::BufferUse::CopySrc);
            let readback = self.count_readbacks[frame].handle();
            for (i, count) in [
                &self.camera.count_buffers[frame],
                &self.camera.count_cutout_buffers[frame],
            ]
            .into_iter()
            .enumerate()
            {
                encoder.copy_buffer(
                    count.handle(),
                    readback,
                    &[rhi::BufferCopy {
                        src_offset: 0,
                        dst_offset: i as u64 * COUNT_SIZE,
                        size: COUNT_SIZE,
                    }],
                );
            }
            encoder.transition_buffers(rhi::BufferUse::CopyDst, rhi::BufferUse::HostRead);
        } else {
            self.camera.cull_on_cpu(frame, frustum, eye);
            self.last_draw_count = self.camera.visible_solid.len() as u32;
        }

        if !self.fade_enabled {
            self.fade_enabled = true;
        }
    }

    pub fn dispatch_shadow_cull(
        &mut self,
        device: &rhi::backend::Device,
        encoder: &rhi::backend::CommandEncoder,
        frame: usize,
        frustum: &[[f32; 4]; 6],
        anchor: DVec3,
        eye: DVec3,
    ) {
        let Some(pipeline) = &self.compute_pipeline else {
            return;
        };
        if self.chunks.is_empty() {
            return;
        }
        let shadow = self.shadow.get_or_insert_with(|| {
            let meta_size = (self.max_meta * size_of::<ChunkMeta>()) as u64;
            let indirect_size = (self.max_meta * size_of::<DrawCommand>()) as u64;
            DrawList::new(device, self.cull_layout, meta_size, indirect_size)
        });
        if self.shadow_dirty {
            build_meta(
                &self.chunks,
                None,
                self.fade_enabled,
                eye,
                crate::clock::Instant::now(),
                &mut shadow.meta,
            );
            shadow.upload_pending = MAX_FRAMES_IN_FLIGHT as u32;
            self.shadow_dirty = false;
        }
        shadow.cull(
            encoder,
            pipeline,
            frame,
            frustum,
            anchor,
            eye,
            !self.indirect_draw.count,
        );
    }

    pub fn draw_indirect(
        &self,
        pass: &rhi::backend::RenderPassEncoder,
        frame: usize,
        cutout: bool,
        view: View,
    ) {
        let list = match view {
            View::Camera => &self.camera,
            View::Shadow => match &self.shadow {
                Some(shadow) => shadow,
                None => return,
            },
        };
        if self.chunks.is_empty() {
            return;
        }

        if self.compute_pipeline.is_none() {
            let visible = if cutout {
                &list.visible_cutout
            } else {
                &list.visible_solid
            };
            let vertex_size = self.format.vertex_size();
            pass.set_index_buffer(self.index_buffer.handle());
            for draw in visible {
                pass.set_vertex_buffer_at(
                    0,
                    self.vertex_buffer.handle(),
                    draw.vertex_offset as u64 * vertex_size,
                );
                pass.set_vertex_buffer_at(
                    1,
                    list.meta_buffers[frame].handle(),
                    draw.meta_index as u64 * size_of::<ChunkMeta>() as u64,
                );
                pass.draw_indexed(draw.index_count, 1, draw.first_index, 0, 0);
            }
            return;
        }

        let max_draws = self
            .chunks
            .values()
            .map(|c| c.sections.len() as u32)
            .sum::<u32>();
        let (indirect, count) = if cutout {
            (
                list.indirect_cutout_buffers[frame].handle(),
                list.count_cutout_buffers[frame].handle(),
            )
        } else {
            (
                list.indirect_buffers[frame].handle(),
                list.count_buffers[frame].handle(),
            )
        };

        pass.set_vertex_buffer(0, self.vertex_buffer.handle());
        pass.set_vertex_buffer(1, list.meta_buffers[frame].handle());
        pass.set_index_buffer(self.index_buffer.handle());
        let stride = size_of::<DrawCommand>() as u32;
        if self.indirect_draw.count {
            pass.draw_indexed_indirect_count(indirect, count, max_draws, stride);
        } else if self.indirect_draw.multi_draw {
            pass.draw_indexed_indirect(indirect, 0, max_draws, stride);
        } else {
            for i in 0..max_draws {
                pass.draw_indexed_indirect(indirect, (i * stride) as u64, 1, stride);
            }
        }
    }

    fn section_visibility(nearby: bool, sec: &SectionAlloc, now: crate::clock::Instant) -> f32 {
        if nearby {
            return 1.0;
        }
        let elapsed_ms = now.duration_since(sec.uploaded_at).as_secs_f32() * 1000.0;
        (elapsed_ms / FADE_DURATION_MS).min(1.0)
    }

    pub fn draw_water(
        &self,
        pass: &rhi::backend::RenderPassEncoder,
        frustum: &[[f32; 4]; 6],
        anchor: DVec3,
        eye: DVec3,
    ) {
        if self.chunks.is_empty() {
            return;
        }

        pass.set_vertex_buffer(0, self.vertex_buffer.handle());
        pass.set_index_buffer(self.index_buffer.handle());

        let gpu_cull = self.compute_pipeline.is_some();
        let vertex_size = self.format.vertex_size();
        let now = crate::clock::Instant::now();
        for (pos, alloc) in self.chunks.iter() {
            let col_vis = self.chunk_visibility.get(pos).copied().unwrap_or(u32::MAX);
            let nearby = column_nearby(self.fade_enabled, *pos, eye);
            for sec in &alloc.sections {
                if sec.water_index_count == 0
                    || col_vis & (1u32 << sec.section_index) == 0
                    || !aabb_in_frustum(&sec.aabb, sec.origin, frustum, eye)
                {
                    continue;
                }
                let vis = Self::section_visibility(nearby, sec, now);
                let rel = (origin_dvec(sec.origin) - anchor).as_vec3();
                let origin_fade = [rel.x, rel.y, rel.z, vis];
                pass.set_push_constants(
                    rhi::ShaderStages::VERTEX,
                    0,
                    bytemuck::bytes_of(&origin_fade),
                );
                let base_vertex = if gpu_cull {
                    sec.vertex_offset
                } else {
                    pass.set_vertex_buffer_at(
                        0,
                        self.vertex_buffer.handle(),
                        sec.vertex_offset as u64 * vertex_size,
                    );
                    0
                };
                pass.draw_indexed(
                    sec.water_index_count,
                    1,
                    sec.water_first_index,
                    base_vertex,
                    0,
                );
            }
        }
    }

    pub fn destroy(&mut self, device: &rhi::backend::Device) {
        for buffer in [&mut self.vertex_buffer, &mut self.index_buffer] {
            std::mem::take(buffer).destroy(device);
        }
        for list in std::iter::once(&mut self.camera).chain(&mut self.shadow) {
            list.destroy(device);
        }
        util::destroy_buffers(device, &mut self.count_readbacks);
        std::mem::take(&mut self.staging_buffer).destroy(device);

        if let Some(pipeline) = &self.compute_pipeline {
            pipeline.destroy(device);
        }
        if let Some(layout) = &self.compute_layout {
            layout.destroy(device);
        }
        if let Some(layout) = &self.cull_layout {
            layout.destroy(device);
        }
    }
}

fn create_pool(
    device: &rhi::backend::Device,
    use_staging: bool,
    label: &str,
    size: u64,
    usage: rhi::BufferUsages,
) -> rhi::backend::Buffer {
    if use_staging {
        rhi::backend::Buffer::new(
            device,
            label,
            size,
            usage | rhi::BufferUsages::COPY_DST,
            rhi::MemoryLocation::Device,
        )
    } else {
        rhi::backend::Buffer::new(device, label, size, usage, rhi::MemoryLocation::Host)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum View {
    Camera,
    Shadow,
}

struct DrawList {
    meta: Vec<ChunkMeta>,
    upload_pending: u32,
    cull_sets: Vec<rhi::backend::BindGroup>,
    meta_buffers: Vec<rhi::backend::Buffer>,
    frustum_buffers: Vec<rhi::backend::Buffer>,
    indirect_buffers: Vec<rhi::backend::Buffer>,
    count_buffers: Vec<rhi::backend::Buffer>,
    indirect_cutout_buffers: Vec<rhi::backend::Buffer>,
    count_cutout_buffers: Vec<rhi::backend::Buffer>,
    visible_solid: Vec<VisibleDraw>,
    visible_cutout: Vec<VisibleDraw>,
}

struct VisibleDraw {
    meta_index: u32,
    first_index: u32,
    index_count: u32,
    vertex_offset: i32,
}

impl DrawList {
    fn new(
        device: &rhi::backend::Device,
        layout: Option<rhi::backend::BindGroupLayout>,
        meta_size: u64,
        indirect_size: u64,
    ) -> Self {
        let gpu = layout.is_some();
        let count = |name: &str| {
            if !gpu {
                return Vec::new();
            }
            util::create_per_frame_host_buffers(
                device,
                COUNT_SIZE,
                rhi::BufferUsages::STORAGE
                    | rhi::BufferUsages::INDIRECT
                    | rhi::BufferUsages::COPY_SRC,
                name,
            )
        };
        let mut this = Self {
            meta: Vec::new(),
            upload_pending: 0,
            cull_sets: if gpu {
                vec![rhi::backend::BindGroup::default(); MAX_FRAMES_IN_FLIGHT]
            } else {
                Vec::new()
            },
            meta_buffers: Vec::new(),
            frustum_buffers: if gpu {
                util::create_per_frame_host_buffers(
                    device,
                    FRUSTUM_SIZE,
                    rhi::BufferUsages::UNIFORM,
                    "frustum_ubo",
                )
            } else {
                Vec::new()
            },
            indirect_buffers: Vec::new(),
            count_buffers: count("draw_count"),
            indirect_cutout_buffers: Vec::new(),
            count_cutout_buffers: count("draw_count_cutout"),
            visible_solid: Vec::new(),
            visible_cutout: Vec::new(),
        };
        this.regrow(device, layout, meta_size, indirect_size);
        this
    }

    fn cull_on_cpu(&mut self, frame: usize, frustum: &[[f32; 4]; 6], eye: DVec3) {
        if self.upload_pending > 0 {
            self.meta_buffers[frame].write(bytemuck::cast_slice(&self.meta));
            self.upload_pending -= 1;
        }
        self.visible_solid.clear();
        self.visible_cutout.clear();
        for (index, meta) in self.meta.iter().enumerate() {
            if meta.index_count == 0 {
                continue;
            }
            let aabb = ChunkAABB {
                min: meta.aabb_min,
                max: meta.aabb_max,
            };
            if !aabb_in_frustum(&aabb, meta.origin, frustum, eye) {
                continue;
            }
            let draw = |first_index: u32, index_count: u32| VisibleDraw {
                meta_index: index as u32,
                first_index,
                index_count,
                vertex_offset: meta.vertex_offset,
            };
            let solid = meta.solid_index_count;
            if solid > 0 {
                self.visible_solid.push(draw(meta.first_index, solid));
            }
            if meta.index_count > solid {
                self.visible_cutout
                    .push(draw(meta.first_index + solid, meta.index_count - solid));
            }
        }
    }

    fn regrow(
        &mut self,
        device: &rhi::backend::Device,
        layout: Option<rhi::backend::BindGroupLayout>,
        meta_size: u64,
        indirect_size: u64,
    ) {
        let Some(layout) = layout else {
            util::destroy_buffers(device, &mut self.meta_buffers);
            self.meta_buffers = util::create_per_frame_host_buffers(
                device,
                meta_size,
                rhi::BufferUsages::VERTEX,
                "chunk_meta",
            );
            self.upload_pending = MAX_FRAMES_IN_FLIGHT as u32;
            return;
        };
        for buffers in [
            &mut self.meta_buffers,
            &mut self.indirect_buffers,
            &mut self.indirect_cutout_buffers,
        ] {
            util::destroy_buffers(device, buffers);
        }
        let indirect = |name: &str| {
            util::create_per_frame_host_buffers(
                device,
                indirect_size,
                rhi::BufferUsages::STORAGE | rhi::BufferUsages::INDIRECT,
                name,
            )
        };
        self.meta_buffers = util::create_per_frame_host_buffers(
            device,
            meta_size,
            rhi::BufferUsages::STORAGE | rhi::BufferUsages::VERTEX,
            "chunk_meta",
        );
        self.indirect_buffers = indirect("indirect_cmds");
        self.indirect_cutout_buffers = indirect("indirect_cmds_cutout");
        self.upload_pending = MAX_FRAMES_IN_FLIGHT as u32;

        let storage = rhi::backend::BindingResource::storage;
        let uniform = rhi::backend::BindingResource::uniform;
        for i in 0..MAX_FRAMES_IN_FLIGHT {
            let bindings = [
                (0, storage(&self.meta_buffers[i], meta_size)),
                (1, uniform(&self.frustum_buffers[i], FRUSTUM_SIZE)),
                (2, storage(&self.indirect_buffers[i], indirect_size)),
                (3, storage(&self.count_buffers[i], COUNT_SIZE)),
                (4, storage(&self.indirect_cutout_buffers[i], indirect_size)),
                (5, storage(&self.count_cutout_buffers[i], COUNT_SIZE)),
            ];
            self.cull_sets[i].replace(device, layout, &bindings);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn cull(
        &mut self,
        encoder: &rhi::backend::CommandEncoder,
        pipeline: &rhi::backend::ComputePipeline,
        frame: usize,
        frustum: &[[f32; 4]; 6],
        anchor: DVec3,
        eye: DVec3,
        zero_indirect: bool,
    ) {
        if self.upload_pending > 0 {
            self.meta_buffers[frame].write(bytemuck::cast_slice(&self.meta));
            self.upload_pending -= 1;
        }
        let count = self.meta.len() as u32;
        let frustum_data = FrustumData {
            planes: *frustum,
            chunk_count: count,
            cam_block: anchor.as_ivec3().to_array(),
            frac: (eye - anchor).as_vec3().to_array(),
            _pad: 0.0,
        };
        self.frustum_buffers[frame].write(bytemuck::bytes_of(&frustum_data));
        for buffer in [
            &mut self.count_buffers[frame],
            &mut self.count_cutout_buffers[frame],
        ] {
            buffer.write(&0u32.to_ne_bytes());
        }
        if zero_indirect {
            for buffer in [
                &mut self.indirect_buffers[frame],
                &mut self.indirect_cutout_buffers[frame],
            ] {
                buffer.zero();
            }
        }
        encoder.dispatch(pipeline, self.cull_sets[frame], [count.div_ceil(64), 1, 1]);
        encoder.transition_buffers(rhi::BufferUse::StorageWrite, rhi::BufferUse::Indirect);
    }

    fn destroy(&mut self, device: &rhi::backend::Device) {
        for buffers in [
            &mut self.meta_buffers,
            &mut self.frustum_buffers,
            &mut self.indirect_buffers,
            &mut self.count_buffers,
            &mut self.indirect_cutout_buffers,
            &mut self.count_cutout_buffers,
        ] {
            util::destroy_buffers(device, buffers);
        }
        util::destroy_bind_groups(device, self.cull_sets.drain(..));
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::clock::Instant;

    #[test]
    fn build_meta_masks_fades_and_sorts_front_to_back() {
        let now = Instant::now();
        let near = ChunkPos::new(0, 0);
        let far = ChunkPos::new(4, 0);
        let mut chunks = HashMap::new();
        chunks.insert(near, ChunkAlloc::synthetic(near, -64, [4, 5], now));
        chunks.insert(
            far,
            ChunkAlloc::synthetic(far, -64, [4], now - Duration::from_millis(500)),
        );
        let vis = HashMap::from([(near, 1u32 << 4)]);
        let eye = DVec3::new(8.0, 8.0, 8.0);

        let mut meta = Vec::new();
        build_meta(&chunks, Some(&vis), true, eye, now, &mut meta);
        sort_meta(&mut meta, eye);

        assert_eq!(meta.len(), 2);
        assert_eq!(meta[0].origin, [0, 0, 0]);
        assert_eq!(f32::from_bits(meta[0].visibility), 1.0);
        assert_eq!(meta[1].origin, [64, 0, 0]);
        let far_vis = f32::from_bits(meta[1].visibility);
        assert!((far_vis - 0.5).abs() < 0.01, "far fade {far_vis}");

        build_meta(&chunks, Some(&vis), false, eye, now, &mut meta);
        assert!(meta.iter().all(|m| f32::from_bits(m.visibility) == 1.0));

        build_meta(&chunks, None, false, eye, now, &mut meta);
        assert_eq!(meta.len(), 3);
    }
}
