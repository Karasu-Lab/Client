use std::path::{Path, PathBuf};

use crate::assets::{AssetIndex, resolve_asset_path};
use crate::renderer::{rhi, shader, util};

const FACE_TO_LAYER: [u32; 6] = [4, 1, 5, 0, 2, 3];

const PARAMS_LAYOUT: &[rhi::BindGroupLayoutEntry] = &[rhi::BindGroupLayoutEntry::uniform(
    rhi::ShaderStages::FRAGMENT,
)];
const CUBE_LAYOUT: &[rhi::BindGroupLayoutEntry] = &[
    rhi::BindGroupLayoutEntry::texture(rhi::ShaderStages::FRAGMENT),
    rhi::BindGroupLayoutEntry::sampler(rhi::ShaderStages::FRAGMENT),
];

pub struct PanoramaPipeline {
    pipeline: rhi::backend::RenderPipeline,
    pipeline_layout: rhi::backend::PipelineLayout,
    params_layout: rhi::backend::BindGroupLayout,
    cube_layout: rhi::backend::BindGroupLayout,
    params_sets: Vec<rhi::backend::BindGroup>,
    cube_set: rhi::backend::BindGroup,
    params_buffers: Vec<rhi::backend::Buffer>,
    cube: rhi::backend::Texture,
    cube_sampler: rhi::backend::Sampler,
    has_cubemap: bool,
    faces: Option<[PathBuf; 6]>,
}

impl PanoramaPipeline {
    pub fn new(
        device: &rhi::backend::Device,
        targets: rhi::backend::PipelineTargets,
        faces: Option<[PathBuf; 6]>,
    ) -> Self {
        let params_layout = rhi::backend::create_bind_group_layout(device, PARAMS_LAYOUT);
        let cube_layout = rhi::backend::create_bind_group_layout(device, CUBE_LAYOUT);
        let pipeline_layout =
            rhi::backend::create_pipeline_layout(device, &[params_layout, cube_layout], None);

        let pipeline = create_pipeline(device, targets, pipeline_layout);

        let (params_sets, params_buffers) =
            util::create_uniform_sets(device, params_layout, 16, "panorama_params");

        let (cube, cube_sampler, has_cubemap) = load_cubemap(device, faces.as_ref());

        let cube_set = util::create_texture_group(device, cube_layout, cube.handle(), cube_sampler);

        Self {
            pipeline,
            pipeline_layout,
            params_layout,
            cube_layout,
            params_sets,
            cube_set,
            params_buffers,
            cube,
            cube_sampler,
            has_cubemap,
            faces,
        }
    }

    pub fn draw(
        &mut self,
        pass: &mut rhi::backend::RenderPassEncoder,
        frame: usize,
        scroll: f32,
        aspect: f32,
        blur: f32,
    ) {
        if !self.has_cubemap {
            return;
        }

        let data: [f32; 4] = [scroll, aspect, blur, 0.0];
        self.params_buffers[frame].write(bytemuck::cast_slice(&data));

        pass.set_pipeline(&self.pipeline);
        pass.set_bind_groups(0, &[self.params_sets[frame], self.cube_set]);
        pass.draw(3, 1, 0, 0);
    }

    pub fn reload_cubemap(&mut self, device: &rhi::backend::Device, faces: Option<[PathBuf; 6]>) {
        if faces == self.faces {
            return;
        }
        device.wait_idle();
        self.destroy_cube(device);

        let (cube, cube_sampler, has_cubemap) = load_cubemap(device, faces.as_ref());

        self.faces = faces;
        self.cube = cube;
        self.cube_sampler = cube_sampler;
        self.has_cubemap = has_cubemap;

        self.cube_set.replace(
            device,
            self.cube_layout,
            &util::texture_binding(self.cube.handle(), self.cube_sampler),
        );
    }

    fn destroy_cube(&mut self, device: &rhi::backend::Device) {
        self.cube_sampler.destroy(device);
        std::mem::take(&mut self.cube).destroy(device);
    }

    pub fn recreate_pipeline(
        &mut self,
        device: &rhi::backend::Device,
        targets: rhi::backend::PipelineTargets,
    ) {
        self.pipeline.destroy(device);
        self.pipeline = create_pipeline(device, targets, self.pipeline_layout);
    }

    pub fn destroy(&mut self, device: &rhi::backend::Device) {
        util::destroy_buffers(device, &mut self.params_buffers);
        self.destroy_cube(device);

        self.pipeline.destroy(device);
        self.pipeline_layout.destroy(device);
        util::destroy_bind_groups(device, self.params_sets.drain(..));
        self.cube_set.destroy(device);
        self.params_layout.destroy(device);
        self.cube_layout.destroy(device);
    }
}

fn resolve_panorama_face(
    i: u32,
    jar_assets_dir: &Path,
    asset_index: &Option<AssetIndex>,
) -> Option<PathBuf> {
    let flat = jar_assets_dir.join(format!("panorama_{i}.png"));
    if pomme_vfs::exists(&flat) {
        return Some(flat);
    }
    let asset_key = format!("minecraft/textures/gui/title/background/panorama_{i}.png");
    let path = resolve_asset_path(jar_assets_dir, asset_index, &asset_key);
    pomme_vfs::exists(&path).then_some(path)
}

pub fn resolve_panorama_faces(
    panorama_dir: &Path,
    jar_assets_dir: &Path,
    asset_index: &Option<AssetIndex>,
) -> Option<[PathBuf; 6]> {
    let collect = |resolve: &dyn Fn(u32) -> Option<PathBuf>| -> Option<[PathBuf; 6]> {
        let faces: Vec<PathBuf> = (0..6).map(resolve).collect::<Option<_>>()?;
        faces.try_into().ok()
    };
    collect(&|i| {
        let flat = panorama_dir.join(format!("panorama_{i}.png"));
        pomme_vfs::exists(&flat).then_some(flat)
    })
    .or_else(|| collect(&|i| resolve_panorama_face(i, jar_assets_dir, asset_index)))
}

fn flip_horizontal(data: &[u8], w: u32, h: u32) -> Vec<u8> {
    let mut out = vec![0u8; data.len()];
    let stride = (w * 4) as usize;
    for y in 0..h as usize {
        for x in 0..w as usize {
            let src = y * stride + x * 4;
            let dst = y * stride + (w as usize - 1 - x) * 4;
            out[dst..dst + 4].copy_from_slice(&data[src..src + 4]);
        }
    }
    out
}

fn load_cubemap(
    device: &rhi::backend::Device,
    face_paths: Option<&[PathBuf; 6]>,
) -> (rhi::backend::Texture, rhi::backend::Sampler, bool) {
    let Some(face_paths) = face_paths else {
        tracing::info!("Panorama faces not found, skipping cubemap");
        return create_fallback_cubemap(device);
    };

    let mut faces: Vec<Vec<u8>> = Vec::new();
    let mut face_w = 0u32;
    let mut face_h = 0u32;

    for (i, path) in face_paths.iter().enumerate() {
        match util::load_png(path) {
            Some((data, w, h)) if w > 1 && h > 1 => {
                face_w = w;
                face_h = h;
                faces.push(data);
            }
            _ => {
                tracing::info!("Panorama face {i} is a placeholder, skipping cubemap");
                return create_fallback_cubemap(device);
            }
        }
    }

    let face_bytes = (face_w * face_h * 4) as usize;
    let mut staging_data = vec![0u8; face_bytes * 6];

    for (panorama_idx, face_data) in faces.iter().enumerate() {
        let layer = FACE_TO_LAYER[panorama_idx] as usize;
        let flipped = flip_horizontal(face_data, face_w, face_h);
        staging_data[layer * face_bytes..(layer + 1) * face_bytes].copy_from_slice(&flipped);
    }

    let mip_levels = mip_levels_for(face_w, face_h);
    let (mip_data, copies) = cubemap_mips(&staging_data, face_w, face_h, mip_levels);
    let texture = create_cubemap(device, face_w, face_h);
    let staging_buffer = rhi::backend::Buffer::with_data(
        device,
        "panorama_cubemap_staging",
        &mip_data,
        rhi::BufferUsages::COPY_SRC,
    );

    upload_cubemap(
        device,
        staging_buffer.handle(),
        texture.handle(),
        mip_levels,
        &copies,
    );
    staging_buffer.destroy(device);

    let sampler =
        rhi::backend::create_sampler(device, &rhi::SamplerDesc::LINEAR.with_mips(mip_levels));

    tracing::info!("Panorama cubemap loaded: {face_w}x{face_h} per face, {mip_levels} mip levels");

    (texture, sampler, true)
}

fn mip_levels_for(w: u32, h: u32) -> u32 {
    (w.max(h) as f32).log2().floor() as u32 + 1
}

fn create_cubemap(device: &rhi::backend::Device, width: u32, height: u32) -> rhi::backend::Texture {
    rhi::backend::Texture::new(
        device,
        &rhi::TextureDesc {
            layers: 6,
            mip_levels: mip_levels_for(width, height),
            dimension: rhi::TextureViewDimension::Cube,
            ..rhi::TextureDesc::new(
                "panorama_cubemap",
                width,
                height,
                rhi::TextureFormat::Rgba8Unorm,
                rhi::TextureUsages::COPY_DST
                    | rhi::TextureUsages::COPY_SRC
                    | rhi::TextureUsages::TEXTURE_BINDING,
            )
        },
    )
}

fn downsample(pixels: &[u8], width: u32, height: u32) -> Vec<u8> {
    let (next_w, next_h) = ((width / 2).max(1), (height / 2).max(1));
    let mut out = Vec::with_capacity((next_w * next_h * 4) as usize);
    for y in 0..next_h {
        for x in 0..next_w {
            let xs = [(x * 2).min(width - 1), (x * 2 + 1).min(width - 1)];
            let ys = [(y * 2).min(height - 1), (y * 2 + 1).min(height - 1)];
            for channel in 0..4 {
                let sum: u32 = ys
                    .iter()
                    .flat_map(|&row| xs.iter().map(move |&column| (row, column)))
                    .map(|(row, column)| {
                        u32::from(pixels[((row * width + column) * 4) as usize + channel])
                    })
                    .sum();
                out.push(((sum + 2) / 4) as u8);
            }
        }
    }
    out
}

fn cubemap_mips(
    base: &[u8],
    width: u32,
    height: u32,
    mip_levels: u32,
) -> (Vec<u8>, Vec<rhi::TexelCopy>) {
    let face_bytes = (width * height * 4) as usize;
    let mut faces: Vec<Vec<u8>> = (0..6)
        .map(|layer| base[layer * face_bytes..(layer + 1) * face_bytes].to_vec())
        .collect();
    let (mut level_w, mut level_h) = (width, height);
    let mut data = Vec::new();
    let mut copies = Vec::new();
    for level in 0..mip_levels {
        for (layer, pixels) in faces.iter().enumerate() {
            copies.push(rhi::TexelCopy {
                buffer_offset: data.len() as u64,
                mip_level: level,
                layer: layer as u32,
                origin: [0, 0],
                width: level_w,
                height: level_h,
            });
            data.extend_from_slice(pixels);
        }
        if level + 1 < mip_levels {
            faces = faces
                .iter()
                .map(|pixels| downsample(pixels, level_w, level_h))
                .collect();
            level_w = (level_w / 2).max(1);
            level_h = (level_h / 2).max(1);
        }
    }
    (data, copies)
}

fn upload_cubemap(
    device: &rhi::backend::Device,
    staging_buffer: rhi::backend::BufferHandle,
    texture: rhi::backend::TextureHandle,
    mip_levels: u32,
    copies: &[rhi::TexelCopy],
) {
    use rhi::TextureUse;

    let all = rhi::TextureRange {
        first_mip: 0,
        mips: mip_levels,
        first_layer: 0,
        layers: 6,
    };
    device.submit(|encoder| {
        encoder.transition_textures(&[rhi::backend::TextureTransition {
            texture,
            range: all,
            from: TextureUse::Undefined,
            to: TextureUse::CopyDst,
        }]);
        encoder.copy_buffer_to_texture(staging_buffer, texture, copies);
        encoder.transition_textures(&[rhi::backend::TextureTransition {
            texture,
            range: all,
            from: TextureUse::CopyDst,
            to: TextureUse::ShaderRead,
        }]);
    });
}

fn create_fallback_cubemap(
    device: &rhi::backend::Device,
) -> (rhi::backend::Texture, rhi::backend::Sampler, bool) {
    let texture = create_cubemap(device, 1, 1);
    let sampler = rhi::backend::create_sampler(device, &rhi::SamplerDesc::LINEAR_REPEAT);

    (texture, sampler, false)
}

fn create_pipeline(
    device: &rhi::backend::Device,
    targets: rhi::backend::PipelineTargets,
    layout: rhi::backend::PipelineLayout,
) -> rhi::backend::RenderPipeline {
    rhi::backend::create_render_pipeline(
        device,
        targets,
        layout,
        &rhi::RenderPipelineDesc::new(
            "panorama",
            shader::include_shader!("panorama.vert"),
            shader::include_shader!("panorama.frag"),
            &[],
        ),
    )
}
