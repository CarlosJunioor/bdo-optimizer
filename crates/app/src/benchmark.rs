//! Benchmark tab: capture controls + state machine, saved-session table, and a
//! grouped-bar comparison chart.

use std::time::{SystemTime, UNIX_EPOCH};

use egui::{Color32, RichText};
use egui_plot::{Bar, BarChart, Legend, Line, Plot};

use bdo_bench::{Metrics, SessionStore};

use crate::app::{App, CaptureStatus, VerifyOutcome};
use crate::capture::{CaptureParams, CaptureWorker};
use crate::{format, presentmon};

const OK_GREEN: Color32 = Color32::from_rgb(0x63, 0xd6, 0x88);
const WARN: Color32 = Color32::from_rgb(0xff, 0xc1, 0x07);
const ERR: Color32 = Color32::from_rgb(0xff, 0x6b, 0x6b);

const COL_AVG: Color32 = Color32::from_rgb(0x4d, 0xa6, 0xff);
const COL_P1: Color32 = Color32::from_rgb(0xff, 0xa6, 0x4d);
const COL_INTEGRAL: Color32 = Color32::from_rgb(0x9d, 0x7d, 0xff);

impl App {
    pub(crate) fn benchmark_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("Measure the gain");
        ui.label(
            RichText::new("Capture one normal-launch baseline and one verified affinity run, then compare them below.")
                .size(13.0)
                .weak(),
        );

        self.poll_verification(ui.ctx());
        self.elevation_banner(ui);
        self.capture_section(ui);
        ui.add_space(10.0);
        ui.separator();
        self.sessions_section(ui);
        ui.add_space(10.0);
        ui.separator();
        self.comparison_section(ui);
    }

    // ------------------------------------------------ Elevation warning banner
    /// Prominent banner shown at the top of the Benchmark tab when the app is not
    /// running as administrator. Benchmarking needs admin rights for PresentMon's
    /// ETW trace session, so we warn up front and offer a one-click elevated
    /// relaunch rather than letting the user hit a capture error first.
    fn elevation_banner(&mut self, ui: &mut egui::Ui) {
        if self.benchmark.elevated {
            return;
        }

        // Windows-only: relaunching elevated is a Windows concept, and `elevated`
        // is always true off Windows, so this body only ever runs there.
        #[cfg(windows)]
        {
            let frame = egui::Frame::new()
                .fill(Color32::from_rgb(0x3a, 0x2e, 0x12))
                .stroke(egui::Stroke::new(1.0, WARN))
                .inner_margin(egui::Margin::same(10))
                .corner_radius(6);

            frame.show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("⚠").color(WARN).size(20.0));
                    ui.label(
                        RichText::new("Benchmarking requires administrator rights")
                            .color(WARN)
                            .strong()
                            .size(15.0),
                    );
                });
                ui.label(
                    "PresentMon captures frames through Windows event tracing (ETW), which only \
                     an elevated process may start. Restart the app as administrator before \
                     capturing.",
                );
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if ui
                        .button(RichText::new("🛡 Restart as administrator").strong())
                        .on_hover_text(
                            "Relaunches this app with a UAC prompt, then closes this window.",
                        )
                        .clicked()
                    {
                        self.benchmark.relaunch_error = None;
                        match crate::relaunch::relaunch_as_admin() {
                            crate::relaunch::RelaunchOutcome::Launched => {
                                // Elevated instance is starting; close this one.
                                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                            }
                            crate::relaunch::RelaunchOutcome::Cancelled => {
                                // User dismissed UAC — stay open, show nothing scary.
                            }
                            crate::relaunch::RelaunchOutcome::Failed(e) => {
                                self.benchmark.relaunch_error = Some(e);
                            }
                        }
                    }
                    if let Some(err) = &self.benchmark.relaunch_error {
                        ui.label(
                            RichText::new(format!("Could not relaunch: {err}"))
                                .color(ERR)
                                .size(12.0),
                        );
                    }
                });
            });
            ui.add_space(8.0);
        }
    }

    // ----------------------------------------------------------- Capture panel
    fn capture_section(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Capture a run").strong().size(20.0));
        let capturing = self.benchmark.worker.is_some();
        let previous_mode = self.benchmark.baseline_capture;
        ui.add_enabled_ui(!capturing, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(
                    &mut self.benchmark.baseline_capture,
                    true,
                    "1  Baseline / normal launch",
                );
                ui.selectable_value(
                    &mut self.benchmark.baseline_capture,
                    false,
                    "2  Optimized / affinity launch",
                );
            });
        });
        if previous_mode != self.benchmark.baseline_capture {
            self.benchmark.label_edited = false;
        }

        // Keep the label auto-synced to the capture mode until the user edits it.
        if !self.benchmark.label_edited {
            let mask = self.optimize.mask_input.trim();
            self.benchmark.label = if self.benchmark.baseline_capture {
                "baseline".to_string()
            } else if mask.is_empty() {
                "capture".to_string()
            } else {
                format!("mask {mask}")
            };
        }

        let logical_cores = self
            .detection
            .as_ref()
            .map(|detection| detection.cpu.logical_cores)
            .unwrap_or(0);
        let running_mask = running_mask(self.optimize.verify.as_ref());
        let baseline_ready = is_full_affinity(running_mask, logical_cores);

        if self.benchmark.baseline_capture {
            if baseline_ready {
                ui.label(
                    RichText::new("Full CPU affinity detected. This normal-launch run is ready to capture as the rollback baseline.")
                        .size(12.0)
                        .color(OK_GREEN),
                );
            } else if let Some(mask) = running_mask {
                ui.label(
                    RichText::new(format!("Restricted running mask 0x{mask:x} detected. Close BDO and relaunch normally before capturing the baseline."))
                        .size(12.0)
                        .color(WARN),
                );
            } else {
                ui.label(
                    RichText::new("Launch BDO normally first. Capture unlocks after full CPU affinity is verified read-only.")
                        .size(12.0)
                        .weak(),
                );
            }
        } else {
            let mask = self.optimize.mask_input.trim();
            ui.label(
                RichText::new(if mask.is_empty() {
                    "Choose an affinity mask on Apply before capturing the optimized run."
                        .to_string()
                } else {
                    format!(
                        "Launch from the optimized shortcut. This run is saved as mask 0x{mask}."
                    )
                })
                .size(12.0)
                .color(if mask.is_empty() {
                    WARN
                } else {
                    ui.visuals().weak_text_color()
                }),
            );
            match &self.optimize.verify {
                Some(VerifyOutcome::Match { mask }) => {
                    ui.label(
                        RichText::new(format!("Verified running mask: 0x{mask:x}"))
                            .color(OK_GREEN)
                            .strong(),
                    );
                }
                _ => {
                    ui.label(
                        RichText::new("Pending verification — launch through Apply and confirm the running mask before measuring.")
                            .color(WARN),
                    );
                }
            }
        }

        ui.horizontal(|ui| {
            ui.label("Label:");
            let resp = ui.add_enabled(
                !capturing,
                egui::TextEdit::singleline(&mut self.benchmark.label).desired_width(200.0),
            );
            if resp.changed() {
                self.benchmark.label_edited = true;
            }
        });

        // Resolve PresentMon once and cache the outcome.
        if !self.benchmark.presentmon_resolved {
            self.benchmark.presentmon = presentmon::resolve();
            self.benchmark.presentmon_resolved = true;
        }

        ui.horizontal(|ui| {
            if !cfg!(windows) {
                ui.label(
                    RichText::new("Live capture is Windows-only (BDO runs only on Windows).")
                        .color(WARN),
                );
                return;
            }

            let verified = matches!(&self.optimize.verify, Some(VerifyOutcome::Match { .. }));
            let mode_ready = if self.benchmark.baseline_capture {
                baseline_ready
            } else {
                verified
            };
            let can_start = !capturing && self.benchmark.presentmon.is_some() && mode_ready;
            let start_label = if self.benchmark.baseline_capture {
                "Start baseline capture"
            } else {
                "Start optimized capture"
            };
            if ui
                .add_enabled(can_start, egui::Button::new(start_label))
                .on_hover_text("Arms PresentMon; capture begins when BlackDesert64.exe appears.")
                .clicked()
            {
                self.start_capture(ui.ctx().clone());
            }
            if ui
                .add_enabled(capturing, egui::Button::new("■ Stop"))
                .clicked()
            {
                if let Some(worker) = &self.benchmark.worker {
                    worker.request_stop();
                }
            }
        });

        if self.benchmark.presentmon.is_none() {
            let [a, b] = presentmon::expected_locations();
            ui.label(RichText::new("PresentMon.exe not found.").color(ERR));
            ui.label(RichText::new(format!("Expected: {a}")).size(12.0).weak());
            ui.label(RichText::new(format!("      or: {b}")).size(12.0).weak());
        }

        ui.add_space(6.0);
        self.status_line(ui);

        ui.add_space(6.0);
        self.overlay_controls(ui);
        self.live_panel(ui);
    }

    // ----------------------------------------------------------- Overlay toggle
    /// "Show overlay" / "Auto-show" checkboxes for the always-on-top FPS window.
    fn overlay_controls(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.benchmark.overlay_enabled, "Show overlay")
                .on_hover_text(
                    "Floats a small always-on-top FPS window over the game. It works in \
                     windowed / borderless-windowed mode (BDO's usual mode); it cannot appear \
                     over exclusive fullscreen. It is NOT injected into the game — it is a \
                     separate window this app draws, so it stays anti-cheat safe.",
                );
            ui.checkbox(&mut self.benchmark.overlay_auto, "Auto-show on capture")
                .on_hover_text("Automatically show the overlay when a capture starts.");
        });
    }

    // ------------------------------------------------------------- Live panel
    /// Live-updating stats + frame-time sparkline shown while a capture runs.
    fn live_panel(&self, ui: &mut egui::Ui) {
        let Some(live) = &self.benchmark.live else {
            return;
        };

        ui.add_space(8.0);
        let frame = egui::Frame::new()
            .fill(Color32::from_rgb(0x1a, 0x1c, 0x22))
            .stroke(egui::Stroke::new(1.0, Color32::from_rgb(0x33, 0x37, 0x42)))
            .corner_radius(6)
            .inner_margin(egui::Margin::same(10));

        frame.show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Live").strong().size(15.0).color(OK_GREEN));
                ui.label(
                    RichText::new(format!(
                        "{} · {} frames",
                        format::duration(live.elapsed),
                        live.frames
                    ))
                    .size(12.0)
                    .weak(),
                );
            });
            ui.add_space(4.0);

            egui::Grid::new("live_stats")
                .num_columns(4)
                .spacing([22.0, 2.0])
                .show(ui, |ui| {
                    for h in ["Current", "Avg", "P1 low", "1% integral"] {
                        ui.label(RichText::new(h).size(12.0).weak());
                    }
                    ui.end_row();
                    ui.label(
                        RichText::new(format::fps(live.current_fps))
                            .size(20.0)
                            .strong()
                            .color(OK_GREEN),
                    );
                    ui.label(RichText::new(format::fps(live.avg_fps)).size(20.0));
                    ui.label(
                        RichText::new(format::fps(live.p1_low_fps))
                            .size(20.0)
                            .color(COL_P1),
                    );
                    ui.label(
                        RichText::new(format::fps(live.one_percent_low_integral_fps))
                            .size(20.0)
                            .color(COL_INTEGRAL),
                    );
                    ui.end_row();
                });

            ui.add_space(6.0);
            if live.sparkline.len() >= 2 {
                let points: Vec<[f64; 2]> = live
                    .sparkline
                    .iter()
                    .enumerate()
                    .map(|(i, &ms)| [i as f64, ms])
                    .collect();
                Plot::new("live_sparkline")
                    .height(90.0)
                    .allow_scroll(false)
                    .allow_drag(false)
                    .allow_zoom(false)
                    .show_x(false)
                    .show_axes([false, true])
                    .show(ui, |plot_ui| {
                        plot_ui.line(Line::new("frame ms", points).color(COL_AVG));
                    });
                ui.label(
                    RichText::new(
                        "Frame time (ms), last ~10 s — lower is better; spikes = stutter.",
                    )
                    .size(11.0)
                    .weak(),
                );
            } else {
                ui.label(RichText::new("Collecting frames…").size(12.0).weak());
            }
        });
    }

    fn status_line(&self, ui: &mut egui::Ui) {
        match &self.benchmark.status {
            CaptureStatus::Idle => {
                ui.label(RichText::new("State: Idle").weak());
            }
            CaptureStatus::Waiting => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Waiting for the game to start…");
                });
            }
            CaptureStatus::Capturing { elapsed } => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(
                        RichText::new(format!("Capturing — {}", format::duration(*elapsed)))
                            .color(OK_GREEN),
                    );
                });
            }
            CaptureStatus::Saving => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Saving session…");
                });
            }
            CaptureStatus::Done { frames } => {
                ui.label(
                    RichText::new(format!("✔ Saved session ({frames} frames).")).color(OK_GREEN),
                );
            }
            CaptureStatus::NeedsElevation => {
                ui.label(
                    RichText::new(
                        "Benchmarking needs administrator rights for Windows event tracing — \
                         use Restart as administrator above.",
                    )
                    .color(ERR),
                )
                .on_hover_text(
                    "PresentMon captures frames through Windows ETW, which only an elevated \
                     (administrator) process may open. Use the 'Restart as administrator' \
                     button above, then try again.",
                );
            }
            CaptureStatus::Error(e) => {
                ui.label(RichText::new(format!("Error: {e}")).color(ERR));
            }
        }
    }

    fn start_capture(&mut self, ctx: egui::Context) {
        let Some(presentmon) = self.benchmark.presentmon.clone() else {
            return;
        };
        self.refresh_verification();
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let output_csv = std::env::temp_dir().join(format!("bdo_capture_{stamp}.csv"));

        let verified_mask = match &self.optimize.verify {
            Some(VerifyOutcome::Match { mask }) => Some(*mask),
            _ => None,
        };
        let logical_cores = self
            .detection
            .as_ref()
            .map(|detection| detection.cpu.logical_cores)
            .unwrap_or(0);
        let baseline_ready =
            is_full_affinity(running_mask(self.optimize.verify.as_ref()), logical_cores);
        let Ok(mask_hex) = capture_mask(
            self.benchmark.baseline_capture,
            baseline_ready,
            verified_mask,
        ) else {
            self.benchmark.status = CaptureStatus::Error(
                "Verify a normal full-affinity baseline or the selected optimized mask before capturing."
                    .into(),
            );
            return;
        };

        let params = CaptureParams {
            presentmon_path: presentmon,
            output_csv,
            label: self.benchmark.label.clone(),
            mask_hex,
            cpu: self.cpu_label(),
            gpu: self.gpu_label(),
            store_dir: self.benchmark.store_dir.clone(),
        };

        // Fresh run: clear any stale live stats and reset the overlay snapshot.
        self.benchmark.live = None;
        if let Ok(mut g) = self.benchmark.overlay.snapshot.lock() {
            *g = Default::default();
        }
        if self.benchmark.overlay_auto {
            self.benchmark.overlay_enabled = true;
        }

        self.benchmark.status = CaptureStatus::Waiting;
        self.benchmark.worker = Some(CaptureWorker::start(params, ctx));
    }

    // ------------------------------------------------------------ Session table
    fn sessions_section(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Choose two runs").strong().size(20.0));
            ui.label(
                RichText::new(format!(
                    "{} / 2 selected",
                    self.benchmark
                        .selected
                        .iter()
                        .filter(|selected| **selected)
                        .count()
                ))
                .monospace()
                .color(COL_AVG),
            );
            if ui.button("Refresh").clicked() {
                self.benchmark.reload();
            }
            if ui.button("Clear").clicked() {
                self.benchmark.selected.fill(false);
            }
        });
        ui.label(
            RichText::new("Select exactly one Baseline and one optimized mask run.")
                .size(12.0)
                .weak(),
        );

        if self.benchmark.sessions.is_empty() {
            ui.add_space(4.0);
            ui.label(
                RichText::new("No sessions yet — capture one above.")
                    .italics()
                    .weak(),
            );
            return;
        }

        let mut delete_stem: Option<String> = None;

        egui::ScrollArea::horizontal().show(ui, |ui| {
            egui::Grid::new("sessions_grid")
                .num_columns(8)
                .striped(true)
                .spacing([18.0, 8.0])
                .show(ui, |ui| {
                    for heading in [
                        "Compare", "Run", "Mask", "Average", "P1 low", "1% low", "Frames", "",
                    ] {
                        ui.label(RichText::new(heading).strong());
                    }
                    ui.end_row();

                    let picked = self
                        .benchmark
                        .selected
                        .iter()
                        .filter(|selected| **selected)
                        .count();
                    for (i, session) in self.benchmark.sessions.iter().enumerate() {
                        let metrics = session.metrics();
                        if let Some(selected) = self.benchmark.selected.get_mut(i) {
                            ui.add_enabled(
                                metrics.is_ok() && (*selected || picked < 2),
                                egui::Checkbox::new(selected, "Pick"),
                            );
                        } else {
                            ui.label("");
                        }
                        ui.vertical(|ui| {
                            ui.label(RichText::new(&session.label).strong());
                            ui.label(RichText::new(&session.timestamp).size(11.0).weak());
                        });
                        ui.label(
                            RichText::new(
                                session
                                    .affinity_mask
                                    .as_deref()
                                    .map(|mask| format!("0x{mask}"))
                                    .unwrap_or_else(|| "Baseline".to_string()),
                            )
                            .monospace()
                            .color(COL_AVG),
                        );

                        if let Ok(metrics) = metrics {
                            ui.label(format::fps(metrics.avg_fps));
                            ui.label(format::fps(metrics.p1_low_fps));
                            ui.label(format::fps(metrics.one_percent_low_integral_fps));
                            ui.label(
                                RichText::new(if metrics.low_confidence {
                                    format!("{} ⚠", metrics.frame_count)
                                } else {
                                    metrics.frame_count.to_string()
                                })
                                .color(
                                    if metrics.low_confidence {
                                        WARN
                                    } else {
                                        ui.visuals().text_color()
                                    },
                                ),
                            )
                            .on_hover_text(format!(
                                "{} · {}",
                                format::duration_secs(metrics.duration_seconds),
                                if metrics.low_confidence {
                                    "short sample; low metrics may be noisy"
                                } else {
                                    "good sample size"
                                }
                            ));
                        } else {
                            for _ in 0..4 {
                                ui.label("—");
                            }
                        }

                        if ui.button("Delete").clicked() {
                            delete_stem = Some(session.file_stem());
                        }
                        ui.end_row();
                    }
                });
        });

        ui.label(
            RichText::new(format!("Stored in {}", self.benchmark.store_dir.display()))
                .size(11.0)
                .weak(),
        );

        if let Some(stem) = delete_stem {
            let _ = SessionStore::new(&self.benchmark.store_dir).delete(&stem);
            self.benchmark.reload();
        }
    }

    // -------------------------------------------------------- Comparison chart
    fn comparison_section(&mut self, ui: &mut egui::Ui) {
        ui.label(
            RichText::new("Compare baseline vs optimized")
                .strong()
                .size(24.0),
        );
        ui.label(
            RichText::new("The delta is B minus A. Higher FPS is better.")
                .size(12.0)
                .weak(),
        );

        let picked: Vec<(_, Metrics)> = self
            .benchmark
            .sessions
            .iter()
            .zip(self.benchmark.selected.iter())
            .filter(|(_, sel)| **sel)
            .filter_map(|(session, _)| session.metrics().ok().map(|metrics| (session, metrics)))
            .collect();

        if picked.len() < 2 {
            ui.label(
                RichText::new(format!(
                    "Choose {} more run{} above to compare.",
                    2 - picked.len(),
                    if picked.is_empty() { "s" } else { "" }
                ))
                .italics()
                .weak(),
            );
            return;
        }

        let baseline = picked
            .iter()
            .find(|(session, _)| session.affinity_mask.is_none());
        let optimized = picked
            .iter()
            .find(|(session, _)| session.affinity_mask.is_some());
        let (Some((session_a, metrics_a)), Some((session_b, metrics_b))) = (baseline, optimized)
        else {
            ui.label(
                RichText::new(
                    "Choose one Baseline and one optimized mask run to calculate a valid gain.",
                )
                .italics()
                .color(WARN),
            );
            return;
        };
        let mask = |session: &bdo_bench::Session| {
            session
                .affinity_mask
                .as_deref()
                .map(|value| format!("0x{value}"))
                .unwrap_or_else(|| "Baseline".to_string())
        };

        ui.add_space(10.0);
        ui.columns(2, |columns| {
            for (column, (marker, session, color)) in columns
                .iter_mut()
                .zip([("A", *session_a, COL_AVG), ("B", *session_b, COL_P1)])
            {
                egui::Frame::new()
                    .fill(Color32::from_rgb(16, 25, 39))
                    .stroke(egui::Stroke::new(1.0, color))
                    .corner_radius(8)
                    .inner_margin(egui::Margin::same(14))
                    .show(column, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(marker).monospace().strong().color(color));
                            ui.label(RichText::new(mask(session)).monospace().strong().size(20.0));
                        });
                        ui.label(RichText::new(&session.label).strong());
                        ui.label(RichText::new(&session.timestamp).size(11.0).weak());
                    });
            }
        });

        if session_a.cpu != session_b.cpu || session_a.gpu != session_b.gpu {
            ui.label(
                RichText::new("⚠ These runs were recorded on different hardware; the result is not a controlled mask comparison.")
                    .color(WARN),
            );
        }
        if session_a.affinity_mask == session_b.affinity_mask {
            ui.label(
                RichText::new("⚠ These runs use the same affinity mask; choose two different masks for an affinity A/B test.")
                    .color(WARN),
            );
        }
        if metrics_a.low_confidence || metrics_b.low_confidence {
            ui.label(
                RichText::new(
                    "⚠ At least one run has fewer than 1,000 frames; low-FPS deltas may be noisy.",
                )
                .color(WARN),
            );
        }

        ui.add_space(10.0);
        egui::Frame::new()
            .fill(Color32::from_rgb(14, 22, 34))
            .stroke(egui::Stroke::new(1.0, Color32::from_rgb(42, 59, 79)))
            .corner_radius(8)
            .inner_margin(egui::Margin::same(14))
            .show(ui, |ui| {
                egui::Grid::new("comparison_delta_grid")
                    .num_columns(4)
                    .striped(true)
                    .spacing([30.0, 10.0])
                    .show(ui, |ui| {
                        ui.label(RichText::new("Metric").strong());
                        ui.label(RichText::new("A").monospace().strong().color(COL_AVG));
                        ui.label(RichText::new("B").monospace().strong().color(COL_P1));
                        ui.label(RichText::new("Delta (B − A)").strong());
                        ui.end_row();

                        for (label, a, b) in [
                            ("Average FPS", metrics_a.avg_fps, metrics_b.avg_fps),
                            ("P1 low", metrics_a.p1_low_fps, metrics_b.p1_low_fps),
                            (
                                "1% low · time weighted",
                                metrics_a.one_percent_low_integral_fps,
                                metrics_b.one_percent_low_integral_fps,
                            ),
                            (
                                "0.1% low · time weighted",
                                metrics_a.zero_1_percent_low_integral_fps,
                                metrics_b.zero_1_percent_low_integral_fps,
                            ),
                        ] {
                            let (delta, percent) = comparison_delta(a, b);
                            let percent = percent
                                .map(|value| format!(" · {value:+.1}%"))
                                .unwrap_or_default();
                            let color = if delta > 0.05 {
                                OK_GREEN
                            } else if delta < -0.05 {
                                ERR
                            } else {
                                ui.visuals().weak_text_color()
                            };
                            ui.label(label);
                            ui.label(RichText::new(format::fps(a)).monospace().size(16.0));
                            ui.label(RichText::new(format::fps(b)).monospace().size(16.0));
                            ui.label(
                                RichText::new(format!("{delta:+.1}{percent}"))
                                    .monospace()
                                    .strong()
                                    .color(color),
                            );
                            ui.end_row();
                        }
                    });
            });

        let values_a = [
            metrics_a.avg_fps,
            metrics_a.p1_low_fps,
            metrics_a.one_percent_low_integral_fps,
        ];
        let values_b = [
            metrics_b.avg_fps,
            metrics_b.p1_low_fps,
            metrics_b.one_percent_low_integral_fps,
        ];
        let bars = |values: [f64; 3], offset: f64, prefix: &str| {
            values
                .into_iter()
                .enumerate()
                .map(|(index, value)| {
                    Bar::new(index as f64 * 3.0 + offset, value)
                        .width(0.85)
                        .name(format!("{prefix}: {value:.1}"))
                })
                .collect()
        };

        Plot::new("cmp_plot")
            .legend(Legend::default())
            .height(220.0)
            .allow_scroll(false)
            .allow_drag(false)
            .allow_zoom(false)
            .show_x(false)
            .show(ui, |plot_ui| {
                plot_ui.bar_chart(BarChart::new("A", bars(values_a, 0.0, "A")).color(COL_AVG));
                plot_ui.bar_chart(BarChart::new("B", bars(values_b, 1.0, "B")).color(COL_P1));
            });

        ui.horizontal_wrapped(|ui| {
            for label in ["1  Average FPS", "2  P1 low", "3  1% low · time weighted"] {
                ui.label(RichText::new(label).monospace().size(12.0));
            }
        });

        ui.add_space(4.0);
        ui.label(
            RichText::new(
                "Use equal-length runs in the same scene. Repeat each mask, then compare representative runs before trusting a small delta.",
            )
            .size(12.0)
            .weak(),
        );
    }
}

fn comparison_delta(a: f64, b: f64) -> (f64, Option<f64>) {
    let delta = b - a;
    (delta, (a.abs() > f64::EPSILON).then_some(delta / a * 100.0))
}

fn running_mask(outcome: Option<&VerifyOutcome>) -> Option<u64> {
    match outcome {
        Some(VerifyOutcome::Match { mask }) => Some(*mask),
        Some(VerifyOutcome::Mismatch { actual, .. }) => Some(*actual),
        _ => None,
    }
}

fn is_full_affinity(mask: Option<u64>, logical_cores: usize) -> bool {
    let full_mask = match logical_cores {
        0 => return false,
        1..=63 => (1_u64 << logical_cores) - 1,
        _ => u64::MAX,
    };
    mask == Some(full_mask)
}

fn capture_mask(
    baseline: bool,
    baseline_ready: bool,
    verified_mask: Option<u64>,
) -> Result<Option<String>, ()> {
    if baseline {
        baseline_ready.then_some(None).ok_or(())
    } else {
        verified_mask
            .map(|mask| Some(format!("{mask:x}")))
            .ok_or(())
    }
}

#[cfg(test)]
mod tests {
    use super::{capture_mask, comparison_delta, is_full_affinity};

    #[test]
    fn comparison_delta_is_b_minus_a_and_handles_zero_baseline() {
        assert_eq!(comparison_delta(100.0, 110.0), (10.0, Some(10.0)));
        assert_eq!(comparison_delta(0.0, 10.0), (10.0, None));
    }

    #[test]
    fn baseline_never_stamps_an_affinity_mask() {
        assert_eq!(capture_mask(true, true, Some(0x555)), Ok(None));
        assert_eq!(capture_mask(true, false, None), Err(()));
        assert_eq!(
            capture_mask(false, false, Some(0x555)),
            Ok(Some("555".to_string()))
        );
        assert_eq!(capture_mask(false, false, None), Err(()));
        assert!(is_full_affinity(Some(0xfff), 12));
        assert!(!is_full_affinity(Some(0x555), 12));
    }
}
