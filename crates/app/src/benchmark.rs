//! Benchmark tab: capture controls + state machine, saved-session table, and a
//! grouped-bar comparison chart.

use std::time::{SystemTime, UNIX_EPOCH};

use egui::{Color32, RichText};
use egui_plot::{Bar, BarChart, Legend, Plot};

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

        self.capture_section(ui);
        ui.add_space(10.0);
        ui.separator();
        self.sessions_section(ui);
        ui.add_space(10.0);
        ui.separator();
        self.comparison_section(ui);
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
                    RichText::new("Benchmarking requires running this app as administrator.")
                        .color(ERR),
                )
                .on_hover_text(
                    "PresentMon captures frames through Windows ETW, which only an elevated \
                     (administrator) process may open. Close the app and relaunch it via \
                     'Run as administrator', then try again.",
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
