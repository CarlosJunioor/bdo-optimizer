//! Application shell: the `App` struct, shared per-tab state, and the
//! `eframe::App` update loop that dispatches to the three tabs.

use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use bdo_bench::{Session, SessionStore};

use crate::capture::{CaptureMsg, CaptureWorker};
use crate::detect::{self, DetectResult};

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
    /// Alternate masks worth trying (from the recommendation).
    pub alternates: Vec<String>,
    /// Append `-steam` to the launch.
    pub steam: bool,
    /// Result of the last Create-Shortcut action.
    pub shortcut_result: Option<Result<PathBuf, String>>,
    /// Result of the last Launch-Now action. `Ok` carries how the launch was
    /// performed (direct with a PID, or via the elevated shell); `Err` carries a
    /// [`bdo_launch::LaunchError`] so the UI can distinguish a UAC cancellation
    /// from a real failure.
    pub launch_result: Option<Result<bdo_launch::LaunchMethod, bdo_launch::LaunchError>>,
    /// Result of the last Verify action.
    pub verify: Option<VerifyOutcome>,
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
}

impl BenchmarkState {
    fn new() -> Self {
        let store_dir = SessionStore::default_store()
            .map(|s| s.dir().to_path_buf())
            .unwrap_or_else(|_| PathBuf::from("sessions"));
        let sessions = SessionStore::new(&store_dir).list().unwrap_or_default();
        let selected = vec![false; sessions.len()];
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
                self.optimize.alternates = result.recommendation.alternates.clone();
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
                CaptureMsg::Saving => self.benchmark.status = CaptureStatus::Saving,
                CaptureMsg::Saved { frames } => {
                    self.benchmark.status = CaptureStatus::Done { frames };
                    self.benchmark.worker = None;
                    self.benchmark.reload();
                }
                CaptureMsg::NeedsElevation => {
                    self.benchmark.status = CaptureStatus::NeedsElevation;
                    self.benchmark.worker = None;
                }
                CaptureMsg::Error(e) => {
                    self.benchmark.status = CaptureStatus::Error(e);
                    self.benchmark.worker = None;
                }
            }
        }
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
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.poll_detection();
        self.poll_capture();

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
