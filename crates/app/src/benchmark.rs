//! Benchmark tab: capture controls + state machine, saved-session table, and a
//! grouped-bar comparison chart.

use std::time::{SystemTime, UNIX_EPOCH};

use egui::{Color32, RichText};
use egui_plot::{Bar, BarChart, Legend, Line, Plot};

use bdo_bench::{Metrics, SessionStore};

use crate::app::{App, CaptureStatus};
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
        ui.heading("Benchmark");

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
        // Keep the label auto-synced to the current mask until the user edits it.
        if !self.benchmark.label_edited {
            let mask = self.optimize.mask_input.trim();
            self.benchmark.label = if mask.is_empty() {
                "capture".to_string()
            } else {
                format!("mask {mask}")
            };
        }

        let capturing = self.benchmark.worker.is_some();

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

            let can_start = !capturing && self.benchmark.presentmon.is_some();
            if ui
                .add_enabled(can_start, egui::Button::new("▶ Start"))
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
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let output_csv = std::env::temp_dir().join(format!("bdo_capture_{stamp}.csv"));

        let mask = self.optimize.mask_input.trim();
        let mask_hex = if mask.is_empty() {
            None
        } else {
            Some(mask.to_string())
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
            ui.label(RichText::new("Saved sessions").strong().size(16.0));
            if ui.small_button("⟳ Refresh").clicked() {
                self.benchmark.reload();
            }
        });
        ui.label(
            RichText::new(format!("Stored in: {}", self.benchmark.store_dir.display()))
                .size(11.0)
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

        egui::Grid::new("sessions_grid")
            .num_columns(10)
            .striped(true)
            .spacing([12.0, 4.0])
            .show(ui, |ui| {
                for h in [
                    "Cmp",
                    "Timestamp",
                    "Label",
                    "Mask",
                    "Frames",
                    "Duration",
                    "Avg",
                    "P1 low",
                    "1% integral",
                    "",
                ] {
                    ui.label(RichText::new(h).strong());
                }
                ui.end_row();

                for (i, session) in self.benchmark.sessions.iter().enumerate() {
                    if let Some(sel) = self.benchmark.selected.get_mut(i) {
                        ui.checkbox(sel, "");
                    } else {
                        ui.label("");
                    }
                    ui.label(RichText::new(&session.timestamp).size(12.0));
                    ui.label(&session.label);
                    ui.label(
                        session
                            .affinity_mask
                            .as_deref()
                            .map(|m| format!("0x{m}"))
                            .unwrap_or_else(|| "-".to_string()),
                    );

                    match session.metrics() {
                        Ok(m) => {
                            let frames = if m.low_confidence {
                                RichText::new(format!("{} ⚠", m.frame_count)).color(WARN)
                            } else {
                                RichText::new(m.frame_count.to_string())
                            };
                            ui.label(frames).on_hover_text(if m.low_confidence {
                                "Fewer than 1000 frames — tail (1% low) metrics are noisy."
                            } else {
                                "Frame count"
                            });
                            ui.label(format::duration_secs(m.duration_seconds));
                            ui.label(format::fps(m.avg_fps));
                            ui.label(format::fps(m.p1_low_fps));
                            ui.label(format::fps(m.one_percent_low_integral_fps));
                        }
                        Err(_) => {
                            for _ in 0..5 {
                                ui.label("-");
                            }
                        }
                    }

                    if ui
                        .small_button("🗑")
                        .on_hover_text("Delete this session")
                        .clicked()
                    {
                        delete_stem = Some(session.file_stem());
                    }
                    ui.end_row();
                }
            });

        if let Some(stem) = delete_stem {
            let _ = SessionStore::new(&self.benchmark.store_dir).delete(&stem);
            self.benchmark.reload();
        }
    }

    // -------------------------------------------------------- Comparison chart
    fn comparison_section(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Comparison").strong().size(16.0));

        // Gather selected sessions with computable metrics.
        let picked: Vec<(String, Metrics)> = self
            .benchmark
            .sessions
            .iter()
            .zip(self.benchmark.selected.iter())
            .filter(|(_, sel)| **sel)
            .filter_map(|(s, _)| s.metrics().ok().map(|m| (s.label.clone(), m)))
            .collect();

        if picked.is_empty() {
            ui.label(
                RichText::new("Tick the Cmp box on two or more sessions to compare them.")
                    .italics()
                    .weak(),
            );
            return;
        }

        let group_span = 4.0; // 3 bars + a gap per session group.
        let mk = |offset: f64, name: &str, color: Color32, pick: &dyn Fn(&Metrics) -> f64| {
            let bars: Vec<Bar> = picked
                .iter()
                .enumerate()
                .map(|(i, (label, m))| {
                    Bar::new(i as f64 * group_span + offset, pick(m))
                        .width(0.9)
                        .name(format!("{label}: {:.1}", pick(m)))
                })
                .collect();
            BarChart::new(name.to_string(), bars).color(color)
        };

        let avg = mk(0.0, "Avg FPS", COL_AVG, &|m| m.avg_fps);
        let p1 = mk(1.0, "P1 low", COL_P1, &|m| m.p1_low_fps);
        let integral = mk(2.0, "1% low integral", COL_INTEGRAL, &|m| {
            m.one_percent_low_integral_fps
        });

        Plot::new("cmp_plot")
            .legend(Legend::default())
            .height(240.0)
            .allow_scroll(false)
            .allow_drag(false)
            .allow_zoom(false)
            .show_x(false)
            .show(ui, |plot_ui| {
                plot_ui.bar_chart(avg);
                plot_ui.bar_chart(p1);
                plot_ui.bar_chart(integral);
            });

        // Group index → session label key (x axis is numeric).
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Groups:").size(12.0).strong());
            for (i, (label, _)) in picked.iter().enumerate() {
                ui.label(RichText::new(format!("{}) {label}", i + 1)).size(12.0));
            }
        });

        ui.add_space(4.0);
        ui.label(
            RichText::new("P1 low = 1000 / the 99th-percentile frame time (rank-based 1% low).")
                .size(11.0)
                .weak(),
        );
        ui.label(
            RichText::new(
                "1% low integral = time-weighted (CapFrameX): the slowest frames whose durations \
                 sum to 1% of the session — most sensitive to stutter.",
            )
            .size(11.0)
            .weak(),
        );
    }
}
