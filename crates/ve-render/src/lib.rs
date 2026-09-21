//! GPU compositing, built on wgpu.
//!
//! # Why the GPU from the start
//!
//! A CPU compositor written first and ported later shapes every interface
//! around CPU assumptions: frames get copied into intermediate buffers, effects
//! run as sequential passes over host memory, and the port turns into a
//! rewrite. Starting on the GPU means the interfaces are already about
//! textures, render targets and passes, and the parts that are simple today —
//! one pipeline, one blend mode — grow into effect graphs without changing
//! shape.
//!
//! # The model
//!
//! A composition is a [`RenderTarget`] plus a back-to-front list of [`Layer`]s.
//! Each layer is a GPU texture, an affine transform and an opacity. Everything
//! later attaches to that: blend modes are pipeline variants, effects are
//! passes between layer draws, masks are extra bindings, and a nested
//! composition is a layer whose texture is another target's output.
//!
//! # Caching
//!
//! A composited picture is cached on a hash of everything that produced it, so
//! an unchanged composition is never drawn twice and changing one clip only
//! recomputes the instants that clip appears in. See [`cache`] for why the
//! cache is content-addressed rather than tracking dependencies.
//!
//! # Colour
//!
//! Frames are uploaded as non-sRGB `Rgba8Unorm` and composited non-linearly,
//! matching what editors coming from the established professional tools expect.
//! See [`texture::FRAME_FORMAT`] for the reasoning and for what a linear-light
//! mode would need.

pub mod cache;
pub mod effects;
mod gpu;
mod renderer;
pub mod scopes;
mod target;
pub mod texture;
pub mod transform;

pub use cache::{CompositeCache, CompositeCacheStats, CompositeKey};
pub use effects::{
    chain_passes, passes_for, three_way_response, white_balance_gains, EffectPass, Program,
};
pub use gpu::GpuContext;
pub use renderer::{Layer, Renderer};
pub use scopes::{
    vectorscope, waveform, Histogram, ScopeImage, ScopeSample, ScopeSampler, WaveformMode,
};
pub use target::RenderTarget;
pub use texture::{GpuTexture, TextureCache, TextureId, FRAME_FORMAT};
pub use transform::{fit_scale, layer_matrix};

#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    #[error("no suitable GPU adapter: {0}")]
    NoAdapter(String),
    #[error("could not create a GPU device: {0}")]
    NoDevice(String),
}
