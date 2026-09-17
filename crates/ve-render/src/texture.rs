//! GPU textures for decoded frames, and a budget for them.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use ve_core::Size;
use ve_media::{CacheKey, VideoFrame};
use ve_metrics::{counters, Metrics};

/// The texture format decoded frames are uploaded as.
///
/// Deliberately **not** the `Srgb` variant. Video is composited non-linearly
/// here, matching the default behaviour of the professional editors this is
/// measured against: an 8-bit source blended in linear light shifts every
/// crossfade and opacity ramp away from what an editor coming from those tools
/// expects. A linear-light compositing mode belongs in the colour-management
/// work later, as an explicit project setting, not as a silent default.
pub const FRAME_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// A process-unique identity for an uploaded texture.
///
/// The point of it is the composite cache. A texture's pixels never change
/// after upload — a new decoded frame means a new upload — so the identity of
/// the object *is* the identity of its contents, and a cached composite can be
/// keyed on which textures went into it without hashing a megabyte of pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TextureId(u64);

impl TextureId {
    fn next() -> Self {
        // Relaxed is enough: the only requirement is that no two uploads ever
        // get the same number, which a fetch_add guarantees on its own.
        static NEXT: AtomicU64 = AtomicU64::new(1);
        TextureId(NEXT.fetch_add(1, Ordering::Relaxed))
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
    pub(crate) bind_group: wgpu::BindGroup,
    size: Size,
    bytes: usize,
    id: TextureId,
}

impl GpuTexture {
    pub fn size(&self) -> Size {
        self.size
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
    pub fn byte_size(&self) -> usize {
        self.bytes
    }

    pub(crate) fn new(
        texture: wgpu::Texture,
        view: wgpu::TextureView,
        bind_group: wgpu::BindGroup,
        size: Size,
    ) -> Self {
        let bytes = size.pixel_count() as usize * 4;
        GpuTexture { texture, view, bind_group, size, bytes, id: TextureId::next() }
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
            view_formats: &[],
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
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("verge-frame-bind-group"),
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
        });

        GpuTexture::new(texture, view, bind_group, size)
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
