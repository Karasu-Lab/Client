//! Pomme's render hardware interface: backend-neutral descriptions of GPU
//! state, which the backend translates into its own objects. The shapes follow
//! WebGPU's (bind groups, vertex buffer layouts), apart from push constants.
//!
//! The descriptions live in `wgpu`, so the Vulkan and the wgpu backend take
//! the same types. The backend is chosen by feature: `wgpu` draws through
//! wgpu (WebGPU in the browser), else `vulkan` through Vulkan.

#[cfg(not(any(feature = "vulkan", feature = "wgpu")))]
compile_error!("enable a graphics backend: the `vulkan` or the `wgpu` feature");

pub use wgpu::rhi::*;

#[cfg(feature = "vulkan")]
pub mod vulkan;

#[cfg(all(feature = "vulkan", not(feature = "wgpu")))]
pub use vulkan as backend;
/// The backend this build draws with; renderer code reaches the GPU only
/// through it.
#[cfg(feature = "wgpu")]
pub use wgpu::backend;

/// A per-vertex buffer of `$vertex`s, its attributes packed as in
/// [`packed_attributes`] from location 0.
macro_rules! vertex_buffer {
    ($vertex:ty, [$($format:ident),+ $(,)?]) => {
        $crate::renderer::rhi::VertexBufferLayout {
            stride: size_of::<$vertex>() as u32,
            step_mode: $crate::renderer::rhi::VertexStepMode::Vertex,
            attributes: &$crate::renderer::rhi::packed_attributes(
                0,
                [$($crate::renderer::rhi::VertexFormat::$format),+],
            ),
        }
    };
}

pub(crate) use vertex_buffer;
