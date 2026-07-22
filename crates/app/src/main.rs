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
mod hardware;
mod optimize;
mod presentmon;
#[cfg(windows)]
mod relaunch;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([920.0, 680.0])
            .with_min_inner_size([720.0, 480.0])
            .with_title("BDO Optimizer"),
        ..Default::default()
    };

    eframe::run_native(
        "BDO Optimizer",
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc)))),
    )
}
