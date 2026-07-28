//! Optimize tab: game path, affinity mask entry + validation, shortcut / launch
//! actions, and read-only affinity verification.

use egui::{Color32, RichText};

use crate::app::{App, Tab, VerifyOutcome};
use crate::format;

const OK_GREEN: Color32 = Color32::from_rgb(0x63, 0xd6, 0x88);
const WARN: Color32 = Color32::from_rgb(0xff, 0xc1, 0x07);
const ERR: Color32 = Color32::from_rgb(0xff, 0x6b, 0x6b);

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
        ui.heading("Apply safely");
        ui.label(
            RichText::new("Use the detected recommendation, verify it in-game, and return to normal launch at any time.")
                .size(13.0)
                .weak(),
        );

        if self.detection.is_none() {
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Detecting hardware…");
            });
            return;
        }

        self.optimization_status(ui);
        ui.add_space(10.0);
        self.game_path_section(ui);
        ui.add_space(10.0);
        ui.separator();
        self.mask_section(ui);
        ui.add_space(10.0);
        ui.separator();
        self.actions_section(ui);
        ui.add_space(10.0);
        ui.separator();
        self.verify_section(ui);
        ui.add_space(10.0);
        ui.separator();
        self.nvidia_section(ui);
        ui.add_space(10.0);
        ui.separator();
        self.gameconfig_section(ui);
        if matches!(&self.optimize.verify, Some(VerifyOutcome::Match { .. }))
            && ui.button("Continue to Measure").clicked()
        {
            self.tab = Tab::Benchmark;
        }
    }

    fn optimization_status(&self, ui: &mut egui::Ui) {
        let (title, detail, color) = match &self.optimize.verify {
            Some(VerifyOutcome::Match { .. }) => (
                "VERIFIED",
                "The running game matches the selected affinity mask.",
                OK_GREEN,
            ),
            Some(VerifyOutcome::Mismatch { .. })
            | Some(VerifyOutcome::BadExpected(_))
            | Some(VerifyOutcome::Error(_)) => (
                "NEEDS ATTENTION",
                "The selected change is not verified. Review the details below before measuring.",
                WARN,
            ),
            _ => (
                "PENDING",
                "Launch with the optimized shortcut or Launch Now; verification starts when BDO opens.",
                Color32::from_rgb(104, 200, 255),
            ),
        };

        egui::Frame::new()
            .fill(Color32::from_rgb(16, 25, 39))
            .stroke(egui::Stroke::new(1.0, color))
            .corner_radius(8)
            .inner_margin(egui::Margin::same(14))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(title).monospace().strong().color(color));
                    ui.label(detail);
                });
                ui.label(
                    RichText::new(
                        "Rollback: close the game and launch BDO normally, or delete the optimized shortcut. No global driver or system settings are changed.",
                    )
                    .size(12.0)
                    .weak(),
                );
            });
    }

    // --------------------------------------------------------------- Game path
    fn game_path_section(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Find your BDO install").strong().size(16.0));

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
        ui.label(
            RichText::new("Review the affinity mask")
                .strong()
                .size(16.0),
        );

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
        ui.label(RichText::new("Apply for this launch").strong().size(16.0));

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
            let has_launcher = launcher.as_ref().map(|p| p.exists()).unwrap_or(false);

            ui.horizontal(|ui| {
                if ui
                    .add_enabled(has_launcher, egui::Button::new("Create Optimized Shortcut"))
                    .clicked()
                {
                    self.optimize.shortcut_result = Some(win_actions::create_shortcut(
                        launcher.clone(),
                        &mask_hex,
                        steam,
                    ));
                }
                if ui
                    .add_enabled(has_launcher, egui::Button::new("Launch Now with Affinity"))
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
            RichText::new("NVIDIA driver profile (guide settings)")
                .strong()
                .size(16.0),
        );
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
        ui.checkbox(
            &mut self.video.ull,
            "Also enable Ultra Low Latency (guide: test whether your CPU handles the overhead)",
        );

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
                return;
            }

            let finished = self
                .video
                .worker
                .as_ref()
                .and_then(|worker| worker.rx.try_recv().ok());
            if let Some(result) = finished {
                self.video.last = Some(result);
                self.video.worker = None;
            }
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
                            self.video.last = None;
                            self.video.worker = Some(crate::video::worker::start(
                                exe.clone(),
                                crate::video::guide_settings(physical_cores, self.video.ull),
                                "apply",
                            ));
                        }
                        if ui
                            .add_enabled(!busy, egui::Button::new("Restore driver defaults"))
                            .clicked()
                        {
                            self.video.last = None;
                            self.video.worker = Some(crate::video::worker::start(
                                exe,
                                crate::video::default_settings(),
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
            RichText::new("Game config files (guide settings)")
                .strong()
                .size(16.0),
        );
        ui.label(
            RichText::new(
                "Sets PostFilter (forced sharpening) and Tessellation to 0 in GameOption.txt and \
                 every UserCache gamevariable.xml, exactly as the guide describes. The original \
                 of each file is backed up next to it the first time it is changed.",
            )
            .size(12.0)
            .weak(),
        );

        let Some(root) = crate::gameconfig::config_root() else {
            ui.label(RichText::new("Documents folder not found.").color(WARN));
            return;
        };
        let files = crate::gameconfig::find_config_files(&root);
        if files.is_empty() {
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
                files.len(),
                root.display()
            ))
            .size(12.0)
            .weak(),
        );

        ui.horizontal(|ui| {
            if ui.button("Set PostFilter & Tessellation to 0").clicked() {
                self.run_gameconfig(&files, "apply");
            }
            if ui.button("Restore from backup").clicked() {
                self.run_gameconfig(&files, "restore");
            }
        });

        if let Some(msg) = &self.gameconfig.blocked {
            ui.label(RichText::new(msg.as_str()).color(WARN).strong());
        }
        if let Some(action) = self.gameconfig.last_action {
            for outcome in &self.gameconfig.outcomes {
                let name = outcome
                    .path
                    .strip_prefix(&root)
                    .unwrap_or(&outcome.path)
                    .display();
                match (&outcome.result, action) {
                    (Ok(0), "apply") => {
                        ui.label(RichText::new(format!("• {name}: already optimized")).size(12.0))
                    }
                    (Ok(n), "apply") => ui.label(
                        RichText::new(format!("✔ {name}: {n} value(s) set to guide defaults"))
                            .color(OK_GREEN)
                            .size(12.0),
                    ),
                    (Ok(0), _) => ui
                        .label(RichText::new(format!("• {name}: no backup to restore")).size(12.0)),
                    (Ok(_), _) => ui.label(
                        RichText::new(format!("✔ {name}: restored original"))
                            .color(OK_GREEN)
                            .size(12.0),
                    ),
                    (Err(e), _) => ui.label(
                        RichText::new(format!("✘ {name}: {e}"))
                            .color(ERR)
                            .size(12.0),
                    ),
                };
            }
        }
    }

    /// Run a config-file action, refusing while BDO is running (the game
    /// rewrites these files on exit, so an edit would be lost or race).
    fn run_gameconfig(&mut self, files: &[std::path::PathBuf], action: &'static str) {
        self.gameconfig.blocked = None;
        if bdo_bench::is_process_running(bdo_launch::GAME_EXE).is_some() {
            self.gameconfig.blocked = Some(
                "BDO is running — close the game first. It rewrites these files on exit, \
                 which would undo the change."
                    .to_string(),
            );
            return;
        }
        self.gameconfig.outcomes = if action == "apply" {
            crate::gameconfig::apply_files(files)
        } else {
            crate::gameconfig::restore_files(files)
        };
        self.gameconfig.last_action = Some(action);
    }

    // ------------------------------------------------------------------ Verify
    fn verify_section(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Automatic verification").strong().size(16.0));
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
