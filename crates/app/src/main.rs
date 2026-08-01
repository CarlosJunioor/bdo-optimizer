//! BDO Optimizer — desktop GUI.
//!
//! Detects the host CPU/GPU, recommends and applies the optimal Black Desert
//! Online CPU-affinity mask, and benchmarks FPS locally via bundled PresentMon.
//!
//! Hardware detection and viewing saved benchmarks work on every OS; the
//! Optimize (shortcut/launch) and Benchmark (live capture) actions are
//! Windows-only, with friendly "not supported here" text on other platforms.

// Hide the console window on Windows release builds (keep it in debug for logs).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod benchmark;
mod capture;
mod detect;
mod enhance;
mod format;
mod gameconfig;
mod hardware;
mod optimize;
mod overlay;
mod presentmon;
// Windows-only: the whole point of this module is a mandatory integrity label,
// which has no equivalent elsewhere. Its two callers are Windows-gated too.
#[cfg(windows)]
mod privdir;
#[cfg(windows)]
mod relaunch;
mod theme;
#[cfg(windows)]
mod update;
mod video;
mod winsettings;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 760.0])
            .with_min_inner_size([820.0, 560.0])
            .with_title(format!("BDO Optimizer v{}", env!("CARGO_PKG_VERSION")))
            .with_icon(app_icon()),
        ..Default::default()
    };

    eframe::run_native(
        "BDO Optimizer",
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc)))),
    )
}

/// Window/taskbar icon decoded from the bundled logo. The exe's *file* icon is
/// embedded separately by build.rs; both come from the same source PNG.
fn app_icon() -> egui::IconData {
    let img = image::load_from_memory(include_bytes!("../../../assets/icon.png"))
        .expect("bundled icon.png is valid")
        .into_rgba8();
    let (width, height) = img.dimensions();
    egui::IconData {
        rgba: img.into_raw(),
        width,
        height,
    }
}
