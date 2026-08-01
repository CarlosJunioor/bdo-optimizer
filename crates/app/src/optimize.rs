//! Optimize tab: a one-button apply flow (round button → BDO-enhance-styled
//! progress screen in `enhance.rs` → results screen), with the individual
//! sections kept under an Advanced collapsible for fixing blockers and
//! fine-tuning.

use egui::{Color32, RichText};

use crate::app::{App, ApplyPhase, Tab, VerifyOutcome};
use crate::format;

const OK_GREEN: Color32 = crate::theme::OK;
const WARN: Color32 = crate::theme::WARN;
const ERR: Color32 = crate::theme::ERR;

const MASK_PRESETS: &[(&str, &str)] = &[
    ("C", "AMD 4-core without SMT"),
    ("50", "AMD 4-core with SMT"),
    ("540", "AMD 6-core Zen/Zen+"),
    ("5500", "AMD 8-core Zen/Zen+"),
    ("554", "Ryzen 9 7900X3D alternate"),
    ("555", "AMD 6-core / 6-core CCD"),
    ("5550", "AMD 8-core alternate"),
    ("5554", "AMD 8-core single CCD"),
    ("5555", "Ryzen 9 7950X3D V-Cache CCD"),
    ("555000", "Ryzen 9 12-core second CCD"),
    ("5550000", "Ryzen 9 16-core second CCD"),
    ("AA", "Intel 4-core with Hyper-Threading"),
    ("AAA", "Intel 6-core with Hyper-Threading"),
    ("AAA0", "Intel 8+ core with Hyper-Threading"),
    ("FC", "Intel 8-core without Hyper-Threading"),
];

fn available_masks(
    recommendation: &bdo_hw::Recommendation,
    logical_cores: usize,
) -> Vec<(String, String, bool)> {
    let mut rows = Vec::new();
    let mut add = |mask: &str, profile: &str, recommended: bool| {
        let valid = bdo_launch::parse_mask_hex(mask)
            .and_then(|value| bdo_launch::validate_mask(value, Some(logical_cores)))
            .is_ok();
        if valid
            && !rows
                .iter()
                .any(|(known, _, _): &(String, String, bool)| known.eq_ignore_ascii_case(mask))
        {
            rows.push((mask.to_uppercase(), profile.to_string(), recommended));
        }
    };

    if let Some(mask) = &recommendation.mask_hex {
        add(mask, "Best match for this CPU", true);
    }
    for mask in &recommendation.alternates {
        add(mask, "Suggested benchmark alternate", false);
    }
    for &(mask, profile) in MASK_PRESETS {
        add(mask, profile, false);
    }
    rows
}

/// Animation clock id for the success checkmark on one-click step `index`.
fn check_id(index: usize) -> egui::Id {
    egui::Id::new(("oneclick-check", index))
}

/// Reset one checkmark's clock to zero so its next frame draws in from nothing.
///
/// `animate_bool_with_time` seeds an unknown id at its *target* value, so a
/// checkmark rendered for the first time would appear already finished. Forcing
/// the clock to `false` over a zero-length animation gives it somewhere to
/// travel from.
fn seed_checks_at(ctx: &egui::Context, index: usize) {
    ctx.animate_bool_with_time(check_id(index), false, 0.0);
}

/// Seed the clocks for steps `0..count`.
fn seed_checks(ctx: &egui::Context, count: usize) {
    for index in 0..count {
        seed_checks_at(ctx, index);
    }
}

/// A checkmark that pops its ring in and then draws its stroke, left to right.
///
/// Progress comes from egui's animation clock keyed on `id`; the caller seeds
/// that clock when the row first appears (see [`seed_checks_at`]).
fn animated_check(ui: &mut egui::Ui, id: egui::Id, color: Color32) {
    const DURATION: f32 = 0.55;
    let side = 15.0;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(side, side), egui::Sense::hover());
    let t = ui.ctx().animate_bool_with_time(id, true, DURATION);
    if t < 1.0 {
        ui.ctx().request_repaint();
    }

    let painter = ui.painter();
    let center = rect.center();
    let radius = side * 0.5 - 1.0;

    // Ring: eases out with a slight overshoot that settles as the stroke starts.
    let pop = ease_out_cubic((t / 0.35).clamp(0.0, 1.0));
    let scale = pop * (1.0 + 0.2 * (1.0 - pop));
    painter.circle_stroke(
        center,
        radius * scale,
        egui::Stroke::new(1.4, color.gamma_multiply(0.5)),
    );

    // Stroke: two segments treated as one path, revealed by arc length so the
    // corner is crossed at a constant speed rather than jumping.
    let points = [
        center + egui::vec2(-radius * 0.48, radius * 0.04),
        center + egui::vec2(-radius * 0.14, radius * 0.42),
        center + egui::vec2(radius * 0.52, -radius * 0.42),
    ];
    let drawn = ease_out_cubic(((t - 0.28) / (1.0 - 0.28)).clamp(0.0, 1.0));
    let first = (points[1] - points[0]).length();
    let length = drawn * (first + (points[2] - points[1]).length());
    let stroke = egui::Stroke::new(2.0, color);
    if length > 0.0 {
        let head = length.min(first);
        painter.line_segment(
            [
                points[0],
                points[0] + (points[1] - points[0]).normalized() * head,
            ],
            stroke,
        );
        if length > first {
            let head = length - first;
            painter.line_segment(
                [
                    points[1],
                    points[1] + (points[2] - points[1]).normalized() * head,
                ],
                stroke,
            );
        }
    }
}

fn ease_out_cubic(t: f32) -> f32 {
    1.0 - (1.0 - t).powi(3)
}

/// How many one-click steps are shown `elapsed` seconds after the click: one
/// immediately, then one more every `STEP_STAGGER` seconds, capped at `total`.
const STEP_STAGGER: f32 = 0.6;

fn revealed_steps(elapsed: f32, total: usize) -> usize {
    ((elapsed / STEP_STAGGER) as usize + 1).min(total)
}

/// Animation clock ids for the apply flow.
fn progress_id() -> egui::Id {
    egui::Id::new("apply-progress")
}
fn fade_id() -> egui::Id {
    egui::Id::new("apply-fadeout")
}
fn done_fade_id() -> egui::Id {
    egui::Id::new("apply-donefade")
}

/// The big round Apply button. Returns true when clicked (only while enabled).
fn round_apply_button(ui: &mut egui::Ui, enabled: bool) -> bool {
    let size = 170.0;
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(size, size),
        if enabled {
            egui::Sense::click()
        } else {
            egui::Sense::hover()
        },
    );
    let response = if enabled {
        response.on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        response
    };

    let accent = crate::theme::ACCENT;
    let hovered = enabled && response.hovered();
    let (fill, ring, ink, sub) = if enabled {
        (
            accent.gamma_multiply(if hovered { 0.30 } else { 0.20 }),
            accent,
            crate::theme::INK,
            crate::theme::INK_2,
        )
    } else {
        (
            crate::theme::PANEL,
            crate::theme::STROKE_SOFT,
            crate::theme::INK_3,
            crate::theme::INK_3,
        )
    };

    let painter = ui.painter();
    let center = rect.center();
    let radius = size / 2.0 - 8.0;
    painter.circle_filled(center, radius, fill);
    painter.circle_stroke(center, radius, egui::Stroke::new(2.0, ring));
    if enabled {
        // Soft outer glow ring, slightly stronger on hover.
        painter.circle_stroke(
            center,
            radius + 6.0,
            egui::Stroke::new(1.2, accent.gamma_multiply(if hovered { 0.55 } else { 0.3 })),
        );
    }
    painter.text(
        center - egui::vec2(0.0, 8.0),
        egui::Align2::CENTER_CENTER,
        "APPLY",
        egui::FontId::new(26.0, crate::theme::display()),
        ink,
    );
    painter.text(
        center + egui::vec2(0.0, 16.0),
        egui::Align2::CENTER_CENTER,
        "PERFORMANCE OPTIMIZER",
        egui::FontId::new(9.0, crate::theme::display()),
        sub,
    );
    enabled && response.clicked()
}

/// Turn a config-file sweep into one step outcome, refusing to call a partial
/// sweep a success.
///
/// Three things make a sweep incomplete, and none of them is an `Err` on an
/// individual file, so filtering for errors alone reported "everything applied"
/// after doing less than everything:
///
/// * a per-file failure — the obvious case;
/// * [`FileChange::Skipped`], meaning the game started mid-run and the latched
///   guard abandoned the rest of the batch;
/// * a path `discover` could not enumerate or open, which is a file that was
///   never in the batch to begin with.
///
/// `NothingRecognized` is deliberately *not* a failure: a `gamevariable.xml`
/// that holds none of the tweaked keys is the normal shape of a real install,
/// and failing on it would flag healthy machines. It is mentioned in the detail
/// instead so a sweep that recognized nothing anywhere is still visible.
#[cfg(windows)]
fn config_step_result(
    outcomes: &[crate::gameconfig::FileOutcome],
    unreadable: &[String],
    detail: String,
) -> Result<String, String> {
    use crate::gameconfig::FileChange;

    let mut problems: Vec<String> = outcomes
        .iter()
        .filter_map(|o| o.result.as_ref().err().cloned())
        .collect();

    let skipped = outcomes
        .iter()
        .filter(|o| matches!(o.result, Ok(FileChange::Skipped)))
        .count();
    if skipped > 0 {
        problems.push(format!(
            "stopped early — Black Desert started mid-run, so {skipped} file(s) were left \
             untouched; close it and click again"
        ));
    }
    problems.extend(unreadable.iter().cloned());

    if !problems.is_empty() {
        return Err(problems.join("; "));
    }

    let unrecognized = outcomes
        .iter()
        .filter(|o| matches!(o.result, Ok(FileChange::NothingRecognized)))
        .count();
    if unrecognized > 0 && unrecognized == outcomes.len() {
        return Ok(format!(
            "{detail} (no tweakable settings found in any config file)"
        ));
    }
    Ok(detail)
}

#[cfg(windows)]
fn verification_due(last: Option<std::time::Instant>, now: std::time::Instant) -> bool {
    last.map(|last| now.saturating_duration_since(last) >= std::time::Duration::from_secs(1))
        .unwrap_or(true)
}

impl App {
    pub(crate) fn refresh_verification(&mut self) {
        #[cfg(windows)]
        {
            self.optimize.verify = Some(win_actions::verify(&self.optimize.mask_input));
            self.optimize.last_verify_at = Some(std::time::Instant::now());
        }
    }

    pub(crate) fn poll_verification(&mut self, ctx: &egui::Context) {
        #[cfg(windows)]
        {
            let now = std::time::Instant::now();
            if verification_due(self.optimize.last_verify_at, now) {
                self.refresh_verification();
            }
            ctx.request_repaint_after(std::time::Duration::from_secs(1));
        }
        #[cfg(not(windows))]
        let _ = ctx;
    }

    pub(crate) fn optimize_ui(&mut self, ui: &mut egui::Ui) {
        use egui_phosphor::regular as icons;

        crate::theme::screen_heading(
            ui,
            "Apply",
            "One click applies the whole optimization — CPU-affinity shortcut, config \
             tweaks, and driver profile. Everything can be undone.",
        );

        if self.detection.is_none() {
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Detecting hardware…");
            });
            return;
        }

        // Keep verification ticking regardless of which screen is up, so the
        // sidebar check and the "verified" footer stay live.
        self.poll_verification(ui.ctx());

        match self.oneclick.phase {
            ApplyPhase::Idle => self.apply_idle_ui(ui),
            ApplyPhase::Running => self.apply_running_ui(ui),
            ApplyPhase::Done => self.apply_done_ui(ui),
        }

        if self.oneclick.phase == ApplyPhase::Running {
            return;
        }
        ui.add_space(20.0);
        egui::CollapsingHeader::new("Advanced: individual settings")
            .id_salt("apply_advanced")
            .show(ui, |ui| {
                crate::theme::section_card(ui, icons::FOLDER_OPEN, "Game path", |ui| {
                    self.game_path_section(ui)
                });
                crate::theme::section_card(ui, icons::CIRCUITRY, "Affinity", |ui| {
                    self.mask_section(ui)
                });
                crate::theme::section_card(ui, icons::ROCKET_LAUNCH, "Apply for this launch", |ui| {
                    self.actions_section(ui)
                });
                crate::theme::section_card(ui, icons::SEAL_CHECK, "Automatic verification", |ui| {
                    self.verify_section(ui)
                });
                if self.show_nvidia_section() {
                    crate::theme::section_card(
                        ui,
                        icons::SHIELD_CHECK,
                        "NVIDIA driver profile",
                        |ui| self.nvidia_section(ui),
                    );
                }
                if self.show_amd_section() {
                    crate::theme::section_card(ui, icons::SLIDERS, "AMD Radeon settings", |ui| {
                        self.amd_section(ui)
                    });
                }
                if cfg!(windows) {
                    crate::theme::section_card(ui, icons::FILE_TEXT, "Game config files", |ui| {
                        self.gameconfig_section(ui)
                    });
                }
            });
    }

    fn show_nvidia_section(&self) -> bool {
        cfg!(windows)
            && self
                .detection
                .as_ref()
                .is_some_and(|d| d.gpus.iter().any(|g| g.vendor == bdo_hw::GpuVendor::Nvidia))
    }

    fn show_amd_section(&self) -> bool {
        cfg!(windows)
            && self.detection.as_ref().is_some_and(|d| {
                d.gpus.iter().any(|g| g.vendor == bdo_hw::GpuVendor::Amd)
                    && !d.gpus.iter().any(|g| g.vendor == bdo_hw::GpuVendor::Nvidia)
            })
    }

    /// Small footer shown on the idle and done screens once the running game
    /// verifiably carries the selected mask.
    fn verified_footer(&mut self, ui: &mut egui::Ui) {
        if !matches!(&self.optimize.verify, Some(VerifyOutcome::Match { .. })) {
            return;
        }
        ui.add_space(10.0);
        ui.vertical_centered(|ui| {
            ui.label(
                RichText::new("Verified: the running game matches the selected affinity mask.")
                    .color(OK_GREEN)
                    .strong(),
            );
            if ui.button("Continue to Measure").clicked() {
                self.tab = Tab::Benchmark;
            }
        });
    }

    // --------------------------------------------------------- One-button apply
    /// Idle screen: the round Apply button, its preflight blockers, and the
    /// undo link.
    fn apply_idle_ui(&mut self, ui: &mut egui::Ui) {
        if cfg!(windows) && !self.video.inspector_resolved {
            self.video.inspector_resolved = true;
            self.video.inspector = crate::video::resolve();
        }
        let blockers = self.one_click_blockers();
        let ready = blockers.is_empty();
        let needs_admin =
            cfg!(windows) && self.show_nvidia_section() && !self.benchmark.elevated;

        ui.add_space(28.0);
        let clicked = ui
            .vertical_centered(|ui| {
                let clicked = round_apply_button(ui, ready);
                ui.add_space(12.0);
                ui.label(
                    RichText::new(
                        "Creates the optimized desktop shortcut and applies the config-file \
                         tweaks, windowed-game optimizations, and the GPU driver profile when \
                         one is available. Everything is reversible.",
                    )
                    .size(12.0)
                    .weak(),
                );
                clicked
            })
            .inner;
        if clicked {
            self.start_apply(ui.ctx(), false);
        }

        // Preflight: every reason the button is disabled, spelled out so the
        // user knows exactly what to fix before they can click.
        if !ready {
            ui.add_space(10.0);
            ui.vertical_centered(|ui| {
                for blocker in &blockers {
                    ui.label(RichText::new(format!("🔒 {blocker}")).color(WARN).size(13.0));
                }
            });
            #[cfg(windows)]
            if needs_admin {
                ui.vertical_centered(|ui| self.relaunch_admin_button(ui));
            }
        }
        let _ = needs_admin;

        if cfg!(windows) {
            ui.add_space(14.0);
            let undo = ui
                .vertical_centered(|ui| {
                    ui.small_button("Undo a previous optimization")
                        .on_hover_text(
                            "Restores the config files from their backups, puts the Windows \
                             setting back exactly as it was, and resets the driver profile.",
                        )
                        .clicked()
                })
                .inner;
            if undo {
                self.start_apply(ui.ctx(), true);
            }
        }

        self.verified_footer(ui);
    }

    /// Kick off an apply or undo run and switch to the progress screen.
    fn start_apply(&mut self, ctx: &egui::Context, undo: bool) {
        if undo {
            self.undo_one_click();
        } else {
            self.run_one_click();
        }
        self.oneclick.undoing = undo;
        self.oneclick.started = Some(std::time::Instant::now());
        self.oneclick.phase = ApplyPhase::Running;
        seed_checks(ctx, self.oneclick.steps.len());
        // Zero the progress/fade clocks so the run always animates from scratch.
        ctx.animate_value_with_time(progress_id(), 0.0, 0.0);
        ctx.animate_bool_with_time(fade_id(), false, 0.0);
        ctx.animate_bool_with_time(done_fade_id(), false, 0.0);
    }

    /// Progress screen: the BDO enhancement window, driven by the real steps.
    /// Each landed step raises the tier; the finale plays PEN on success or a
    /// failed enhance when any step errored, then hands over to the results.
    fn apply_running_ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.collect_driver_step(&ctx);

        let driver_pending = self.oneclick.driver_step.is_some();
        let total = self.oneclick.steps.len() + usize::from(driver_pending);
        let elapsed = self
            .oneclick
            .started
            .map(|s| s.elapsed().as_secs_f32())
            .unwrap_or(1e6);
        let visible = revealed_steps(elapsed, self.oneclick.steps.len());
        let target = if total == 0 {
            1.0
        } else {
            visible as f32 / total as f32
        };
        let progress = ctx.animate_value_with_time(progress_id(), target, 0.45);
        let complete = !driver_pending && visible == self.oneclick.steps.len() && progress > 0.995;
        // The fade clock times the finale: flash, shockwave and banner.
        let finale = ctx.animate_bool_with_time(fade_id(), complete, 1.1);
        if complete && finale >= 1.0 {
            self.oneclick.phase = ApplyPhase::Done;
            ctx.animate_bool_with_time(done_fade_id(), false, 0.0);
            ctx.request_repaint();
            return;
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(16));

        let failed = self
            .oneclick
            .steps
            .iter()
            .take(visible)
            .any(|(_, r)| r.is_err());
        let verb = if self.oneclick.undoing {
            "Extracting"
        } else {
            "Enhancing"
        };
        let stage = if visible < self.oneclick.steps.len() {
            format!("{verb}: {}", self.oneclick.steps[visible].0)
        } else if driver_pending {
            format!("{verb}: NVIDIA driver profile")
        } else if failed {
            "Enhancement failed — see what resisted below".to_string()
        } else {
            "Enhancement succeeded".to_string()
        };
        let mask_cores = bdo_hw::mask_to_cores(&self.optimize.mask_input)
            .map(|c| c.len())
            .unwrap_or(0);

        let logo = self.logo_texture(&ctx);
        let view = crate::enhance::EnhanceView {
            steps: &self.oneclick.steps,
            pending_driver: self.oneclick.driver_step.as_deref(),
            visible,
            progress,
            elapsed,
            finale: if complete { finale } else { 0.0 },
            failed,
            undoing: self.oneclick.undoing,
            stage,
            mask_hex: &self.optimize.mask_input,
            mask_cores,
        };
        crate::enhance::draw(ui, &logo, &view);
    }

    /// Results screen: what was applied, what failed and how to fix it.
    fn apply_done_ui(&mut self, ui: &mut egui::Ui) {
        use egui_phosphor::regular as icons;

        let ctx = ui.ctx().clone();
        let t = ctx.animate_bool_with_time(done_fade_id(), true, 0.45);
        if t < 1.0 {
            ctx.request_repaint();
        }
        let failures = self
            .oneclick
            .steps
            .iter()
            .filter(|(_, r)| r.is_err())
            .count();

        ui.add_space(24.0);
        ui.scope(|ui| {
            ui.set_opacity(t);
            ui.vertical_centered(|ui| {
                if failures == 0 {
                    ui.label(
                        RichText::new(icons::CHECK_CIRCLE)
                            .size(52.0)
                            .color(OK_GREEN),
                    );
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(if self.oneclick.undoing {
                            "Extraction complete — setup restored"
                        } else {
                            "PEN! Setup fully enhanced"
                        })
                        .font(egui::FontId::new(22.0, crate::theme::display()))
                        .color(crate::theme::INK),
                    );
                    if !self.oneclick.undoing {
                        ui.label(
                            RichText::new(
                                "Every optimization step landed. Start Black Desert with the \
                                 new desktop shortcut and enjoy the optimized settings.",
                            )
                            .size(13.0)
                            .weak(),
                        );
                    }
                } else {
                    ui.label(RichText::new(icons::WARNING).size(52.0).color(WARN));
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(format!(
                            "Enhancement failed — {failures} step{} resisted",
                            if failures == 1 { "" } else { "s" },
                        ))
                        .font(egui::FontId::new(22.0, crate::theme::display()))
                        .color(crate::theme::INK),
                    );
                    ui.label(
                        RichText::new(
                            "Durability lost: none. Fix the items below, then enhance again.",
                        )
                        .size(13.0)
                        .weak(),
                    );
                }
            });

            ui.add_space(16.0);
            for (index, (step, outcome)) in self.oneclick.steps.iter().enumerate() {
                match outcome {
                    Ok(detail) => {
                        ui.horizontal(|ui| {
                            animated_check(ui, check_id(index), OK_GREEN);
                            ui.label(
                                RichText::new(format!("{step}: {detail}"))
                                    .color(OK_GREEN)
                                    .size(12.0),
                            );
                        });
                    }
                    Err(e) => {
                        ui.label(
                            RichText::new(format!("✘ {step}: {e}"))
                                .color(ERR)
                                .size(12.0),
                        );
                    }
                }
            }
            if failures == 0 && !self.oneclick.undoing {
                ui.add_space(8.0);
                ui.label(
                    RichText::new(
                        "Rollback any time: launch BDO normally, delete the shortcut, or use \
                         \"Undo a previous optimization\". No global system settings were \
                         changed.",
                    )
                    .size(12.0)
                    .weak(),
                );
            }

            ui.add_space(14.0);
            ui.vertical_centered(|ui| {
                if ui
                    .button(if failures == 0 { "Back" } else { "Fix and retry" })
                    .clicked()
                {
                    self.oneclick.phase = ApplyPhase::Idle;
                }
            });
            self.verified_footer(ui);
        });
    }

    /// Everything that would make a one-click run fail, checked before the
    /// button is enabled. Uses only state that is already polled each frame —
    /// no extra process or disk scans.
    fn one_click_blockers(&self) -> Vec<String> {
        let mut blockers = Vec::new();
        if !cfg!(windows) {
            blockers.push(
                "Windows only — Black Desert and these optimizations require Windows."
                    .to_string(),
            );
            return blockers;
        }
        if matches!(
            self.optimize.verify,
            Some(VerifyOutcome::Match { .. }) | Some(VerifyOutcome::Mismatch { .. })
        ) {
            blockers.push(
                "Black Desert is running — close it first; the game rewrites its config files \
                 on exit."
                    .to_string(),
            );
        }
        if !self
            .optimize
            .launcher_path()
            .map(|p| p.exists())
            .unwrap_or(false)
        {
            blockers.push(
                "No game launcher found — pick BlackDesertLauncher.exe in the Game path \
                 section below."
                    .to_string(),
            );
        }
        let logical = self.detection.as_ref().map(|d| d.cpu.logical_cores);
        if bdo_launch::parse_mask_hex(&self.optimize.mask_input)
            .and_then(|mask| bdo_launch::validate_mask(mask, logical))
            .is_err()
        {
            blockers.push(
                "The affinity mask is invalid — fix it in the Affinity section below.".to_string(),
            );
        }
        if self.show_nvidia_section() {
            if !self.benchmark.elevated {
                blockers.push(
                    "You need to run the app as administrator so the NVIDIA driver profile can \
                     be written:"
                        .to_string(),
                );
            } else if self.video.inspector.is_none() {
                blockers.push(format!(
                    "{} was not found next to the app — the NVIDIA profile step cannot run.",
                    crate::video::INSPECTOR_EXE
                ));
            }
        }
        blockers
    }

    /// The "Restart as administrator" button plus its error line, shared by
    /// the one-click preflight and the NVIDIA section.
    #[cfg(windows)]
    fn relaunch_admin_button(&mut self, ui: &mut egui::Ui) {
        if ui
            .button(RichText::new("🛡 Restart as administrator").strong())
            .on_hover_text("Relaunches this app with a UAC prompt, then closes this window.")
            .clicked()
        {
            self.benchmark.relaunch_error = None;
            match crate::relaunch::relaunch_as_admin() {
                crate::relaunch::RelaunchOutcome::Launched => {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
                crate::relaunch::RelaunchOutcome::Cancelled => {}
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
    }

    /// Fold a finished driver-profile job started by the one-click button back
    /// Drain the driver-profile worker's result into `video.last`.
    ///
    /// Called from the app update loop every frame, not from a section UI: the
    /// one-click progress screen hides the NVIDIA section entirely, so draining
    /// there would deadlock the run at "Applying: NVIDIA driver profile" — the
    /// worker done, its result forever unread.
    #[cfg(windows)]
    pub fn poll_driver_worker(&mut self) {
        use std::sync::mpsc::TryRecvError;

        let finished = match self.video.worker.as_ref().map(|worker| worker.rx.try_recv()) {
            Some(Ok(result)) => Some(result),
            // The worker thread ended without sending — a panic inside the job.
            // Treating that as "still running" leaves the one-click screen
            // waiting forever on a result that can never arrive, with the
            // Advanced section hidden and no way out but restarting the app.
            Some(Err(TryRecvError::Disconnected)) => Some(Err(
                "the driver-profile job stopped unexpectedly — try again, and run the app as \
                 administrator if it keeps failing"
                    .to_string(),
            )),
            Some(Err(TryRecvError::Empty)) | None => None,
        };
        if let Some(result) = finished {
            self.video.last = Some(result);
            self.video.worker = None;
        }
    }

    /// into its step list. The driver pipeline is the only asynchronous step, so
    /// its result arrives a frame or two after the rest.
    #[cfg(windows)]
    fn collect_driver_step(&mut self, ctx: &egui::Context) {
        if self.video.worker.is_some() {
            return;
        }
        let Some(step) = self.oneclick.driver_step.take() else {
            return;
        };
        // This row appears frames after the synchronous ones, so it needs its
        // own seed to animate instead of popping in finished.
        seed_checks_at(ctx, self.oneclick.steps.len());
        // A worker gone without a result must still be reported: dropping the
        // row silently would let the results screen claim every step landed.
        let result = self.video.last.clone().unwrap_or_else(|| {
            Err("the driver-profile job ended without reporting a result".to_string())
        });
        self.oneclick.steps.push((step, result));
    }

    #[cfg(not(windows))]
    fn collect_driver_step(&mut self, _ctx: &egui::Context) {}

    /// Run the safe steps in order, recording each outcome separately so a
    /// failure in one never reads as a failure of the whole thing.
    fn run_one_click(&mut self) {
        self.oneclick.steps.clear();
        self.oneclick.driver_step = None;

        #[cfg(windows)]
        {
            self.one_click_affinity_shortcut();
            self.one_click_config_files();
            self.one_click_windowed_optimizations();
            self.one_click_driver_profile(
                crate::video::guide_settings(
                    self.detection.as_ref().map_or(0, |d| d.cpu.physical_cores),
                    self.video.ull,
                ),
                "apply",
            );
        }
    }

    /// Start the driver-profile job as part of a one-click run.
    ///
    /// Skipped silently on non-NVIDIA hosts (nothing to apply). Everything else
    /// that stops it — no elevation, missing inspector — is reported as a failed
    /// step rather than swallowed, because the user asked for it.
    #[cfg(windows)]
    fn one_click_driver_profile(&mut self, settings: Vec<(u32, u32)>, label: &'static str) {
        const STEP: &str = "NVIDIA driver profile";
        let has_nvidia = self
            .detection
            .as_ref()
            .is_some_and(|d| d.gpus.iter().any(|g| g.vendor == bdo_hw::GpuVendor::Nvidia));
        if !has_nvidia {
            return;
        }
        if !self.benchmark.elevated {
            self.oneclick.steps.push((
                STEP.into(),
                Err(
                    "needs administrator — use \"Restart as administrator\" in the NVIDIA \
                     section below, then click again"
                        .into(),
                ),
            ));
            return;
        }
        if !self.video.inspector_resolved {
            self.video.inspector_resolved = true;
            self.video.inspector = crate::video::resolve();
        }
        let Some(exe) = self.video.inspector.clone() else {
            self.oneclick.steps.push((
                STEP.into(),
                Err(format!(
                    "{} not found next to the app",
                    crate::video::INSPECTOR_EXE
                )),
            ));
            return;
        };
        if self.video.worker.is_some() {
            self.oneclick.steps.push((
                STEP.into(),
                Err("a driver-profile job is already running".into()),
            ));
            return;
        }
        // Record before the job runs, not after: a crash mid-import must not
        // leave settings written with no record of which ones, or undo would
        // reset the wrong set.
        if label == "apply" {
            crate::video::record_applied(&settings);
        } else {
            crate::video::clear_applied_record();
        }
        self.video.last = None;
        self.video.worker = Some(crate::video::worker::start(exe, settings, label));
        self.oneclick.driver_step = Some(STEP.to_string());
    }

    /// Reverse everything [`Self::run_one_click`] applied.
    fn undo_one_click(&mut self) {
        self.oneclick.steps.clear();
        self.oneclick.driver_step = None;

        #[cfg(windows)]
        {
            const STEP: &str = "Game config files";
            let game_absent = || bdo_bench::is_process_running(bdo_launch::GAME_EXE).is_none();
            match crate::gameconfig::config_root() {
                Some(root) if !game_absent() => {
                    let _ = root;
                    self.oneclick.steps.push((
                        STEP.into(),
                        Err("Black Desert is running — close it and click again".into()),
                    ));
                }
                Some(root) => {
                    let found = crate::gameconfig::discover(&root);
                    let outcomes =
                        crate::gameconfig::restore_files(&root, &found.files, &game_absent);
                    let restored = outcomes
                        .iter()
                        .filter(|o| matches!(o.result, Ok(crate::gameconfig::FileChange::Restored)))
                        .count();
                    let result = config_step_result(
                        &outcomes,
                        &found.unreadable,
                        format!("{restored} file(s) restored"),
                    );
                    self.oneclick.steps.push((STEP.into(), result));
                    self.gameconfig.outcomes = outcomes;
                    self.gameconfig.last_action = Some("restore");
                }
                None => self
                    .oneclick
                    .steps
                    .push((STEP.into(), Err("Documents folder not found".into()))),
            }

            let step = crate::winsettings::WINDOWED_OPT_LABEL.to_string();
            match crate::winsettings::restore_windowed_optimizations() {
                Ok(true) => self.oneclick.steps.push((step, Ok("put back".into()))),
                Ok(false) => self
                    .oneclick
                    .steps
                    .push((step, Ok("was never changed by this app".into()))),
                Err(e) => self.oneclick.steps.push((step, Err(e))),
            }

            // Reset only what an apply actually wrote — see
            // [`crate::video::restore_settings`].
            let applied = crate::video::recorded_applied();
            self.one_click_driver_profile(crate::video::restore_settings(&applied), "restore");
        }
    }

    /// Create the optimized run-with-affinity desktop shortcut, mirroring the
    /// result into the manual "Apply for this launch" section below.
    #[cfg(windows)]
    fn one_click_affinity_shortcut(&mut self) {
        const STEP: &str = "CPU affinity";
        let result = win_actions::create_shortcut(
            self.optimize.launcher_path(),
            &self.optimize.mask_input,
            self.optimize.steam,
        );
        self.optimize.shortcut_result = Some(result.clone());
        self.oneclick.steps.push((
            STEP.into(),
            result.map(|path| format!("optimized shortcut created at {}", path.display())),
        ));
    }

    #[cfg(windows)]
    fn one_click_config_files(&mut self) {
        const STEP: &str = "Game config files";
        let Some(root) = crate::gameconfig::config_root() else {
            self.oneclick
                .steps
                .push((STEP.into(), Err("Documents folder not found".into())));
            return;
        };
        let game_absent = || bdo_bench::is_process_running(bdo_launch::GAME_EXE).is_none();
        if !game_absent() {
            self.oneclick.steps.push((
                STEP.into(),
                Err("Black Desert is running — close it and click again".into()),
            ));
            return;
        }
        let found = crate::gameconfig::discover(&root);
        if found.files.is_empty() {
            self.oneclick.steps.push((
                STEP.into(),
                Err("no config files found — start the game once so it creates them".into()),
            ));
            return;
        }
        let outcomes = crate::gameconfig::apply_files(&root, &found.files, &game_absent);
        let changed: usize = outcomes
            .iter()
            .filter_map(|o| match o.result {
                Ok(crate::gameconfig::FileChange::Patched(n)) => Some(n),
                _ => None,
            })
            .sum();
        let detail = if changed == 0 {
            "already applied".to_string()
        } else {
            format!("{changed} value(s) set across {} file(s)", outcomes.len())
        };
        let result = config_step_result(&outcomes, &found.unreadable, detail);
        self.oneclick.steps.push((STEP.into(), result));
        // Keep the detailed per-file view below in sync with what just ran.
        self.gameconfig.outcomes = outcomes;
        self.gameconfig.last_action = Some("apply");
    }

    #[cfg(windows)]
    fn one_click_windowed_optimizations(&mut self) {
        let step = crate::winsettings::WINDOWED_OPT_LABEL.to_string();
        match crate::winsettings::enable_windowed_optimizations_recording_undo() {
            Ok(true) => self.oneclick.steps.push((step, Ok("enabled".into()))),
            Ok(false) => self
                .oneclick
                .steps
                .push((step, Ok("already enabled".into()))),
            Err(e) => self.oneclick.steps.push((step, Err(e))),
        }
    }

    // --------------------------------------------------------------- Game path
    fn game_path_section(&mut self, ui: &mut egui::Ui) {
        if !cfg!(windows) {
            ui.label(
                RichText::new(
                    "Automatic BDO detection is only available on Windows. Use Browse… to point at the launcher.",
                )
                .weak()
                .size(12.0),
            );
        }

        let install_labels: Vec<String> = self
            .optimize
            .installs
            .iter()
            .map(|p| p.display().to_string())
            .collect();

        if install_labels.is_empty() {
            ui.label(RichText::new("No install auto-detected.").italics().weak());
        } else if install_labels.len() == 1 {
            ui.label(&install_labels[0]);
        } else {
            let mut sel = self.optimize.selected_install.min(install_labels.len() - 1);
            egui::ComboBox::from_label("Detected installs")
                .selected_text(install_labels[sel].clone())
                .show_ui(ui, |ui| {
                    for (i, label) in install_labels.iter().enumerate() {
                        ui.selectable_value(&mut sel, i, label);
                    }
                });
            self.optimize.selected_install = sel;
        }

        ui.horizontal(|ui| {
            if ui.button("Browse…").clicked() {
                if let Some(path) = rfd::FileDialog::new()
                    .set_title("Select BlackDesertLauncher.exe")
                    .add_filter("Executable", &["exe"])
                    .pick_file()
                {
                    self.optimize.manual_launcher = Some(path);
                }
            }
            #[cfg(windows)]
            if ui
                .button("Re-detect")
                .on_hover_text("Re-scan common paths, Steam libraries, and the uninstall registry.")
                .clicked()
            {
                self.optimize.installs = bdo_launch::find_bdo_install();
                self.optimize.selected_install = 0;
            }
            if let Some(m) = &self.optimize.manual_launcher {
                ui.label(
                    RichText::new(format!("Manual: {}", m.display()))
                        .monospace()
                        .size(12.0),
                );
            }
        });

        match self.optimize.launcher_path() {
            Some(p) => {
                ui.label(
                    RichText::new(format!("Launcher: {}", p.display()))
                        .monospace()
                        .size(12.0),
                );
            }
            None => {
                ui.label(
                    RichText::new(
                        "No launcher selected — pick BlackDesertLauncher.exe with Browse….",
                    )
                    .color(WARN)
                    .size(12.0),
                );
            }
        }
    }

    // --------------------------------------------------------------- Mask entry
    fn mask_section(&mut self, ui: &mut egui::Ui) {
        let Some(detection) = &self.detection else {
            return;
        };
        let recommendation = &detection.recommendation;
        ui.horizontal(|ui| {
            if let Some(mask) = &recommendation.mask_hex {
                if ui
                    .button("Use recommended mask")
                    .on_hover_text(format!("Set the affinity mask to 0x{mask}"))
                    .clicked()
                {
                    self.optimize.mask_input = mask.clone();
                    self.optimize.last_verify_at = None;
                }
                ui.label(
                    RichText::new(format!("0x{mask} for {}", detection.cpu.model))
                        .color(OK_GREEN)
                        .strong(),
                );
            } else {
                ui.add_enabled(false, egui::Button::new("No exact recommendation"));
            }
        });
        ui.label(RichText::new(&recommendation.explanation).size(12.0).weak());

        // The mask the user is about to apply, drawn on the actual CPU.
        if let Ok(cores) = bdo_hw::mask_to_cores(&self.optimize.mask_input) {
            if !cores.is_empty() && cores.iter().all(|c| *c < detection.cpu.logical_cores) {
                ui.add_space(6.0);
                crate::theme::core_map(ui, &detection.cpu, &cores);
                ui.add_space(6.0);
            }
        }

        ui.horizontal(|ui| {
            ui.label("Current mask:");
            if ui
                .add(
                    egui::TextEdit::singleline(&mut self.optimize.mask_input)
                        .desired_width(120.0)
                        .font(egui::TextStyle::Monospace),
                )
                .changed()
            {
                self.optimize.last_verify_at = None;
            }
            ui.checkbox(&mut self.optimize.steam, "Steam version (-steam)");
        });

        let logical = detection.cpu.logical_cores;
        let rows = available_masks(recommendation, logical);
        egui::CollapsingHeader::new("Advanced: choose a different supported mask").show(ui, |ui| {
            ui.label(
                RichText::new(format!(
                    "{} supported BDO masks fit this CPU's {logical} logical processors.",
                    rows.len()
                ))
                .size(12.0)
                .weak(),
            );
            egui::ScrollArea::vertical()
                .id_salt("affinity_mask_table_scroll")
                .max_height(260.0)
                .show(ui, |ui| {
                    egui::Grid::new("affinity_mask_table")
                        .num_columns(5)
                        .striped(true)
                        .spacing([16.0, 5.0])
                        .show(ui, |ui| {
                            for heading in ["", "Mask", "Logical cores", "CPU profile", ""] {
                                ui.label(RichText::new(heading).strong());
                            }
                            ui.end_row();

                            for (mask, profile, recommended) in rows {
                                if recommended {
                                    ui.label(RichText::new("Recommended").color(OK_GREEN).strong());
                                } else {
                                    ui.label("");
                                }
                                ui.monospace(format!("0x{mask}"));
                                let cores = bdo_launch::parse_mask_hex(&mask)
                                    .map(bdo_launch::mask_to_cores)
                                    .unwrap_or_default();
                                ui.label(format::cores(&cores));
                                ui.label(profile);

                                let selected = self.optimize.mask_input.eq_ignore_ascii_case(&mask);
                                if ui
                                    .add_enabled(
                                        !selected,
                                        egui::Button::new(if selected {
                                            "Selected"
                                        } else {
                                            "Use"
                                        }),
                                    )
                                    .clicked()
                                {
                                    self.optimize.mask_input = mask;
                                    self.optimize.last_verify_at = None;
                                }
                                ui.end_row();
                            }
                        });
                });
        });

        match bdo_launch::parse_mask_hex(&self.optimize.mask_input) {
            Ok(mask) => {
                let cores = bdo_launch::mask_to_cores(mask);
                match bdo_launch::validate_mask(mask, Some(logical)) {
                    Ok(()) => {
                        ui.label(
                            RichText::new(format!("Selected cores: {}", format::cores(&cores)))
                                .color(OK_GREEN),
                        );
                    }
                    Err(e) => {
                        ui.label(
                            RichText::new(format!(
                                "Selected cores: {} - but {}",
                                format::cores(&cores),
                                e
                            ))
                            .color(WARN),
                        );
                    }
                }
            }
            Err(e) => {
                ui.label(RichText::new(format!("Invalid mask: {e}")).color(ERR));
            }
        }
    }

    // ----------------------------------------------------------------- Actions
    fn actions_section(&mut self, ui: &mut egui::Ui) {
        if !cfg!(windows) {
            ui.label(
                RichText::new(
                    "Creating the optimized shortcut and launching with affinity are Windows-only \
                     (Black Desert Online runs only on Windows).",
                )
                .color(WARN),
            );
            return;
        }

        #[cfg(windows)]
        {
            let launcher = self.optimize.launcher_path();
            let mask_hex = self.optimize.mask_input.clone();
            let steam = self.optimize.steam;
            // A mask the CPU cannot satisfy must gate these too, not just the
            // launcher path: otherwise the shortcut is written with an affinity
            // `start` will refuse, and Launch Now walks into the failing Windows
            // call. The section above already says the mask is out of range —
            // the buttons should not disagree with it.
            let logical = self.detection.as_ref().map(|d| d.cpu.logical_cores);
            let mask_ok = bdo_launch::parse_mask_hex(&mask_hex)
                .and_then(|mask| bdo_launch::validate_mask(mask, logical))
                .is_ok();
            let has_launcher = launcher.as_ref().map(|p| p.exists()).unwrap_or(false);
            let ready = has_launcher && mask_ok;

            ui.horizontal(|ui| {
                if ui
                    .add_enabled(ready, egui::Button::new("Create Optimized Shortcut"))
                    .clicked()
                {
                    self.optimize.shortcut_result = Some(win_actions::create_shortcut(
                        launcher.clone(),
                        &mask_hex,
                        steam,
                    ));
                }
                if ui
                    .add_enabled(ready, egui::Button::new("Launch Now with Affinity"))
                    .clicked()
                {
                    self.optimize.launch_result =
                        Some(win_actions::launch(launcher.clone(), &mask_hex, steam));
                    self.optimize.last_verify_at = None;
                }
            });

            if !has_launcher {
                ui.label(
                    RichText::new("Select a valid launcher path above to enable these actions.")
                        .size(12.0)
                        .weak(),
                );
            } else if !mask_ok {
                ui.label(
                    RichText::new(
                        "Fix the affinity mask above to enable these actions — this one does \
                         not fit this CPU.",
                    )
                    .size(12.0)
                    .color(WARN),
                );
            }

            if let Some(res) = &self.optimize.shortcut_result {
                match res {
                    Ok(path) => ui.label(
                        RichText::new(format!("✔ Shortcut created: {}", path.display()))
                            .color(OK_GREEN),
                    ),
                    Err(e) => ui.label(RichText::new(format!("Shortcut failed: {e}")).color(ERR)),
                };
            }
            if let Some(res) = &self.optimize.launch_result {
                use bdo_launch::{LaunchError, LaunchMethod};
                match res {
                    Ok(LaunchMethod::Direct { pid }) => ui.label(
                        RichText::new(format!(
                            "✔ Launched directly (launcher PID {pid}). The game inherits the mask."
                        ))
                        .color(OK_GREEN)
                        .strong(),
                    ),
                    Ok(LaunchMethod::ElevatedShell) => ui.label(
                        RichText::new(
                            "✔ Launch requested via elevated shell (UAC). Once the launcher \
                             starts, the game inherits the mask.",
                        )
                        .color(OK_GREEN)
                        .strong(),
                    ),
                    Err(LaunchError::Cancelled) => ui.label(
                        RichText::new("Launch cancelled at the UAC prompt.")
                            .color(WARN)
                            .strong(),
                    ),
                    Err(e) => ui.label(
                        RichText::new(format!("Launch failed: {e}"))
                            .color(ERR)
                            .strong()
                            .size(14.0),
                    ),
                };
            }
        }
    }

    // ------------------------------------------------------ AMD driver settings
    /// The guide's Radeon advice as manual steps. Shown only on Windows hosts
    /// with an AMD GPU.
    ///
    /// Manual on purpose: AMD's per-game profiles live in a proprietary blob
    /// that Adrenalin overwrites at will, and AMD's supported SDK (ADLX) can
    /// only change *global* GPU settings — every game, not just BDO. There is
    /// no safe per-game write path for an external app, so unlike the NVIDIA
    /// side this stays a walkthrough instead of a button.
    fn amd_section(&mut self, ui: &mut egui::Ui) {
        if !cfg!(windows) {
            return;
        }
        // Only when Radeon is what BDO will run on: an NVIDIA card alongside an
        // AMD iGPU (the common Ryzen APU + GeForce build) gets the automated
        // NVIDIA profile instead, and this walkthrough would be noise.
        let amd_only = self.detection.as_ref().is_some_and(|d| {
            d.gpus.iter().any(|g| g.vendor == bdo_hw::GpuVendor::Amd)
                && !d.gpus.iter().any(|g| g.vendor == bdo_hw::GpuVendor::Nvidia)
        });
        if !amd_only {
            return;
        }

        ui.label(
            RichText::new(
                "The guide's driver tweak for Radeon is two switches in one screen. AMD \
                 offers no safe way for an app to write a per-game profile (Adrenalin owns \
                 and overwrites them), so these two are set by hand:",
            )
            .size(12.0)
            .weak(),
        );
        for step in [
            "1.  Open AMD Software: Adrenalin Edition (right-click the desktop).",
            "2.  Gaming → Games → Black Desert. If it is not listed, add \
             BlackDesert64.exe via the ⋯ menu → Add A Game.",
            "3.  Enhanced Sync: On.",
            "4.  Wait for Vertical Refresh: Always Off.",
        ] {
            ui.label(RichText::new(step).size(12.0));
        }
        ui.label(
            RichText::new(
                "Leave injection-based extras (e.g. the retired Anti-Lag+) alone — BDO runs \
                 Easy Anti-Cheat. The original driver-level Anti-Lag is safe if you want to \
                 try it.",
            )
            .size(12.0)
            .weak(),
        );
    }

    // ---------------------------------------------------- NVIDIA driver profile
    /// The guide's NVIDIA per-game profile tweaks, applied through the bundled
    /// Profile Inspector. Hidden entirely on non-Windows and non-NVIDIA hosts.
    fn nvidia_section(&mut self, ui: &mut egui::Ui) {
        if !cfg!(windows) {
            return;
        }
        let (has_nvidia, physical_cores) = match &self.detection {
            Some(d) => (
                d.gpus.iter().any(|g| g.vendor == bdo_hw::GpuVendor::Nvidia),
                d.cpu.physical_cores,
            ),
            None => return,
        };
        if !has_nvidia {
            return;
        }

        ui.label(
            RichText::new(
                "Writes only the driver's per-game \"Black Desert\" profile using the bundled \
                 NVIDIA Profile Inspector (merge import). Other profile settings — G-Sync \
                 included — and all global driver settings stay untouched. Every apply is \
                 verified against the driver database afterwards.",
            )
            .size(12.0)
            .weak(),
        );

        if !self.video.inspector_resolved {
            self.video.inspector_resolved = true;
            self.video.inspector = crate::video::resolve();
        }

        let threaded = if physical_cores >= 6 {
            "On (6+ cores)"
        } else {
            "Off (guide rule for older quad-cores)"
        };
        ui.label(
            RichText::new(format!(
                "Applies: Threaded optimization {threaded} · Ansel Off · Power management Adaptive{}",
                if self.video.ull {
                    " · Ultra Low Latency On"
                } else {
                    ""
                }
            ))
            .size(12.0),
        );
        // The running job captured this value at start; letting it change
        // mid-run would make the result message describe a different profile
        // than the one actually imported.
        #[cfg(windows)]
        let ull_locked = self.video.worker.is_some();
        #[cfg(not(windows))]
        let ull_locked = false;
        ui.add_enabled_ui(!ull_locked, |ui| {
            ui.checkbox(
                &mut self.video.ull,
                "Also enable Ultra Low Latency (guide: test whether your CPU handles the overhead)",
            );
        });

        #[cfg(windows)]
        {
            // Profile Inspector's manifest is requireAdministrator, so spawning
            // it silently only works from an elevated app (same relaunch flow
            // the Benchmark tab uses).
            if !self.benchmark.elevated {
                ui.label(
                    RichText::new(
                        "Driver-profile changes need administrator rights. Restart the app as \
                         administrator to enable these actions.",
                    )
                    .color(WARN)
                    .size(12.0),
                );
                self.relaunch_admin_button(ui);
                return;
            }

            // The worker is drained centrally by `poll_driver_worker`; this
            // section only displays the in-flight state.
            if let Some(worker) = &self.video.worker {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(if worker.label == "apply" {
                        "Applying and verifying the driver profile…"
                    } else {
                        "Restoring driver defaults…"
                    });
                });
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(250));
            }

            let busy = self.video.worker.is_some();
            match self.video.inspector.clone() {
                Some(exe) => {
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(!busy, egui::Button::new("Apply guide profile"))
                            .clicked()
                        {
                            let settings =
                                crate::video::guide_settings(physical_cores, self.video.ull);
                            crate::video::record_applied(&settings);
                            self.video.last = None;
                            self.video.worker =
                                Some(crate::video::worker::start(exe.clone(), settings, "apply"));
                        }
                        if ui
                            .add_enabled(!busy, egui::Button::new("Restore driver defaults"))
                            .on_hover_text(
                                "Resets only the settings this app applied, so a Low Latency \
                                 setup you configured yourself is left alone.",
                            )
                            .clicked()
                        {
                            let applied = crate::video::recorded_applied();
                            crate::video::clear_applied_record();
                            self.video.last = None;
                            self.video.worker = Some(crate::video::worker::start(
                                exe,
                                crate::video::restore_settings(&applied),
                                "restore",
                            ));
                        }
                    });
                }
                None => {
                    let locations = crate::video::expected_locations();
                    ui.label(
                        RichText::new(format!(
                            "{} not found — expected {} or {}.",
                            crate::video::INSPECTOR_EXE,
                            locations[0],
                            locations[1]
                        ))
                        .color(WARN),
                    );
                }
            }

            if let Some(result) = &self.video.last {
                match result {
                    Ok(msg) => ui.label(RichText::new(format!("✔ {msg}")).color(OK_GREEN)),
                    Err(e) => ui.label(
                        RichText::new(format!("Driver profile action failed: {e}")).color(ERR),
                    ),
                };
            }
        }
    }

    // ------------------------------------------------------- Game config files
    /// The guide's `GameOption.txt` / `gamevariable.xml` edits (PostFilter and
    /// Tessellation off), with one-time backups and a restore button.
    fn gameconfig_section(&mut self, ui: &mut egui::Ui) {
        if !cfg!(windows) {
            return;
        }
        ui.label(
            RichText::new(
                "Turns off PostFilter (forced sharpening) and Tessellation in GameOption.txt and \
                 every UserCache gamevariable.xml, exactly as the guide describes. The original \
                 of each file is backed up next to it the first time it is changed.",
            )
            .size(12.0)
            .weak(),
        );
        // The guide calls this out explicitly, and it is the one way these edits
        // silently revert — worth saying where the button is, not just in a
        // readme the user will not have open while playing.
        ui.label(
            RichText::new(
                "Note: switching on the in-game Display Filter setting undoes these edits.",
            )
            .size(12.0)
            .color(WARN),
        );

        let Some(root) = crate::gameconfig::config_root() else {
            ui.label(RichText::new("Documents folder not found.").color(WARN));
            return;
        };
        let found = crate::gameconfig::discover(&root);
        // Anything the scan could not account for means the sweep is
        // incomplete — say so rather than letting a reduced file count read as
        // "all done". Each entry already describes itself; wrapping them in a
        // "directory" sentence would mislabel per-file problems.
        if !found.unreadable.is_empty() {
            ui.label(
                RichText::new("Some paths were skipped, so this sweep is incomplete:")
                    .color(WARN)
                    .size(12.0),
            );
            for problem in &found.unreadable {
                ui.label(RichText::new(format!("• {problem}")).color(WARN).size(12.0));
            }
        }
        if found.files.is_empty() {
            ui.label(
                RichText::new(format!(
                    "No BDO config files found under {}. Start the game once so it creates them.",
                    root.display()
                ))
                .weak()
                .size(12.0),
            );
            return;
        }
        ui.label(
            RichText::new(format!(
                "{} file(s) found under {}",
                found.files.len(),
                root.display()
            ))
            .size(12.0)
            .weak(),
        );

        let files = found.files.clone();
        ui.horizontal(|ui| {
            if ui.button("Set PostFilter & Tessellation to 0").clicked() {
                self.run_gameconfig(&root, &files, "apply");
            }
            if ui.button("Restore from backup").clicked() {
                self.run_gameconfig(&root, &files, "restore");
            }
        });

        if let Some(msg) = &self.gameconfig.blocked {
            ui.label(RichText::new(msg.as_str()).color(WARN).strong());
        }
        for outcome in &self.gameconfig.outcomes {
            use crate::gameconfig::FileChange;
            let name = outcome
                .path
                .strip_prefix(&root)
                .unwrap_or(&outcome.path)
                .display();
            match &outcome.result {
                Ok(FileChange::Patched(n)) => ui.label(
                    RichText::new(format!("✔ {name}: {n} value(s) set to guide defaults"))
                        .color(OK_GREEN)
                        .size(12.0),
                ),
                Ok(FileChange::AlreadyOptimized) => {
                    ui.label(RichText::new(format!("• {name}: already optimized")).size(12.0))
                }
                Ok(FileChange::NothingRecognized) => ui.label(
                    RichText::new(format!(
                        "! {name}: no PostFilter/Tessellation settings recognized — left unchanged"
                    ))
                    .color(WARN)
                    .size(12.0),
                ),
                Ok(FileChange::Restored) => ui.label(
                    RichText::new(format!("✔ {name}: restored original"))
                        .color(OK_GREEN)
                        .size(12.0),
                ),
                Ok(FileChange::NoBackup) => {
                    ui.label(RichText::new(format!("• {name}: no backup to restore")).size(12.0))
                }
                Ok(FileChange::Skipped) => ui.label(
                    RichText::new(format!("! {name}: skipped — BDO started mid-run"))
                        .color(WARN)
                        .size(12.0),
                ),
                Err(e) => ui.label(
                    RichText::new(format!("✘ {name}: {e}"))
                        .color(ERR)
                        .size(12.0),
                ),
            };
        }
    }

    /// Run a config-file action, refusing while BDO is running (the game
    /// rewrites these files on exit, so an edit would be lost or race).
    fn run_gameconfig(
        &mut self,
        root: &std::path::Path,
        files: &[std::path::PathBuf],
        action: &'static str,
    ) {
        self.gameconfig.blocked = None;
        // Stale results from an earlier click must not survive next to a fresh
        // warning — they would read as if this click had succeeded.
        self.gameconfig.outcomes.clear();
        self.gameconfig.last_action = None;

        let game_absent = || bdo_bench::is_process_running(bdo_launch::GAME_EXE).is_none();
        if !game_absent() {
            self.gameconfig.blocked = Some(
                "BDO is running — close the game first. It rewrites these files on exit, \
                 which would undo the change."
                    .to_string(),
            );
            return;
        }
        // The guard is re-checked before each file, so a launch mid-batch stops
        // the run instead of writing behind the game's back.
        self.gameconfig.outcomes = if action == "apply" {
            crate::gameconfig::apply_files(root, files, &game_absent)
        } else {
            crate::gameconfig::restore_files(root, files, &game_absent)
        };
        if self
            .gameconfig
            .outcomes
            .iter()
            .any(|o| matches!(o.result, Ok(crate::gameconfig::FileChange::Skipped)))
        {
            self.gameconfig.blocked = Some(
                "BDO launched while files were being processed — the run stopped early. \
                 Close the game and click again."
                    .to_string(),
            );
        }
        self.gameconfig.last_action = Some(action);
    }

    // ------------------------------------------------------------------ Verify
    fn verify_section(&mut self, ui: &mut egui::Ui) {
        ui.label(
            RichText::new(
                "The app checks the running game every second. This is read-only; the game inherits the mask at launch.",
            )
            .size(12.0)
            .weak(),
        );

        if !cfg!(windows) {
            ui.label(RichText::new("Verification is Windows-only.").color(WARN));
            return;
        }

        #[cfg(windows)]
        {
            self.poll_verification(ui.ctx());

            if let Some(outcome) = &self.optimize.verify {
                match outcome {
                    VerifyOutcome::NotRunning => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label("Watching for BlackDesert64.exe...");
                        });
                    }
                    VerifyOutcome::Match { mask } => {
                        ui.label(
                            RichText::new(format!(
                                "Verified: running mask 0x{mask:x} matches the selected mask.",
                            ))
                            .color(OK_GREEN)
                            .strong(),
                        );
                    }
                    VerifyOutcome::Mismatch { actual, expected } => {
                        ui.label(
                            RichText::new(format!(
                                "Mismatch: running mask 0x{actual:x}, selected mask 0x{expected:x}.",
                            ))
                            .color(WARN)
                            .strong(),
                        );
                    }
                    VerifyOutcome::BadExpected(e) => {
                        ui.label(
                            RichText::new(format!("Selected mask is invalid: {e}")).color(ERR),
                        );
                    }
                    VerifyOutcome::Error(e) => {
                        ui.label(RichText::new(format!("Verification error: {e}")).color(ERR));
                    }
                }
            }
        }
    }
}

#[cfg(windows)]
mod win_actions {
    use std::path::PathBuf;

    use bdo_launch::{parse_mask_hex, ShortcutOptions, GAME_EXE};

    use crate::app::VerifyOutcome;

    pub fn create_shortcut(
        launcher: Option<PathBuf>,
        mask_hex: &str,
        steam: bool,
    ) -> Result<PathBuf, String> {
        let launcher = launcher.ok_or_else(|| "no launcher path selected".to_string())?;
        let mut opts = ShortcutOptions::new(launcher, mask_hex.to_string());
        opts.steam = steam;
        bdo_launch::create_shortcut(opts).map_err(|e| e.to_string())
    }

    pub fn launch(
        launcher: Option<PathBuf>,
        mask_hex: &str,
        steam: bool,
    ) -> Result<bdo_launch::LaunchMethod, bdo_launch::LaunchError> {
        let launcher = launcher
            .ok_or_else(|| bdo_launch::LaunchError::Os("no launcher path selected".into()))?;
        let mask = parse_mask_hex(mask_hex)?;
        bdo_launch::launch_with_affinity(&launcher, mask, steam)
    }

    pub fn verify(expected_hex: &str) -> VerifyOutcome {
        let expected = match parse_mask_hex(expected_hex) {
            Ok(m) => m,
            Err(e) => return VerifyOutcome::BadExpected(e.to_string()),
        };
        match bdo_launch::read_process_affinity(GAME_EXE) {
            Ok(None) => VerifyOutcome::NotRunning,
            Ok(Some(actual)) => {
                if actual == expected {
                    VerifyOutcome::Match { mask: actual }
                } else {
                    VerifyOutcome::Mismatch { actual, expected }
                }
            }
            Err(e) => VerifyOutcome::Error(e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_table_keeps_recommendation_first_and_filters_for_cpu() {
        let recommendation = bdo_hw::Recommendation {
            mask_hex: Some("555".to_string()),
            cores: vec![0, 2, 4, 6, 8, 10],
            alternates: vec!["554".to_string()],
            explanation: String::new(),
            topology_confirmed: None,
        };

        let rows = available_masks(&recommendation, 12);

        assert_eq!(rows.first().map(|row| row.0.as_str()), Some("555"));
        assert!(rows.first().is_some_and(|row| row.2));
        assert!(rows.iter().any(|row| row.0 == "554"));
        assert!(!rows.iter().any(|row| row.0 == "AAA0"));
        assert_eq!(rows.iter().filter(|row| row.0 == "555").count(), 1);
    }

    #[test]
    fn steps_reveal_one_at_a_time() {
        assert_eq!(revealed_steps(0.0, 4), 1);
        assert_eq!(revealed_steps(0.59, 4), 1);
        assert_eq!(revealed_steps(0.61, 4), 2);
        assert_eq!(revealed_steps(10.0, 4), 4);
        assert_eq!(revealed_steps(0.0, 0), 0);
    }

    #[cfg(windows)]
    #[test]
    fn automatic_verification_runs_once_per_second() {
        let now = std::time::Instant::now();

        assert!(verification_due(None, now));
        assert!(!verification_due(
            Some(now),
            now + std::time::Duration::from_millis(999)
        ));
        assert!(verification_due(
            Some(now),
            now + std::time::Duration::from_secs(1)
        ));
    }
}
