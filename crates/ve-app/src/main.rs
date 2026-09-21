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
