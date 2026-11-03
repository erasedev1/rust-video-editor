//! Verge — a fast, native, open non-linear video editor.

use std::path::PathBuf;

use ve_app::app;

fn main() -> eframe::Result<()> {
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("verge=info,ve_=info,warn"),
    )
    .format_timestamp_millis()
    .init();

    // Reading every font on the machine takes long enough to notice, and the
    // answer is the same whoever asks, so it is worked out on a thread of its
    // own before anyone types a title rather than in front of them when they
    // do. Nothing waits on this: a rasterisation that arrives first simply
    // blocks until the scan is done.
    std::thread::spawn(|| {
        let _ = ve_graphics::FontLibrary::shared();
    });

    load_plugins();

    let open = std::env::args().nth(1).map(PathBuf::from);

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Verge")
            .with_inner_size([1600.0, 950.0])
            .with_min_inner_size([960.0, 600.0]),
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };

    eframe::run_native(
        "Verge",
        options,
        Box::new(|cc| Ok(Box::new(app::VergeApp::new(cc, open)))),
    )
}

/// Where plugins live: `VERGE_PLUGIN_DIR`, else `verge/plugins` under the
/// user's data directory.
fn plugin_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("VERGE_PLUGIN_DIR") {
        return Some(PathBuf::from(dir));
    }
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("APPDATA").map(PathBuf::from))
        .or_else(|| {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("share"))
        })?;
    Some(data.join("verge").join("plugins"))
}

/// Loads plugin manifests into the registry the whole editor resolves effects
/// against. Must run before anything asks for that registry. A broken plugin
/// is logged and skipped; it never stops the editor starting.
fn load_plugins() {
    let mut registry = ve_core::builtin_registry().clone();
    if let Some(dir) = plugin_dir() {
        for (path, outcome) in
            ve_plugin::load_dir_checked(&dir, &mut registry, &ve_render::validate_plugin_shader)
        {
            match outcome {
                Ok(id) => log::info!("plugin {id} loaded from {}", path.display()),
                Err(e) => log::warn!("plugin at {} skipped: {e}", path.display()),
            }
        }
    }
    if ve_core::install_registry(registry).is_err() {
        log::warn!("effect registry was already in use; plugins not installed");
    }
}
