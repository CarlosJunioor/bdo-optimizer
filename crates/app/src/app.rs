//! Application shell: the `App` struct, shared per-tab state, and the
//! `eframe::App` update loop that dispatches to the three tabs.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bdo_bench::{Metrics, Session, SessionStore};

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

/// State for the NVIDIA driver-profile section of the Optimize tab.
///
/// The worker handle only exists on Windows (the inspector pipeline is
/// Windows-only); the remaining fields stay cross-platform so the struct's
/// default wiring is identical everywhere.
#[derive(Default)]
pub struct VideoState {
    /// Opt-in Ultra Low Latency (the guide says "test with your system").
    pub ull: bool,
    /// Resolved bundled Profile Inspector path (`None` until resolved / missing).
    pub inspector: Option<PathBuf>,
    /// Whether inspector resolution has been attempted.
    pub inspector_resolved: bool,
    /// In-flight apply/restore job, if any.
    #[cfg(windows)]
    pub worker: Option<crate::video::worker::DriverWorker>,
    /// Outcome of the last finished job (message to show).
    pub last: Option<Result<String, String>>,
    /// The recorded applied-setting ids as they stood before the in-flight
    /// apply added to them, so a failure that never reached the driver can be
    /// rewound. See `video::restore_applied_record`.
    pub applied_before: Vec<u32>,
}

/// State for the game-config-files section of the Optimize tab.
#[derive(Default)]
pub struct GameConfigState {
    /// Which action produced `outcomes` ("apply" or "restore").
    pub last_action: Option<&'static str>,
    /// Per-file results of the last action.
    pub outcomes: Vec<crate::gameconfig::FileOutcome>,
    /// Set when the last click was refused because BDO was running.
    pub blocked: Option<String>,
}

/// Which screen the Apply tab shows: the round button, the progress ring, or
/// the results summary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ApplyPhase {
    #[default]
    Idle,
    Running,
    Done,
}

/// State for the one-click "Apply Performance Optimizer" action.
#[derive(Default)]
pub struct OneClickState {
    /// Which apply screen is showing.
    pub phase: ApplyPhase,
    /// True when the current/last run was an undo rather than an apply.
    pub undoing: bool,
    /// Per-step outcome of the last run, in the order the steps ran.
    pub steps: Vec<(String, Result<String, String>)>,
    /// When the last run started; drives the staggered step-reveal animation.
    pub started: Option<Instant>,
    /// Set while the driver-profile job this run started is still in flight, so
    /// its result can be folded back into `steps` when it finishes.
    pub driver_step: Option<String>,
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
    /// Whether the next run is a normal-launch baseline rather than an affinity run.
    pub baseline_capture: bool,
    /// Saved sessions, newest first.
    pub sessions: Vec<Session>,
    /// Selection flags, parallel to `sessions`.
    pub selected: Vec<bool>,
    /// Metrics per session, parallel to `sessions`, computed once on load.
    ///
    /// `Session::metrics()` clones and sorts the whole frame series, so calling
    /// it from the render loop cost a full sort per session per repaint — with a
    /// few long captures saved that saturates a core while the user is gaming.
    /// Sessions are immutable once written, so this is computed on load only.
    pub metrics: Vec<Option<Metrics>>,
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
        let metrics = session_metrics(&sessions);
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
            baseline_capture: true,
            sessions,
            selected,
            metrics,
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
        self.metrics = session_metrics(&self.sessions);
    }
}

/// Compute metrics for each session once, keeping the result parallel to
/// `sessions` so the render loop can index it instead of re-sorting frame data.
fn session_metrics(sessions: &[Session]) -> Vec<Option<Metrics>> {
    sessions.iter().map(|s| s.metrics().ok()).collect()
}

/// The whole application.
pub struct App {
    pub tab: Tab,
    detect_rx: Receiver<DetectResult>,
    pub detection: Option<DetectResult>,
    pub optimize: OptimizeState,
    pub video: VideoState,
    pub gameconfig: GameConfigState,
    pub oneclick: OneClickState,
    pub benchmark: BenchmarkState,
    #[cfg(windows)]
    pub update: crate::update::UpdateState,
    /// When the app opened; drives the startup splash screen.
    splash_started: Instant,
    /// Lazily-uploaded splash logo texture.
    splash_texture: Option<egui::TextureHandle>,
}

impl App {
    /// Construct the app and kick off background hardware detection.
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        configure_ui(&cc.egui_ctx);
        let detect_rx = detect::spawn(cc.egui_ctx.clone());
        Self {
            tab: Tab::Hardware,
            detect_rx,
            detection: None,
            optimize: OptimizeState::default(),
            video: VideoState::default(),
            gameconfig: GameConfigState::default(),
            oneclick: OneClickState::default(),
            benchmark: BenchmarkState::new(),
            #[cfg(windows)]
            update: crate::update::UpdateState::new(cc.egui_ctx.clone()),
            splash_started: Instant::now(),
            splash_texture: None,
        }
    }

    /// Startup splash: the app logo centered, version bottom-right, fading out
    /// into the app. Returns true while it is showing (the app UI waits).
    fn splash_ui(&mut self, ui: &mut egui::Ui) -> bool {
        const TOTAL: Duration = Duration::from_millis(1800);
        const FADE_SECS: f32 = 0.45;

        let elapsed = self.splash_started.elapsed();
        if elapsed >= TOTAL {
            return false;
        }
        let ctx = ui.ctx().clone();
        let texture = self.logo_texture(&ctx);

        let rect = ctx.content_rect();
        let painter = ui.painter();
        painter.rect_filled(rect, 0.0, crate::theme::BG);
        // Fade the logo and version out over the last FADE_SECS of the splash.
        let remaining = (TOTAL - elapsed).as_secs_f32();
        let opacity = (remaining / FADE_SECS).clamp(0.0, 1.0);
        let side = 230.0;
        painter.image(
            texture.id(),
            egui::Rect::from_center_size(rect.center(), egui::vec2(side, side)),
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE.gamma_multiply(opacity),
        );
        painter.text(
            rect.right_bottom() - egui::vec2(14.0, 10.0),
            egui::Align2::RIGHT_BOTTOM,
            format!("v{}", env!("CARGO_PKG_VERSION")),
            egui::FontId::monospace(12.0),
            crate::theme::INK_3.gamma_multiply(opacity),
        );
        ctx.request_repaint();
        true
    }

    /// The bundled logo as an egui texture, uploaded once and shared by the
    /// splash screen and the enhance-style apply screen.
    pub(crate) fn logo_texture(&mut self, ctx: &egui::Context) -> egui::TextureHandle {
        self.splash_texture
            .get_or_insert_with(|| {
                let img = image::load_from_memory(include_bytes!("../../../assets/icon.png"))
                    .expect("bundled icon.png is valid")
                    .into_rgba8();
                let size = [img.width() as usize, img.height() as usize];
                ctx.load_texture(
                    "splash-logo",
                    egui::ColorImage::from_rgba_unmultiplied(size, &img),
                    Default::default(),
                )
            })
            .clone()
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

        // A deferred viewport only exists for the passes in which its parent
        // *declares* it: egui garbage-collects any viewport that a parent pass
        // does not re-declare, and (unlike an immediate viewport) the deferred
        // child repainting itself re-runs only its own stored callback, never the
        // parent's `drive_overlay`. The main window otherwise repaints only on
        // interaction, so without an explicit wake-up the overlay's first
        // appearance — and its survival — would hinge on incidental repaints
        // (widget animations, the capture-worker poll) and window/driver timing.
        // That is why the overlay could silently fail to appear. Keep the root
        // viewport ticking while the overlay is enabled so `show_viewport_deferred`
        // runs every pass: the overlay then appears promptly and self-heals if it
        // is ever lost. ~4 Hz matches the overlay's own repaint cadence and keeps
        // the idle main window's CPU cost negligible.
        ctx.request_repaint_after(Duration::from_millis(250));

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

    /// Shut a running capture down before the process goes away.
    ///
    /// PresentMon is a separate elevated process holding an ETW trace session,
    /// and `CaptureHandle` deliberately does not kill it on drop — so exiting
    /// mid-capture would leave it running with nothing left to stop it, and
    /// discard the session the user was recording. This gives the worker its
    /// ordinary stop path instead.
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if let Some(worker) = self.benchmark.worker.as_mut() {
            worker.stop_and_join();
        }
        // The driver job spawns Profile Inspector and can be parked waiting on
        // it for up to 90 seconds. Exiting underneath it leaves that child
        // running elevated, still writing the driver profile, with nothing left
        // to verify the result or delete the staging directory.
        #[cfg(windows)]
        if let Some(worker) = self.video.worker.as_mut() {
            worker.stop();
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if self.splash_ui(ui) {
            return;
        }
        self.poll_detection();
        self.poll_capture();
        #[cfg(windows)]
        self.poll_driver_worker();
        #[cfg(windows)]
        self.poll_update();
        self.drive_overlay(&ui.ctx().clone());

        // The GL clear is transparent for the overlay viewport's sake, so any
        // pixel the panels miss (stroke insets, rounded corners) would show the
        // window behind. Paint the whole main window opaque first.
        ui.painter()
            .rect_filled(ui.ctx().content_rect(), 0.0, crate::theme::BG);

        egui::Panel::left("nav")
            .exact_size(232.0)
            .frame(
                egui::Frame::new()
                    .fill(crate::theme::PANEL)
                    .stroke(egui::Stroke::new(1.0, crate::theme::STROKE_SOFT))
                    .inner_margin(egui::Margin::symmetric(12, 20)),
            )
            .show(ui, |ui| {
                self.sidebar(ui);
            });

        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                crate::theme::content_column(ui, |ui| match self.tab {
                    Tab::Hardware => self.hardware_ui(ui),
                    Tab::Optimize => self.optimize_ui(ui),
                    Tab::Benchmark => self.benchmark_ui(ui),
                });
            });
        });
    }
}

impl App {
    fn sidebar(&mut self, ui: &mut egui::Ui) {
        use crate::theme;

        ui.horizontal(|ui| {
            ui.add_space(10.0);
            ui.label(
                egui::RichText::new("BDO")
                    .font(egui::FontId::new(17.0, theme::display()))
                    .color(theme::INK),
            );
            ui.label(
                egui::RichText::new("//")
                    .font(egui::FontId::new(17.0, theme::display()))
                    .color(theme::ACCENT),
            );
            ui.label(
                egui::RichText::new("OPTIMIZER")
                    .font(egui::FontId::new(17.0, theme::display()))
                    .color(theme::INK),
            );
        });
        ui.add_space(16.0);

        let inspect_done = self.detection.is_some();
        let apply_done = matches!(&self.optimize.verify, Some(VerifyOutcome::Match { .. }));
        for (tab, num, label, done) in [
            (Tab::Hardware, "1", "Inspect", inspect_done),
            (Tab::Optimize, "2", "Apply", apply_done),
            (Tab::Benchmark, "3", "Measure", false),
        ] {
            if nav_row(ui, self.tab == tab, num, label, done) {
                self.tab = tab;
            }
        }

        ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
            ui.add_space(2.0);
            egui::Frame::new()
                .inner_margin(egui::Margin::symmetric(10, 0))
                .show(ui, |ui| {
                    ui.label(
                        egui::RichText::new(format!(
                            "v{} · windows-x64",
                            env!("CARGO_PKG_VERSION")
                        ))
                        .monospace()
                        .size(11.0)
                        .color(theme::INK_3),
                    );
                    ui.label(
                        egui::RichText::new(self.gpu_label())
                            .size(12.0)
                            .color(theme::INK_2),
                    );
                    ui.label(
                        egui::RichText::new(self.cpu_label())
                            .size(12.0)
                            .color(theme::INK_2),
                    );
                    ui.add_space(10.0);
                    ui.separator();
                });
            #[cfg(windows)]
            self.update_row(ui);
        });
    }
}

/// One sidebar navigation row: number, label, optional done-check, and the
/// accent bar on the active row. Custom-painted for left alignment.
fn nav_row(ui: &mut egui::Ui, active: bool, num: &str, label: &str, done: bool) -> bool {
    use crate::theme;

    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 38.0), egui::Sense::click());
    let painter = ui.painter();
    if active || response.hovered() {
        painter.rect_filled(rect, egui::CornerRadius::same(9), theme::HOVER);
    }
    if active {
        painter.rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(rect.left() - 12.0, rect.top() + 8.0),
                egui::pos2(rect.left() - 9.0, rect.bottom() - 8.0),
            ),
            egui::CornerRadius::same(2),
            theme::ACCENT,
        );
    }
    let ink = if active { theme::INK } else { theme::INK_2 };
    painter.text(
        egui::pos2(rect.left() + 14.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        num,
        egui::FontId::new(12.0, theme::display()),
        if active { theme::ACCENT } else { theme::INK_3 },
    );
    painter.text(
        egui::pos2(rect.left() + 34.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::new(14.0, egui::FontFamily::Proportional),
        ink,
    );
    if done {
        painter.text(
            egui::pos2(rect.right() - 12.0, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            egui_phosphor::regular::CHECK,
            egui::FontId::new(13.0, egui::FontFamily::Proportional),
            theme::OK,
        );
    }
    response.clicked()
}

fn configure_ui(ctx: &egui::Context) {
    crate::theme::apply(ctx);
}
