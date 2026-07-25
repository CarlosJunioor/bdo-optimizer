//! Always-on-top FPS overlay — an **external** window rendered by our own process.
//!
//! # Why this is anti-cheat safe
//!
//! This overlay is *not* injected into the game and does *not* hook its graphics
//! API. It is a separate egui **deferred viewport** — a second native window our
//! process owns — flagged always-on-top, borderless and transparent. Because BDO
//! usually runs windowed/borderless, this window simply floats above it. It never
//! opens a handle into `BlackDesert64.exe`; the only game interaction anywhere is
//! the read-only [`bdo_launch::read_process_affinity`] call used for the affinity
//! readout.
//!
//! **Limitation:** an always-on-top window can only draw over the game in
//! windowed / borderless-windowed mode. In *exclusive* fullscreen the GPU hands
//! the display to the game and no external window can appear over it.
//!
//! # Threading
//!
//! The deferred-viewport render callback runs on the UI thread, so it must read
//! shared state cheaply. [`OverlayShared`] holds mutex-guarded snapshots that are
//! quick to lock-copy: the live capture stats (fed from the capture channel) and
//! the game affinity (refreshed on a background thread every ~2 s).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use egui::{Color32, FontId, Margin, Sense, Stroke, ViewportBuilder, ViewportCommand};

/// The affinity mask currently applied to the running game, if any.
#[derive(Debug, Clone, Copy, Default)]
pub enum AffinityStatus {
    /// Not polled yet, or the read failed.
    #[default]
    Unknown,
    /// The game process was not found running.
    NotRunning,
    /// The game is running with this affinity mask (`cores` = set-bit count).
    Running { mask: u64, cores: u32 },
}

/// Cheap-to-copy live stats mirrored into the overlay from the capture channel.
#[derive(Debug, Clone, Copy, Default)]
pub struct OverlaySnapshot {
    /// True while a capture is actively producing frames.
    pub active: bool,
    /// Smoothed current FPS.
    pub current_fps: f64,
    /// Session average FPS so far.
    pub avg_fps: f64,
    /// 1% low (percentile) so far.
    pub p1_low_fps: f64,
    /// 1% low integral (CapFrameX) so far.
    pub one_percent_low_integral_fps: f64,
    /// Frames captured so far.
    pub frames: usize,
    /// Elapsed capture seconds.
    pub elapsed_secs: u64,
}

/// State shared between the UI thread (which renders the overlay viewport), the
/// capture message pump (which writes [`OverlaySnapshot`]), and the affinity
/// poller thread (which writes [`AffinityStatus`]).
pub struct OverlayShared {
    /// Latest live capture stats.
    pub snapshot: Mutex<OverlaySnapshot>,
    /// Latest game affinity readout.
    pub affinity: Mutex<AffinityStatus>,
    /// Whether the overlay is currently meant to be shown (drives affinity polling).
    pub visible: AtomicBool,
    /// Set when the user clicks the overlay's own close button; the app reads and
    /// clears this to untick the "Show overlay" checkbox.
    pub close_clicked: AtomicBool,
}

impl OverlayShared {
    /// Create shared state (hidden, empty) wrapped in an [`Arc`].
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            snapshot: Mutex::new(OverlaySnapshot::default()),
            affinity: Mutex::new(AffinityStatus::Unknown),
            visible: AtomicBool::new(false),
            close_clicked: AtomicBool::new(false),
        })
    }
}

/// Read the running game's affinity (read-only, anti-cheat safe). Windows-only;
/// elsewhere the game does not run, so this reports [`AffinityStatus::Unknown`].
#[cfg(windows)]
fn read_affinity() -> AffinityStatus {
    match bdo_launch::read_process_affinity(crate::capture::GAME_EXE) {
        Ok(Some(mask)) => AffinityStatus::Running {
            mask,
            cores: mask.count_ones(),
        },
        Ok(None) => AffinityStatus::NotRunning,
        Err(_) => AffinityStatus::Unknown,
    }
}

#[cfg(not(windows))]
fn read_affinity() -> AffinityStatus {
    AffinityStatus::Unknown
}

/// Spawn the background affinity poller. It refreshes the shared affinity every
/// ~2 s **only while the overlay is visible**, so a hidden overlay costs nothing.
/// The thread lives for the process lifetime (it ends when the process exits).
pub fn spawn_affinity_poller(shared: Arc<OverlayShared>) {
    std::thread::spawn(move || loop {
        if shared.visible.load(Ordering::Relaxed) {
            let status = read_affinity();
            if let Ok(mut g) = shared.affinity.lock() {
                *g = status;
            }
        }
        std::thread::sleep(Duration::from_secs(2));
    });
}

/// The deterministic viewport id for the overlay window.
pub fn viewport_id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("bdo_fps_overlay")
}

/// The `ViewportBuilder` for the overlay window: small, borderless, transparent,
/// always-on-top, kept out of the taskbar, positioned near the top-left.
pub fn builder() -> ViewportBuilder {
    ViewportBuilder::default()
        .with_title("BDO Overlay")
        .with_inner_size([260.0, 150.0])
        .with_min_inner_size([260.0, 150.0])
        .with_position([24.0, 24.0])
        .with_decorations(false)
        .with_transparent(true)
        .with_resizable(false)
        .with_taskbar(false)
        .with_always_on_top()
}

/// Render the overlay's contents into its viewport `Ui`.
///
/// Reads [`OverlayShared`] (cheap lock-copies), draws a semi-transparent rounded
/// panel with the live numbers and the game affinity, supports window dragging by
/// the panel background, and a close button. Self-schedules a repaint so it keeps
/// updating even when the main window is idle.
pub fn render(ui: &mut egui::Ui, shared: &OverlayShared) {
    let snap = shared.snapshot.lock().map(|g| *g).unwrap_or_default();
    let affinity = shared
        .affinity
        .lock()
        .map(|g| *g)
        .unwrap_or(AffinityStatus::Unknown);

    let panel = egui::Frame::new()
        .fill(Color32::from_rgba_unmultiplied(16, 17, 22, 232))
        .stroke(Stroke::new(
            1.0,
            Color32::from_rgba_unmultiplied(90, 96, 120, 200),
        ))
        .corner_radius(10)
        .inner_margin(Margin::same(12));

    // Drag the whole window by its background (borderless window has no titlebar).
    // Interact over the full panel rect; a press-drag starts an OS window move.
    //
    // This MUST be registered before the panel contents. egui resolves a hit-test
    // tie in favour of the last-registered widget, so with this after the panel
    // the full-rect sensor swallowed clicks on the ✕ button underneath it and the
    // close button never fired. Sensing drag only (not click) keeps clicks flowing
    // to the widgets registered above it.
    let full = ui.max_rect();
    let bg = ui.interact(full, ui.id().with("overlay_drag"), Sense::drag());
    if bg.drag_started() {
        ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
    }

    panel.show(ui, |ui| {
        ui.set_width(ui.available_width());
        draw_contents(ui, shared, &snap, affinity);
    });

    // Keep the overlay ticking (~4 Hz) independent of the parent window.
    ui.ctx().request_repaint_after(Duration::from_millis(250));
}

fn draw_contents(
    ui: &mut egui::Ui,
    shared: &OverlayShared,
    snap: &OverlaySnapshot,
    affinity: AffinityStatus,
) {
    let dim = Color32::from_rgb(0x9a, 0xa0, 0xb4);
    let bright = Color32::from_rgb(0xf0, 0xf2, 0xf8);
    let accent = Color32::from_rgb(0x63, 0xd6, 0x88);

    // Header row: title + close button.
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("BDO FPS")
                .color(dim)
                .size(11.0)
                .strong(),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .add(egui::Button::new(egui::RichText::new("✕").size(12.0)).frame(false))
                .on_hover_text("Hide overlay")
                .clicked()
            {
                shared.close_clicked.store(true, Ordering::SeqCst);
                ui.ctx().send_viewport_cmd(ViewportCommand::Close);
            }
        });
    });

    // Big current-FPS number.
    let fps_text = if snap.current_fps > 0.0 {
        format!("{:.0}", snap.current_fps)
    } else {
        "--".to_string()
    };
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(fps_text)
                .color(if snap.active { accent } else { bright })
                .font(FontId::proportional(38.0)),
        );
        ui.label(egui::RichText::new("FPS").color(dim).size(12.0));
    });

    // Small stat rows.
    stat_row(ui, "avg", snap.avg_fps, dim, bright);
    stat_row(ui, "1% low", snap.p1_low_fps, dim, bright);
    stat_row(ui, "1% int", snap.one_percent_low_integral_fps, dim, bright);

    ui.add_space(2.0);
    ui.label(
        egui::RichText::new(format!(
            "{} frames · {}",
            snap.frames,
            crate::format::duration(Duration::from_secs(snap.elapsed_secs))
        ))
        .color(dim)
        .size(11.0),
    );

    // Affinity readout.
    let (aff_text, aff_color) = match affinity {
        AffinityStatus::Running { mask, cores } => (
            format!("{:x} ({} cores)", mask, cores),
            Color32::from_rgb(0x8f, 0xc8, 0xff),
        ),
        AffinityStatus::NotRunning => ("game not running".to_string(), dim),
        AffinityStatus::Unknown => ("affinity —".to_string(), dim),
    };
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("aff").color(dim).size(11.0));
        ui.label(
            egui::RichText::new(aff_text)
                .color(aff_color)
                .size(12.0)
                .strong(),
        );
    });
}

/// One right-aligned label/value stat row.
fn stat_row(ui: &mut egui::Ui, label: &str, value: f64, dim: Color32, bright: Color32) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(label).color(dim).size(12.0));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let text = if value.is_finite() && value > 0.0 {
                format!("{value:.0}")
            } else {
                "--".to_string()
            };
            ui.label(egui::RichText::new(text).color(bright).size(13.0));
        });
    });
}
