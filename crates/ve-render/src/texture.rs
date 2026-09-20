//! GPU textures for decoded frames, and a budget for them.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use ve_core::{ColorSpace, Size};
use ve_media::{CacheKey, VideoFrame};
use ve_metrics::{counters, Metrics};

/// The format decoded frames are *stored* as.
///
/// Always the non-sRGB variant, because this is about storage, not
/// interpretation. The same bytes are read back either literally or as sRGB
/// depending on the composition's [`ColorSpace`], and which of those happens is
/// chosen by the *view*, not the texture — see [`SRGB_FRAME_FORMAT`].
pub const FRAME_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// The sRGB reinterpretation of [`FRAME_FORMAT`].
///
/// Every texture and target is created able to be viewed as either. Sampling
/// through this view makes the hardware decode sRGB to linear on the way in,
/// and rendering through it makes the hardware encode on the way out — so
/// linear-light compositing costs a different view rather than any shader
/// arithmetic. On hardware both conversions are fixed-function and effectively
/// free; on a software rasteriser they are real per-texel work, which the
/// `composite_1080p_4_layers` benchmark measures rather than assumes.
///
/// Doing the conversion this way, instead of in the shader, is what keeps
/// 8 bits usable: the values *stored* stay gamma-encoded, where the codes are
/// distributed the way the eye needs them. Blending linear light into a plain
/// `Rgba8Unorm` target would band visibly in the shadows.
pub const SRGB_FRAME_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// The storage format paired with the view format a colour space asks for.
pub fn view_format_for(color_space: ColorSpace) -> wgpu::TextureFormat {
    match color_space {
        ColorSpace::Perceptual => FRAME_FORMAT,
        ColorSpace::Linear => SRGB_FRAME_FORMAT,
    }
}

/// A process-unique identity for an uploaded texture.
///
/// The point of it is the composite cache. A texture's pixels never change
/// after upload — a new decoded frame means a new upload — so the identity of
/// the object *is* the identity of its contents, and a cached composite can be
/// keyed on which textures went into it without hashing a megabyte of pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TextureId(u64);

impl TextureId {
    /// The top bit separates the two ways an identity is minted, so a content
    /// hash can never collide with an upload counter.
    const CONTENT_BIT: u64 = 1 << 63;

    fn next() -> Self {
        // Relaxed is enough: the only requirement is that no two uploads ever
        // get the same number, which a fetch_add guarantees on its own.
        static NEXT: AtomicU64 = AtomicU64::new(1);
        TextureId(NEXT.fetch_add(1, Ordering::Relaxed) & !Self::CONTENT_BIT)
    }

    /// An identity for a texture whose *contents* are already identified by a
    /// hash — a composited picture held in the render cache.
    ///
    /// A nested composition is drawn into a target that is reused from frame to
    /// frame, so the object's identity says nothing about its pixels. Its
    /// composite key does, and using that here is what lets a parent composite
    /// be cached: two frames whose nested composition came out the same share an
    /// identity, and one whose nested composition changed does not.
    pub fn from_content(hash: u64) -> Self {
        TextureId(hash | Self::CONTENT_BIT)
    }

    pub fn raw(self) -> u64 {
        self.0
    }
}

/// A decoded frame living in GPU memory.
///
/// Carries its own bind group, built once at upload. Drawing a layer is then
/// two `set_bind_group` calls with nothing allocated per frame.
pub struct GpuTexture {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    /// One bind group per colour space, built at upload. They are a handful of
    /// descriptors over the same pixels, so holding both costs nothing next to
    /// the texture itself and removes any per-frame branch on the hot path.
    bind_groups: [wgpu::BindGroup; ColorSpace::ALL.len()],
    size: Size,
    bytes: usize,
    id: TextureId,
    /// Whether the pixels already carry their own alpha.
    ///
    /// A decoded frame does not: its colour is independent of its coverage. The
    /// output of a render pass does, because the shader premultiplies as it
    /// writes. Sampling the second kind as if it were the first multiplies the
    /// alpha in twice, which darkens every semi-transparent nested composite
    /// and every motion blur — so the renderer picks its fragment shader from
    /// this.
    premultiplied: bool,
}

impl GpuTexture {
    pub fn size(&self) -> Size {
        self.size
    }

    /// Whether this texture's colour already carries its alpha. See the field.
    pub fn is_premultiplied(&self) -> bool {
        self.premultiplied
    }

    /// This texture's identity, which stands in for its contents.
    pub fn id(&self) -> TextureId {
        self.id
    }

    /// The underlying texture, for a caller that needs to bind it itself.
    pub fn texture(&self) -> &wgpu::Texture {
        &self.texture
    }

    /// A view of the texture, for handing to the UI or to a nested pass.
    pub fn view(&self) -> &wgpu::TextureView {
        &self.view
    }

    /// GPU memory this texture occupies, for the budget.
    ///
    /// Zero for a texture that only borrows a render target's memory: the target
    /// is accounted for by the cache that owns it, and counting it twice would
    /// evict twice as much as it should.
    pub fn byte_size(&self) -> usize {
        self.bytes
    }

    pub(crate) fn new(
        texture: wgpu::Texture,
        view: wgpu::TextureView,
        bind_groups: [wgpu::BindGroup; ColorSpace::ALL.len()],
        size: Size,
    ) -> Self {
        let bytes = size.pixel_count() as usize * 4;
        GpuTexture {
            texture,
            view,
            bind_groups,
            size,
            bytes,
            id: TextureId::next(),
            premultiplied: false,
        }
    }

    /// The bind group that samples this texture for a given colour space.
    pub(crate) fn bind_group(&self, color_space: ColorSpace) -> &wgpu::BindGroup {
        &self.bind_groups[color_space as usize]
    }

    /// Builds a sampling bind group per colour space over one texture.
    pub(crate) fn bind_groups_for(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
        texture: &wgpu::Texture,
        label: &str,
    ) -> [wgpu::BindGroup; ColorSpace::ALL.len()] {
        ColorSpace::ALL.map(|space| {
            let view = texture.create_view(&wgpu::TextureViewDescriptor {
                label: Some(label),
                format: Some(view_format_for(space)),
                ..Default::default()
            });
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(label),
                layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(sampler),
                    },
                ],
            })
        })
    }

    /// Uploads a decoded frame into a new texture.
    ///
    /// Handles the stride mismatch that swscale's row alignment produces: wgpu
    /// wants `bytes_per_row` to be a multiple of 256, and a decoded frame's
    /// stride usually is not, so rows are repacked when they disagree. A
    /// tightly packed frame whose stride already satisfies the alignment is
    /// uploaded straight from the decoder's buffer with no copy at all.
    pub(crate) fn upload(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layout: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
        frame: &VideoFrame,
    ) -> GpuTexture {
        let size = frame.size();
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("verge-frame"),
            size: wgpu::Extent3d {
                width: size.width.max(1),
                height: size.height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FRAME_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            // Declared so the same pixels can be sampled either literally or as
            // sRGB, which is what makes the colour space a per-composition
            // choice rather than a property baked in at upload.
            view_formats: &[SRGB_FRAME_FORMAT],
        });

        const ALIGN: u32 = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let tight = size.width * 4;
        let padded = tight.div_ceil(ALIGN) * ALIGN;

        if frame.stride() == padded {
            // The decoder's rows already satisfy the copy alignment.
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                frame.data(),
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(size.height),
                },
                wgpu::Extent3d {
                    width: size.width.max(1),
                    height: size.height.max(1),
                    depth_or_array_layers: 1,
                },
            );
        } else {
            let mut packed = vec![0u8; (padded * size.height) as usize];
            for y in 0..size.height {
                let dst = (y * padded) as usize;
                let row = frame.row(y);
                packed[dst..dst + row.len()].copy_from_slice(row);
            }
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &packed,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(size.height),
                },
                wgpu::Extent3d {
                    width: size.width.max(1),
                    height: size.height.max(1),
                    depth_or_array_layers: 1,
                },
            );
        }

        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_groups = GpuTexture::bind_groups_for(
            device,
            layout,
            sampler,
            &texture,
            "verge-frame-bind-group",
        );

        GpuTexture::new(texture, view, bind_groups, size)
    }
}

impl GpuTexture {
    /// Builds a bindable texture over a render target's own texture.
    ///
    /// Shares the target's texture rather than copying it — `wgpu::Texture` is a
    /// handle — so a nested composition is sampled straight out of the target it
    /// was drawn into, with no intermediate copy.
    pub(crate) fn wrap_target(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
        target: &crate::target::RenderTarget,
        content: crate::cache::CompositeKey,
    ) -> GpuTexture {
        let texture = target.texture().clone();
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        // Both views again, so the level above can sample a nested composition
        // in whichever space *it* composites in. The nested picture is stored
        // encoded either way, so the two levels' choices stay independent.
        let bind_groups = GpuTexture::bind_groups_for(
            device,
            layout,
            sampler,
            &texture,
            "verge-nested-bind-group",
        );
        let size = target.size();
        GpuTexture {
            texture,
            view,
            bind_groups,
            size,
            bytes: 0,
            id: TextureId::from_content(content.raw()),
            // The output of a pass of this renderer, and so already
            // premultiplied.
            premultiplied: true,
        }
    }
}

/// Least-recently-used cache of uploaded frames, bounded by GPU memory.
///
/// Separate from the CPU-side frame cache because the budgets are different
/// resources with different pressures: VRAM is usually the scarcer of the two,
/// and evicting a GPU texture does not require re-decoding, only re-uploading.
pub struct TextureCache {
    entries: HashMap<CacheKey, (GpuTexture, u64)>,
    capacity_bytes: usize,
    bytes: usize,
    clock: u64,
    hits: u64,
    misses: u64,
    metrics: Option<Metrics>,
}

impl TextureCache {
    pub fn new(capacity_bytes: usize) -> Self {
        TextureCache {
            entries: HashMap::new(),
            capacity_bytes,
            bytes: 0,
            clock: 0,
            hits: 0,
            misses: 0,
            metrics: None,
        }
    }

    pub fn with_budget_mb(mb: usize) -> Self {
        TextureCache::new(mb * 1024 * 1024)
    }

    pub fn with_metrics(mut self, metrics: Metrics) -> Self {
        self.metrics = Some(metrics);
        self
    }

    pub fn get(&mut self, key: &CacheKey) -> Option<&GpuTexture> {
        self.touch(key).then(|| &self.entries.get(key).expect("just touched").0)
    }

    /// Marks an entry as recently used, reporting whether it was there.
    ///
    /// Separate from [`TextureCache::get`] because a render pass needs shared
    /// references to *several* textures at once, which a single `&mut self`
    /// lookup cannot hand out. Callers touch every key first, then read them
    /// all through [`TextureCache::peek`].
    pub fn touch(&mut self, key: &CacheKey) -> bool {
        self.clock += 1;
        let clock = self.clock;
        match self.entries.get_mut(key) {
            Some((_, last_used)) => {
                *last_used = clock;
                self.hits += 1;
                true
            }
            None => {
                self.misses += 1;
                false
            }
        }
    }

    /// Reads an entry without disturbing LRU order.
    pub fn peek(&self, key: &CacheKey) -> Option<&GpuTexture> {
        self.entries.get(key).map(|(texture, _)| texture)
    }

    pub fn contains(&self, key: &CacheKey) -> bool {
        self.entries.contains_key(key)
    }

    pub fn insert(&mut self, key: CacheKey, texture: GpuTexture) {
        let size = texture.byte_size();
        if size > self.capacity_bytes {
            return;
        }
        if let Some((old, _)) = self.entries.remove(&key) {
            self.bytes -= old.byte_size();
        }
        while self.bytes + size > self.capacity_bytes && !self.entries.is_empty() {
            let victim = self.entries.iter().min_by_key(|(_, (_, t))| *t).map(|(k, _)| *k);
            match victim {
                Some(k) => {
                    if let Some((old, _)) = self.entries.remove(&k) {
                        self.bytes -= old.byte_size();
                    }
                }
                None => break,
            }
        }
        self.clock += 1;
        self.bytes += size;
        self.entries.insert(key, (texture, self.clock));
        if let Some(m) = &self.metrics {
            m.set_gauge(counters::GPU_TEXTURE_BYTES, self.bytes as f64);
        }
    }

    pub fn invalidate_asset(&mut self, asset: ve_core::AssetId) {
        let doomed: Vec<CacheKey> =
            self.entries.keys().filter(|k| k.asset == asset).copied().collect();
        for key in doomed {
            if let Some((old, _)) = self.entries.remove(&key) {
                self.bytes -= old.byte_size();
            }
        }
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn hit_rate(&self) -> f64 {
        let total = self.hits + self.misses;
        if total == 0 {
            0.0
        } else {
            self.hits as f64 / total as f64
        }
    }
}
