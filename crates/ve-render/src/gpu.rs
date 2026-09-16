//! Device acquisition, including a headless path.
//!
//! The editor normally borrows its device from the windowing layer, which
//! already has one for the UI: sharing means a decoded frame is uploaded once
//! and drawn both by the compositor and by the UI, with no cross-device copy.
//! [`GpuContext::headless`] exists so that tests, benchmarks and future
//! command-line export can run the same renderer with no window at all.

use std::sync::Arc;

use crate::RenderError;

/// A device and queue, however they were obtained.
#[derive(Clone)]
pub struct GpuContext {
    pub device: Arc<wgpu::Device>,
    pub queue: Arc<wgpu::Queue>,
    pub adapter_info: wgpu::AdapterInfo,
}

impl GpuContext {
    /// Wraps a device the caller already has, such as the one the UI created.
    pub fn from_parts(
        device: Arc<wgpu::Device>,
        queue: Arc<wgpu::Queue>,
        adapter_info: wgpu::AdapterInfo,
    ) -> Self {
        GpuContext { device, queue, adapter_info }
    }

    /// Creates a device with no surface attached.
    ///
    /// Falls back to any available adapter, including software rasterisers, so
    /// that continuous integration without a GPU still exercises the real
    /// rendering path rather than skipping it.
    pub fn headless() -> Result<Self, RenderError> {
        pollster::block_on(Self::headless_async())
    }

    pub async fn headless_async() -> Result<Self, RenderError> {
        // Reads WGPU_BACKEND and the other wgpu environment overrides, which is
        // how a headless machine pins this to a software rasteriser. No display
        // handle, because nothing here presents to a surface.
        let instance = wgpu::Instance::new(
            wgpu::InstanceDescriptor::new_without_display_handle_from_env(),
        );

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None,
                // A software adapter is far better than no adapter when the
                // alternative is not testing the renderer at all.
                force_fallback_adapter: false,
                ..Default::default()
            })
            .await
            .map_err(|e| RenderError::NoAdapter(e.to_string()))?;

        let adapter_info = adapter.get_info();
        log::info!(
            "using GPU adapter '{}' ({:?}, {:?})",
            adapter_info.name,
            adapter_info.device_type,
            adapter_info.backend
        );

        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("verge-device"),
                // Downlevel defaults keep the renderer runnable on integrated
                // and software adapters; nothing here needs more.
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_defaults()
                    .using_resolution(adapter.limits()),
                memory_hints: wgpu::MemoryHints::Performance,
                ..Default::default()
            })
            .await
            .map_err(|e| RenderError::NoDevice(e.to_string()))?;

        Ok(GpuContext {
            device: Arc::new(device),
            queue: Arc::new(queue),
            adapter_info,
        })
    }

    /// Whether this is a software rasteriser, which the performance overlay
    /// reports so that a slow machine is not mistaken for a slow renderer.
    pub fn is_software(&self) -> bool {
        matches!(self.adapter_info.device_type, wgpu::DeviceType::Cpu)
    }

    pub fn adapter_name(&self) -> &str {
        &self.adapter_info.name
    }
}

impl std::fmt::Debug for GpuContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuContext")
            .field("adapter", &self.adapter_info.name)
            .field("backend", &self.adapter_info.backend)
            .field("device_type", &self.adapter_info.device_type)
            .finish()
    }
}
