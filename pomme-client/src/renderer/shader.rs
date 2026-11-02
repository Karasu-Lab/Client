/// The shader build.rs compiled for `name`: SPIR-V, or with `wgsl-shaders`
/// the WGSL it was translated to.
macro_rules! include_shader {
    ($name:literal) => {{
        #[cfg(not(feature = "wgsl-shaders"))]
        let shader: &'static [u8] = include_bytes!(concat!(env!("OUT_DIR"), "/", $name, ".spv"));
        #[cfg(feature = "wgsl-shaders")]
        let shader: &'static [u8] = include_bytes!(concat!(env!("OUT_DIR"), "/", $name, ".wgsl"));
        shader
    }};
}

pub(crate) use include_shader;
