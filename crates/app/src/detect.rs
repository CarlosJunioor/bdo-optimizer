//! Background hardware detection.
//!
//! wgpu adapter enumeration plus a sysinfo refresh can take a noticeable moment,
//! so all of it runs once on a dedicated thread at startup. The result is
//! delivered over an `mpsc` channel; the worker calls
//! [`egui::Context::request_repaint`] so the UI wakes up the instant detection
//! finishes instead of waiting for the next input event.

use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver};

use bdo_hw::{detect_cpu, detect_gpus, recommend, CpuInfo, GpuInfo, Recommendation};

/// Everything discovered about the host at startup.
pub struct DetectResult {
    pub cpu: CpuInfo,
    pub gpus: Vec<GpuInfo>,
    pub recommendation: Recommendation,
    /// BDO install directories (Windows only; empty elsewhere).
    pub installs: Vec<PathBuf>,
}

/// Spawn detection on a background thread, returning the receiver the UI polls.
pub fn spawn(ctx: egui::Context) -> Receiver<DetectResult> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let cpu = detect_cpu();
        let gpus = detect_gpus();
        let recommendation = recommend(&cpu);
        let installs = find_installs();
        // If the receiver is gone the app is closing; ignore the send error.
        let _ = tx.send(DetectResult {
            cpu,
            gpus,
            recommendation,
            installs,
        });
        ctx.request_repaint();
    });
    rx
}

#[cfg(windows)]
fn find_installs() -> Vec<PathBuf> {
    bdo_launch::find_bdo_install()
}

#[cfg(not(windows))]
fn find_installs() -> Vec<PathBuf> {
    Vec::new()
}
