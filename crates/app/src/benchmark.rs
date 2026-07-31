//! Benchmark tab: capture controls + state machine, saved-session table, and a
//! grouped-bar comparison chart.

use std::time::{SystemTime, UNIX_EPOCH};

use egui::{Color32, RichText};
use egui_plot::{Bar, BarChart, Legend, Line, Plot};

use bdo_bench::{
    gain_verdict, GainVerdict, Metrics, SessionRole, SessionStore, MIN_MEANINGFUL_DELTA_FPS,
    MIN_MEANINGFUL_DELTA_PERCENT,
};

use crate::app::{App, CaptureStatus, VerifyOutcome};
use crate::capture::{CaptureParams, CaptureWorker};
use crate::{format, presentmon};

const OK_GREEN: Color32 = crate::theme::OK;
const WARN: Color32 = crate::theme::WARN;
const ERR: Color32 = crate::theme::ERR;

const COL_AVG: Color32 = crate::theme::CHART_1;
const COL_P1: Color32 = crate::theme::CHART_2;
const COL_INTEGRAL: Color32 = crate::theme::CHART_3;

impl App {
    pub(crate) fn benchmark_ui(&mut self, ui: &mut egui::Ui) {
        use egui_phosphor::regular as icons;

        crate::theme::screen_heading(
            ui,
            "Measure",
            "Capture one normal-launch baseline and one verified affinity run, then compare \
             them below.",
        );

        self.poll_verification(ui.ctx());
        self.elevation_banner(ui);
        crate::theme::section_card(ui, icons::RECORD, "Capture a run", |ui| {
            self.capture_section(ui)
        });
        crate::theme::section_card(ui, icons::ROWS, "Choose two runs", |ui| {
            self.sessions_section(ui)
        });
        crate::theme::section_card(
            ui,
            icons::CHART_BAR,
            "Compare baseline vs optimized",
            |ui| self.comparison_section(ui),
        );
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
                    // `mask` is raw user text, which may already carry a 0x
                    // prefix; and the run is saved with the *observed* running
                    // mask, not with what was typed here.
                    format!(
                        "Launch from the optimized shortcut. Selected mask {}; the run is saved \
                         with the mask actually observed on the running game.",
                        mask_text(Some(mask.trim_start_matches("0x").trim_start_matches("0X")))
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

        let logical_cores = self
            .detection
            .as_ref()
            .map(|detection| detection.cpu.logical_cores)
            .unwrap_or(0);
        let observed_mask = running_mask(self.optimize.verify.as_ref());
        let expected_mask = if self.benchmark.baseline_capture {
            full_affinity_mask(logical_cores)
        } else {
            bdo_launch::parse_mask_hex(&self.optimize.mask_input).ok()
        };
        let Ok(affinity) = capture_affinity(
            self.benchmark.baseline_capture,
            expected_mask,
            observed_mask,
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
            role: affinity.role,
            expected_affinity_mask: affinity.expected,
            observed_affinity_mask: affinity.observed,
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
                        "Compare",
                        "Run",
                        "Role / affinity",
                        "Average",
                        "P1 low",
                        "1% low",
                        "Frames",
                        "",
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
                        // Precomputed on load — recomputing here would re-sort the
                        // whole frame series for every session, every repaint.
                        let metrics = self.benchmark.metrics.get(i).cloned().flatten();
                        let trusted = session.role != SessionRole::Unknown
                            && session.affinity_verified();
                        if let Some(selected) = self.benchmark.selected.get_mut(i) {
                            ui.add_enabled(
                                metrics.is_some() && trusted && (*selected || picked < 2),
                                egui::Checkbox::new(selected, "Pick"),
                            )
                            .on_disabled_hover_text(
                                "Legacy or unverified sessions cannot be used for a trusted comparison.",
                            );
                        } else {
                            ui.label("");
                        }
                        ui.vertical(|ui| {
                            ui.label(RichText::new(&session.label).strong());
                            ui.label(RichText::new(&session.timestamp).size(11.0).weak());
                        });
                        ui.vertical(|ui| {
                            ui.label(
                                RichText::new(match session.role {
                                    SessionRole::Baseline => "Baseline",
                                    SessionRole::Optimized => "Optimized",
                                    SessionRole::Unknown => "Legacy / untrusted",
                                })
                                .strong()
                                .color(if trusted { COL_AVG } else { WARN }),
                            );
                            ui.label(
                                RichText::new(format!(
                                    "expected {} · observed {}",
                                    mask_text(session.expected_affinity_mask.as_deref()),
                                    mask_text(session.observed_affinity_mask.as_deref()),
                                ))
                                .monospace()
                                .size(11.0)
                                .weak(),
                            );
                        });

                        if let Some(metrics) = metrics {
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
            RichText::new("The delta is B minus A. Higher FPS is better.")
                .size(12.0)
                .weak(),
        );

        // Same precomputed metrics as the table above — never recomputed per frame.
        let picked: Vec<(_, Metrics)> = self
            .benchmark
            .sessions
            .iter()
            .zip(self.benchmark.selected.iter())
            .zip(self.benchmark.metrics.iter())
            .filter(|((_, sel), _)| **sel)
            .filter_map(|((session, _), metrics)| metrics.clone().map(|metrics| (session, metrics)))
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

        let Some((baseline_index, optimized_index)) =
            comparison_order(picked[0].0.role, picked[1].0.role)
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
        let (session_a, metrics_a) = &picked[baseline_index];
        let (session_b, metrics_b) = &picked[optimized_index];
        if !session_a.affinity_verified() || !session_b.affinity_verified() {
            ui.label(
                RichText::new(
                    "Both runs need matching expected and observed affinity before comparison.",
                )
                .italics()
                .color(WARN),
            );
            return;
        }
        let mask =
            |session: &bdo_bench::Session| mask_text(session.observed_affinity_mask.as_deref());

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

        let different_hardware = session_a.cpu != session_b.cpu || session_a.gpu != session_b.gpu;
        if different_hardware {
            ui.label(
                RichText::new("⚠ These runs were recorded on different hardware; the result is not a controlled mask comparison.")
                    .color(WARN),
            );
        }
        // Compare parsed masks, not raw text: "0xFFF" and "fff" are the same mask.
        let parsed_mask = |session: &bdo_bench::Session| {
            session
                .observed_affinity_mask
                .as_deref()
                .and_then(|mask| bdo_launch::parse_mask_hex(mask).ok())
        };
        let same_affinity = match (parsed_mask(session_a), parsed_mask(session_b)) {
            (Some(a), Some(b)) => a == b,
            _ => session_a.observed_affinity_mask == session_b.observed_affinity_mask,
        };
        if same_affinity {
            ui.label(
                RichText::new("⚠ These runs use the same affinity mask; choose two different masks for an affinity A/B test.")
                    .color(WARN),
            );
        }
        let duration_mismatch =
            durations_mismatched(metrics_a.duration_seconds, metrics_b.duration_seconds);
        if duration_mismatch {
            ui.label(
                RichText::new(format!(
                    "⚠ These runs are {:.0}s and {:.0}s long — more than {MAX_DURATION_RATIO:.0}× apart. \
                     Re-run the shorter one over the same route and duration; this delta reflects \
                     different workloads, not the mask.",
                    metrics_a.duration_seconds, metrics_b.duration_seconds
                ))
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
        ui.label(
            RichText::new(format!(
                "A delta must be at least {MIN_MEANINGFUL_DELTA_FPS:.0} FPS and {MIN_MEANINGFUL_DELTA_PERCENT:.0}% to be treated as conclusive."
            ))
            .size(12.0)
            .weak(),
        );

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
                            let verdict = gain_verdict(
                                a,
                                b,
                                comparison_is_inconclusive(
                                    metrics_a.low_confidence || metrics_b.low_confidence,
                                    different_hardware,
                                    same_affinity,
                                    duration_mismatch,
                                ),
                            );
                            let percent = percent
                                .map(|value| format!(" · {value:+.1}%"))
                                .unwrap_or_default();
                            let (color, qualifier) = match verdict {
                                GainVerdict::Improvement => (OK_GREEN, ""),
                                GainVerdict::Regression => (ERR, ""),
                                GainVerdict::Inconclusive => {
                                    (ui.visuals().weak_text_color(), " · inconclusive")
                                }
                            };
                            ui.label(label);
                            ui.label(RichText::new(format::fps(a)).monospace().size(16.0));
                            ui.label(RichText::new(format::fps(b)).monospace().size(16.0));
                            ui.label(
                                RichText::new(format!("{delta:+.1}{percent}{qualifier}"))
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

fn comparison_order(first: SessionRole, second: SessionRole) -> Option<(usize, usize)> {
    match (first, second) {
        (SessionRole::Baseline, SessionRole::Optimized) => Some((0, 1)),
        (SessionRole::Optimized, SessionRole::Baseline) => Some((1, 0)),
        _ => None,
    }
}

/// Largest ratio between two runs' durations still treated as a fair comparison.
///
/// Frame-pacing metrics are only comparable across runs of similar length: a
/// short run samples one scene, a long one averages over many. Two runs whose
/// durations differ by more than this factor are compared on different workloads,
/// not on different affinity masks.
const MAX_DURATION_RATIO: f64 = 2.0;

/// Whether two run durations are too far apart to compare (see
/// [`MAX_DURATION_RATIO`]). Non-finite or non-positive durations count as
/// mismatched, since nothing meaningful can be said about them.
fn durations_mismatched(a_seconds: f64, b_seconds: f64) -> bool {
    if !a_seconds.is_finite() || !b_seconds.is_finite() || a_seconds <= 0.0 || b_seconds <= 0.0 {
        return true;
    }
    let (short, long) = if a_seconds < b_seconds {
        (a_seconds, b_seconds)
    } else {
        (b_seconds, a_seconds)
    };
    long / short > MAX_DURATION_RATIO
}

fn comparison_is_inconclusive(
    low_confidence: bool,
    different_hardware: bool,
    same_affinity: bool,
    duration_mismatch: bool,
) -> bool {
    low_confidence || different_hardware || same_affinity || duration_mismatch
}

fn mask_text(mask: Option<&str>) -> String {
    mask.map(|mask| format!("0x{mask}"))
        .unwrap_or_else(|| "unknown".to_string())
}

fn running_mask(outcome: Option<&VerifyOutcome>) -> Option<u64> {
    match outcome {
        Some(VerifyOutcome::Match { mask }) => Some(*mask),
        Some(VerifyOutcome::Mismatch { actual, .. }) => Some(*actual),
        _ => None,
    }
}

fn is_full_affinity(mask: Option<u64>, logical_cores: usize) -> bool {
    // Both sides must be known. Comparing the Options directly made
    // `is_full_affinity(None, 0)` true — no running game and no detected CPU
    // read as "full affinity detected", which is the opposite of the truth.
    match (mask, full_affinity_mask(logical_cores)) {
        (Some(mask), Some(full)) => mask == full,
        _ => false,
    }
}

fn full_affinity_mask(logical_cores: usize) -> Option<u64> {
    match logical_cores {
        0 => None,
        1..=63 => Some((1_u64 << logical_cores) - 1),
        _ => Some(u64::MAX),
    }
}

struct CaptureAffinity {
    role: SessionRole,
    expected: String,
    observed: String,
}

fn capture_affinity(
    baseline: bool,
    expected: Option<u64>,
    observed: Option<u64>,
) -> Result<CaptureAffinity, ()> {
    let (Some(expected), Some(observed)) = (expected, observed) else {
        return Err(());
    };
    if expected != observed {
        return Err(());
    }
    Ok(CaptureAffinity {
        role: if baseline {
            SessionRole::Baseline
        } else {
            SessionRole::Optimized
        },
        expected: format!("{expected:x}"),
        observed: format!("{observed:x}"),
    })
}

#[cfg(test)]
mod tests {
    use super::{
        capture_affinity, comparison_delta, comparison_is_inconclusive, comparison_order,
        durations_mismatched, is_full_affinity,
    };
    use bdo_bench::SessionRole;

    #[test]
    fn comparison_delta_is_b_minus_a_and_handles_zero_baseline() {
        assert_eq!(comparison_delta(100.0, 110.0), (10.0, Some(10.0)));
        assert_eq!(comparison_delta(0.0, 10.0), (10.0, None));
    }

    #[test]
    fn comparison_always_orders_baseline_before_optimized() {
        assert_eq!(
            comparison_order(SessionRole::Baseline, SessionRole::Optimized),
            Some((0, 1))
        );
        assert_eq!(
            comparison_order(SessionRole::Optimized, SessionRole::Baseline),
            Some((1, 0))
        );
        assert_eq!(
            comparison_order(SessionRole::Baseline, SessionRole::Baseline),
            None
        );
    }

    #[test]
    fn same_affinity_comparison_is_inconclusive() {
        assert!(comparison_is_inconclusive(false, false, true, false));
        assert!(!comparison_is_inconclusive(false, false, false, false));
    }

    #[test]
    fn full_affinity_needs_both_sides_known() {
        // Nothing running and no CPU detected yet must not read as "full affinity".
        assert!(!is_full_affinity(None, 0));
        assert!(!is_full_affinity(None, 16));
        assert!(!is_full_affinity(Some(0xffff), 0));
        // The real case still works.
        assert!(is_full_affinity(Some(0xffff), 16));
        assert!(!is_full_affinity(Some(0x5554), 16));
    }

    #[test]
    fn mismatched_run_lengths_are_inconclusive() {
        // A 25s baseline against a 30min run is a workload difference, not a
        // mask difference — every other gate passes, so this one must not.
        assert!(durations_mismatched(25.0, 1800.0));
        assert!(comparison_is_inconclusive(false, false, false, true));
        // Equal-ish runs stay comparable, in both orderings.
        assert!(!durations_mismatched(300.0, 320.0));
        assert!(!durations_mismatched(320.0, 300.0));
        // Exactly at the ratio is still allowed; past it is not.
        assert!(!durations_mismatched(100.0, 200.0));
        assert!(durations_mismatched(100.0, 201.0));
        // Degenerate durations cannot be compared.
        assert!(durations_mismatched(0.0, 100.0));
        assert!(durations_mismatched(f64::NAN, 100.0));
    }

    #[test]
    fn capture_affinity_requires_matching_role_provenance() {
        let baseline = capture_affinity(true, Some(0xfff), Some(0xfff)).unwrap();
        assert_eq!(baseline.role, SessionRole::Baseline);
        assert_eq!(baseline.expected, "fff");
        assert_eq!(baseline.observed, "fff");

        let optimized = capture_affinity(false, Some(0x555), Some(0x555)).unwrap();
        assert_eq!(optimized.role, SessionRole::Optimized);
        assert_eq!(optimized.expected, "555");
        assert_eq!(optimized.observed, "555");

        assert!(capture_affinity(false, Some(0x555), Some(0x554)).is_err());
        assert!(capture_affinity(true, Some(0xfff), None).is_err());
        assert!(is_full_affinity(Some(0xfff), 12));
        assert!(!is_full_affinity(Some(0x555), 12));
    }
}
