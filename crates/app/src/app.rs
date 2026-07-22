//! Application shell: the `App` struct, shared per-tab state, and the
//! `eframe::App` update loop that dispatches to the three tabs.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bdo_bench::{Session, SessionStore};

use crate::capture::{CaptureMsg, CaptureWorker, LiveSnapshot};
use crate::detect::{self, DetectResult};
use crate::overlay::{self, OverlayShared, OverlaySnapshot};

/// Which tab is currently shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Hardware,
    Optimize,
    Benchmark,
}

/// State for the Optimize tab.
///
/// A few fields (the action results) are only touched by the Windows-only
/// shortcut / launch / verify code paths; `allow(dead_code)` keeps non-Windows
/// builds warning-free without splitting the struct per platform.
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Default)]
pub struct OptimizeState {
    /// Detected BDO install directories.
    pub installs: Vec<PathBuf>,
    /// Index into `installs` when several were found.
    pub selected_install: usize,
    /// A launcher path chosen manually via Browse… (overrides `installs`).
    pub manual_launcher: Option<PathBuf>,
    /// The affinity mask being edited (hex).
    pub mask_input: String,
    /// Append `-steam` to the launch.
    pub steam: bool,
    /// Result of the last Create-Shortcut action.
    pub shortcut_result: Option<Result<PathBuf, String>>,
    /// Result of the last Launch-Now action. `Ok` carries how the launch was
    /// performed (direct with a PID, or via the elevated shell); `Err` carries a
    /// [`bdo_launch::LaunchError`] so the UI can distinguish a UAC cancellation
    /// from a real failure.
    pub launch_result: Option<Result<bdo_launch::LaunchMethod, bdo_launch::LaunchError>>,
    /// Latest automatic affinity verification result.
    pub verify: Option<VerifyOutcome>,
    /// Last automatic verification poll.
    pub last_verify_at: Option<Instant>,
    /// Whether `mask_input` has been seeded from the recommendation yet.
    pub seeded: bool,
}

impl OptimizeState {
    /// The effective launcher `.exe` path, if one is known.
    pub fn launcher_path(&self) -> Option<PathBuf> {
        if let Some(m) = &self.manual_launcher {
            return Some(m.clone());
        }
        self.installs
            .get(self.selected_install)
            .map(|dir| dir.join(bdo_launch::LAUNCHER_EXE))
    }
}

/// Outcome of a read-only affinity verification.
///
/// Only constructed/read on Windows (the verify action is Windows-only); the
/// `allow(dead_code)` keeps non-Windows builds warning-free.
#[cfg_attr(not(windows), allow(dead_code))]
pub enum VerifyOutcome {
    NotRunning,
    /// Running mask matches the expected mask.
    Match {
        mask: u64,
    },
    /// Running mask differs from expected.
    Mismatch {
        actual: u64,
        expected: u64,
    },
    /// The expected mask field could not be parsed.
    BadExpected(String),
    Error(String),
}

/// Coarse capture state machine surfaced in the Benchmark tab.
pub enum CaptureStatus {
    Idle,
    Waiting,
    Capturing { elapsed: Duration },
    Saving,
    NeedsElevation,
    Error(String),
    Done { frames: usize },
}

/// State for the Benchmark tab.
pub struct BenchmarkState {
    /// Capture label field.
    pub label: String,
    /// True once the user has hand-edited the label (stops auto-updates).
    pub label_edited: bool,
    /// Saved sessions, newest first.
    pub sessions: Vec<Session>,
    /// Selection flags, parallel to `sessions`.
    pub selected: Vec<bool>,
    /// Directory the session store reads/writes.
    pub store_dir: PathBuf,
    /// Current capture state.
    pub status: CaptureStatus,
    /// The in-flight capture worker, if any.
    pub worker: Option<CaptureWorker>,
    /// Resolved PresentMon path (cached; `None` until first resolved / not found).
    pub presentmon: Option<PathBuf>,
    /// Whether we have attempted PresentMon resolution.
    pub presentmon_resolved: bool,
    /// Latest live stats snapshot for the in-tab live panel (cleared between runs).
    pub live: Option<LiveSnapshot>,
    /// Shared state feeding the always-on-top overlay window.
    pub overlay: Arc<OverlayShared>,
    /// Whether the overlay window is currently shown.
    pub overlay_enabled: bool,
    /// Auto-show the overlay when a capture starts.
    pub overlay_auto: bool,
    /// Whether the app is running elevated (administrator). Checked once at
    /// startup; drives the "run as administrator" warning banner. Always `true`
    /// off Windows (elevation is not a concept there and live capture is
    /// Windows-only anyway).
    pub elevated: bool,
    /// Message from the last failed "Restart as administrator" attempt, if any.
    pub relaunch_error: Option<String>,
}

impl BenchmarkState {
    fn new() -> Self {
        let store_dir = SessionStore::default_store()
            .map(|s| s.dir().to_path_buf())
            .unwrap_or_else(|_| PathBuf::from("sessions"));
        let sessions = SessionStore::new(&store_dir).list().unwrap_or_default();
        let selected = vec![false; sessions.len()];
        // Check elevation once at startup so the Benchmark tab can warn up front
        // that a capture needs administrator rights (PresentMon's ETW session).
        #[cfg(windows)]
        let elevated = bdo_launch::is_elevated();
        #[cfg(not(windows))]
        let elevated = true;
        // Shared overlay state + its background affinity poller (idle until shown).
        let overlay = OverlayShared::new();
        overlay::spawn_affinity_poller(overlay.clone());
        Self {
            label: String::new(),
            label_edited: false,
            sessions,
            selected,
            store_dir,
            status: CaptureStatus::Idle,
            worker: None,
            presentmon: None,
            presentmon_resolved: false,
            elevated,
            relaunch_error: None,
            live: None,
            overlay,
            overlay_enabled: false,
            overlay_auto: true,
        }
    }

    /// Reload the session list from disk, resetting selection.
    pub fn reload(&mut self) {
        self.sessions = SessionStore::new(&self.store_dir)
            .list()
            .unwrap_or_default();
        self.selected = vec![false; self.sessions.len()];
    }
}

/// The whole application.
pub struct App {
    pub tab: Tab,
    detect_rx: Receiver<DetectResult>,
    pub detection: Option<DetectResult>,
    pub optimize: OptimizeState,
    pub benchmark: BenchmarkState,
}

impl App {
    /// Construct the app and kick off background hardware detection.
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let detect_rx = detect::spawn(cc.egui_ctx.clone());
        Self {
            tab: Tab::Hardware,
            detect_rx,
            detection: None,
            optimize: OptimizeState::default(),
            benchmark: BenchmarkState::new(),
        }
    }

    /// Poll the detection channel; when the result lands, seed dependent state.
    fn poll_detection(&mut self) {
        if self.detection.is_some() {
            return;
        }
        if let Ok(result) = self.detect_rx.try_recv() {
            // Seed the optimize tab from the recommendation.
            if !self.optimize.seeded {
                self.optimize.installs = result.installs.clone();
                if let Some(mask) = &result.recommendation.mask_hex {
                    self.optimize.mask_input = mask.clone();
                }
                self.optimize.seeded = true;
            }
            self.detection = Some(result);
        }
    }

    /// Drain capture worker messages into the status machine.
    fn poll_capture(&mut self) {
        let mut msgs = Vec::new();
        if let Some(worker) = &self.benchmark.worker {
            while let Ok(m) = worker.rx.try_recv() {
                msgs.push(m);
            }
        }
        for m in msgs {
            match m {
                CaptureMsg::Waiting => self.benchmark.status = CaptureStatus::Waiting,
                CaptureMsg::Capturing { elapsed } => {
                    self.benchmark.status = CaptureStatus::Capturing { elapsed }
                }
                CaptureMsg::Live(snap) => {
                    self.set_overlay_snapshot(&snap, true);
                    self.benchmark.live = Some(snap);
                }
                CaptureMsg::Saving => self.benchmark.status = CaptureStatus::Saving,
                CaptureMsg::Saved { frames } => {
                    self.benchmark.status = CaptureStatus::Done { frames };
                    self.benchmark.worker = None;
                    self.mark_overlay_inactive();
                    self.benchmark.reload();
                }
                CaptureMsg::NeedsElevation => {
                    self.benchmark.status = CaptureStatus::NeedsElevation;
                    self.benchmark.worker = None;
                    self.mark_overlay_inactive();
                }
                CaptureMsg::Error(e) => {
                    self.benchmark.status = CaptureStatus::Error(e);
                    self.benchmark.worker = None;
                    self.mark_overlay_inactive();
                }
            }
        }
    }

    /// Mirror a live snapshot into the overlay's shared state.
    fn set_overlay_snapshot(&self, snap: &LiveSnapshot, active: bool) {
        if let Ok(mut g) = self.benchmark.overlay.snapshot.lock() {
            *g = OverlaySnapshot {
                active,
                current_fps: snap.current_fps,
                avg_fps: snap.avg_fps,
                p1_low_fps: snap.p1_low_fps,
                one_percent_low_integral_fps: snap.one_percent_low_integral_fps,
                frames: snap.frames,
                elapsed_secs: snap.elapsed.as_secs(),
            };
        }
    }

    /// Flag the overlay stats as no longer live (capture ended); last numbers stay.
    fn mark_overlay_inactive(&self) {
        if let Ok(mut g) = self.benchmark.overlay.snapshot.lock() {
            g.active = false;
        }
    }

    /// Show / hide the always-on-top overlay viewport and honor its self-close.
    fn drive_overlay(&mut self, ctx: &egui::Context) {
        // The overlay's own close button asks the app to untick "Show overlay".
        if self
            .benchmark
            .overlay
            .close_clicked
            .swap(false, Ordering::SeqCst)
        {
            self.benchmark.overlay_enabled = false;
        }
        let enabled = self.benchmark.overlay_enabled;
        // Gate the background affinity poller on visibility.
        self.benchmark
            .overlay
            .visible
            .store(enabled, Ordering::SeqCst);
        if !enabled {
            return;
        }
        let shared = self.benchmark.overlay.clone();
        ctx.show_viewport_deferred(
            overlay::viewport_id(),
            overlay::builder(),
            move |ui, _class| {
                overlay::render(ui, &shared);
            },
        );
    }

    /// CPU model for stamping into sessions (falls back to a placeholder).
    pub fn cpu_label(&self) -> String {
        self.detection
            .as_ref()
            .map(|d| d.cpu.model.clone())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "Unknown CPU".to_string())
    }

    /// Primary GPU model for stamping into sessions.
    pub fn gpu_label(&self) -> String {
        self.detection
            .as_ref()
            .and_then(|d| d.gpus.first())
            .map(|g| g.name.clone())
            .unwrap_or_else(|| "Unknown GPU".to_string())
    }
}

impl eframe::App for App {
    /// Transparent GL clear color so the always-on-top overlay viewport (which is
    /// created transparent) shows the game through its un-painted areas. The main
    /// window is opaque and fully covered by its panels, so this does not affect it.
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.poll_detection();
        self.poll_capture();
        self.drive_overlay(&ui.ctx().clone());

        egui::Panel::top("tabs").show(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.heading("BDO Optimizer");
                ui.separator();
                ui.selectable_value(&mut self.tab, Tab::Hardware, "Hardware");
                ui.selectable_value(&mut self.tab, Tab::Optimize, "Optimize");
                ui.selectable_value(&mut self.tab, Tab::Benchmark, "Benchmark");
            });
            ui.add_space(4.0);
        });

        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| match self.tab {
                Tab::Hardware => self.hardware_ui(ui),
                Tab::Optimize => self.optimize_ui(ui),
                Tab::Benchmark => self.benchmark_ui(ui),
            });
        });
    }
}
