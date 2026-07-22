//! Optimize tab: game path, affinity mask entry + validation, shortcut / launch
//! actions, and read-only affinity verification.

use egui::{Color32, RichText};

use crate::app::App;
#[cfg(windows)]
use crate::app::VerifyOutcome;
use crate::format;

const OK_GREEN: Color32 = Color32::from_rgb(0x63, 0xd6, 0x88);
const WARN: Color32 = Color32::from_rgb(0xff, 0xc1, 0x07);
const ERR: Color32 = Color32::from_rgb(0xff, 0x6b, 0x6b);

impl App {
    pub(crate) fn optimize_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("Optimize");

        if self.detection.is_none() {
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Detecting hardware…");
            });
            return;
        }

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
    }

    // --------------------------------------------------------------- Game path
    fn game_path_section(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Game install").strong().size(16.0));

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
        ui.label(RichText::new("Affinity mask").strong().size(16.0));

        ui.horizontal(|ui| {
            ui.label("Mask (hex):");
            ui.add(
                egui::TextEdit::singleline(&mut self.optimize.mask_input)
                    .desired_width(120.0)
                    .font(egui::TextStyle::Monospace),
            );
            ui.checkbox(&mut self.optimize.steam, "Steam version (-steam)");
        });

        // Quick buttons: the recommended mask + alternates.
        let mut alternates = self.optimize.alternates.clone();
        if let Some(rec) = self
            .detection
            .as_ref()
            .and_then(|d| d.recommendation.mask_hex.clone())
        {
            if !alternates.contains(&rec) {
                alternates.insert(0, rec);
            }
        }
        if !alternates.is_empty() {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Quick set:").size(12.0));
                for alt in &alternates {
                    if ui.button(format!("0x{alt}")).clicked() {
                        self.optimize.mask_input = alt.clone();
                    }
                }
            });
        }

        // Live validation against the detected logical-core count.
        let logical = self.detection.as_ref().map(|d| d.cpu.logical_cores);
        match bdo_launch::parse_mask_hex(&self.optimize.mask_input) {
            Ok(mask) => {
                let cores = bdo_launch::mask_to_cores(mask);
                match bdo_launch::validate_mask(mask, logical) {
                    Ok(()) => {
                        ui.label(
                            RichText::new(format!("Selects cores: {}", format::cores(&cores)))
                                .color(OK_GREEN),
                        );
                    }
                    Err(e) => {
                        ui.label(
                            RichText::new(format!(
                                "Selects cores: {} — but {}",
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
        ui.label(RichText::new("Apply").strong().size(16.0));

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

    // ------------------------------------------------------------------ Verify
    fn verify_section(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Verify running game").strong().size(16.0));
        ui.label(
            RichText::new(
                "Read-only check: the app never writes to the running game. Affinity is inherited \
                 at launch, so this only reads back what took effect.",
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
            let expected = self.optimize.mask_input.clone();
            if ui.button("Verify").clicked() {
                self.optimize.verify = Some(win_actions::verify(&expected));
            }

            if let Some(outcome) = &self.optimize.verify {
                match outcome {
                    VerifyOutcome::NotRunning => {
                        ui.label(RichText::new("Game is not running.").weak());
                    }
                    VerifyOutcome::Match { mask } => {
                        ui.label(
                            RichText::new(format!(
                                "✔ Match — running mask 0x{mask:x} equals the expected mask.",
                            ))
                            .color(OK_GREEN),
                        );
                    }
                    VerifyOutcome::Mismatch { actual, expected } => {
                        ui.label(
                            RichText::new(format!(
                                "⚠ Mismatch — running mask 0x{actual:x}, expected 0x{expected:x}.",
                            ))
                            .color(WARN),
                        );
                    }
                    VerifyOutcome::BadExpected(e) => {
                        ui.label(RichText::new(format!("Expected mask invalid: {e}")).color(ERR));
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
