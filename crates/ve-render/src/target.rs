//! Offscreen render targets.

use ve_core::Size;

use crate::texture::FRAME_FORMAT;

/// A texture the compositor draws into.
///
/// The preview, each nested composition and every exported frame all render
/// into one of these rather than straight to a window, which is what lets the
/// same code path serve preview, export and the test suite.
pub struct RenderTarget {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
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
            // export and the tests can read it back.
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        RenderTarget { texture, view, size, format }
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
