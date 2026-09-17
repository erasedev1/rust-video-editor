//! Offscreen render targets.

use ve_core::{ColorSpace, Size};

use crate::texture::{view_format_for, FRAME_FORMAT, SRGB_FRAME_FORMAT};

/// A texture the compositor draws into.
///
/// The preview, each nested composition and every exported frame all render
/// into one of these rather than straight to a window, which is what lets the
/// same code path serve preview, export and the test suite.
pub struct RenderTarget {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    /// One attachment view per colour space. Which one a pass attaches decides
    /// whether the hardware encodes on store, and so whether blending happens
    /// in linear light.
    views: [wgpu::TextureView; ColorSpace::ALL.len()],
    size: Size,
    format: wgpu::TextureFormat,
}

impl RenderTarget {
    pub fn new(device: &wgpu::Device, size: Size) -> Self {
        Self::with_format(device, size, FRAME_FORMAT)
    }

    pub fn with_format(device: &wgpu::Device, size: Size, format: wgpu::TextureFormat) -> Self {
        let size = Size::new(size.width.max(1), size.height.max(1));
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("verge-render-target"),
            size: wgpu::Extent3d {
                width: size.width,
                height: size.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            // RENDER_ATTACHMENT to draw into, TEXTURE_BINDING so the UI can
            // display it and a nested composition can sample it, COPY_SRC so
            // export and the tests can read it back, COPY_DST so a composite
            // kept in the render cache can be copied into the target the
            // interface is already drawing from.
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST,
            // Rendering through the sRGB view makes the hardware encode on
            // store, which is what lets linear blending land in 8 bits without
            // banding.
            view_formats: &[SRGB_FRAME_FORMAT],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        // A target created with an explicitly non-default format — the readback
        // tests use one — cannot be reinterpreted, so it keeps its own format
        // for both entries rather than claiming an sRGB view it never declared.
        let reinterpretable = format == FRAME_FORMAT;
        let views = ColorSpace::ALL.map(|space| {
            texture.create_view(&wgpu::TextureViewDescriptor {
                label: Some("verge-target-view"),
                format: Some(if reinterpretable { view_format_for(space) } else { format }),
                ..Default::default()
            })
        });
        RenderTarget { texture, view, views, size, format }
    }

    pub fn size(&self) -> Size {
        self.size
    }

    pub fn format(&self) -> wgpu::TextureFormat {
        self.format
    }

    pub fn view(&self) -> &wgpu::TextureView {
        &self.view
    }

    /// The attachment view for a colour space.
    pub fn view_for(&self, color_space: ColorSpace) -> &wgpu::TextureView {
        &self.views[color_space as usize]
    }

    /// GPU memory this target occupies, for a cache budget.
    pub fn byte_size(&self) -> usize {
        let per_pixel = self.format.block_copy_size(None).unwrap_or(4) as usize;
        self.size.pixel_count() as usize * per_pixel
    }

    pub fn texture(&self) -> &wgpu::Texture {
        &self.texture
    }

    /// Recreates the target if the requested size differs.
    ///
    /// Returns whether anything was rebuilt, so a caller can re-register the
    /// view with the UI only when it actually changed.
    pub fn resize(&mut self, device: &wgpu::Device, size: Size) -> bool {
        let size = Size::new(size.width.max(1), size.height.max(1));
        if size == self.size {
            return false;
        }
        *self = RenderTarget::with_format(device, size, self.format);
        true
    }

    /// Copies another target's image into this one on the GPU.
    ///
    /// This is how a composite held in the render cache reaches the screen: the
    /// interface draws from one long-lived texture, so a cache hit copies into
    /// that texture rather than re-registering a different one with the UI
    /// every frame. A same-format copy of the whole image is a single blit with
    /// no shader, no pass and no host round trip.
    ///
    /// Returns whether anything was copied. A size or format mismatch is a
    /// caller error — the cache keys a composite on its size, so a hit cannot
    /// be the wrong shape — and is reported rather than silently producing a
    /// half-copied picture.
    pub fn blit_from(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        source: &RenderTarget,
    ) -> bool {
        // A target already holds its own contents, and wgpu rejects a copy
        // between overlapping regions of one texture, so this is a no-op rather
        // than an error.
        if source.texture == self.texture {
            return true;
        }
        if source.size != self.size || source.format != self.format {
            log::error!(
                "refusing to copy a {:?} {:?} target into a {:?} {:?} one",
                source.size,
                source.format,
                self.size,
                self.format
            );
            return false;
        }

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("verge-blit-encoder"),
        });
        encoder.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &source.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d {
                width: self.size.width,
                height: self.size.height,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(Some(encoder.finish()));
        true
    }

    /// Copies the rendered image back to the CPU as tightly packed RGBA.
    ///
    /// Blocking, and deliberately so: the callers are export, which has nothing
    /// else to do, and the tests, which need the pixels to assert on. The
    /// interactive path never reads back — it hands the view straight to the UI.
    pub fn read_pixels(&self, device: &wgpu::Device, queue: &wgpu::Queue) -> Vec<u8> {
        const ALIGN: u32 = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let tight = self.size.width * 4;
        let padded = tight.div_ceil(ALIGN) * ALIGN;

        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("verge-readback"),
            size: (padded * self.size.height) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("verge-readback-encoder"),
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(self.size.height),
                },
            },
            wgpu::Extent3d {
                width: self.size.width,
                height: self.size.height,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(Some(encoder.finish()));

        let slice = buffer.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });
        // The map only resolves once the queue has been polled.
        let _ = device.poll(wgpu::PollType::wait_indefinitely());
        rx.recv().expect("map callback").expect("buffer mapping");

        let mapped = slice.get_mapped_range().expect("mapped range");
        let mut out = Vec::with_capacity((tight * self.size.height) as usize);
        for y in 0..self.size.height {
            let start = (y * padded) as usize;
            out.extend_from_slice(&mapped[start..start + tight as usize]);
        }
        drop(mapped);
        buffer.unmap();
        out
    }

    /// The pixel at `(x, y)` from a readback buffer produced by
    /// [`RenderTarget::read_pixels`].
    pub fn pixel_at(pixels: &[u8], size: Size, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * size.width + x) * 4) as usize;
        [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
    }
}
