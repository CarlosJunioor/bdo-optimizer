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
mod format;
mod gameconfig;
mod hardware;
mod optimize;
mod overlay;
mod presentmon;
#[cfg(windows)]
mod relaunch;
mod video;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 760.0])
            .with_min_inner_size([820.0, 560.0])
            .with_title(format!("BDO Optimizer v{}", env!("CARGO_PKG_VERSION"))),
        ..Default::default()
    };

    eframe::run_native(
        "BDO Optimizer",
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc)))),
    )
}
