//! In-app updater (Windows only).
//!
//! On startup a background thread asks the GitHub Releases API whether a newer
//! tag exists. If so, the sidebar shows an "Update to vX.Y.Z" button; clicking
//! it downloads the release zip, extracts it, swaps every bundled file next to
//! the exe (the running exe is renamed to `.old` first — Windows allows that),
//! and restarts the app. Leftover `.old` files are deleted on the next start.
//!
//! Downloading and unzipping use the `curl.exe` and `tar.exe` that ship with
//! Windows 10+, so no HTTP or archive crates are needed. Users who prefer the
//! GitHub releases page can keep downloading from there — this is additive.

use std::path::Path;
use std::process::Command;
use std::sync::mpsc::{channel, Receiver, Sender};

use crate::app::App;
use crate::theme;

const REPO: &str = "CarlosJunioor/bdo-optimizer";
const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// A newer release found on GitHub.
pub struct ReleaseInfo {
    /// The release tag, e.g. `v0.5.0`.
    pub tag: String,
    /// Direct download URL of the `*-windows-x64.zip` asset.
    pub zip_url: String,
}

/// Messages from the check / apply worker threads.
pub enum UpdateMsg {
    UpToDate,
    Available(ReleaseInfo),
    Progress(&'static str),
    /// The new exe has been spawned; the receiver should exit the process.
    Restarting,
    Error(String),
}

/// Updater state machine surfaced in the sidebar.
pub enum UpdateStatus {
    Checking,
    UpToDate,
    Available(ReleaseInfo),
    Working(&'static str),
    Error(String),
}

pub struct UpdateState {
    pub rx: Receiver<UpdateMsg>,
    pub status: UpdateStatus,
}

impl UpdateState {
    /// Delete `.old` leftovers from a previous update, then start the
    /// background version check.
    pub fn new(ctx: egui::Context) -> Self {
        cleanup_old_files();
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let msg = match check_latest() {
                Ok(msg) => msg,
                Err(e) => UpdateMsg::Error(e),
            };
            let _ = tx.send(msg);
            ctx.request_repaint();
        });
        Self {
            rx,
            status: UpdateStatus::Checking,
        }
    }
}

impl App {
    /// Drain worker messages into the status machine. Exits the process once
    /// the freshly installed exe has been spawned.
    pub fn poll_update(&mut self) {
        while let Ok(msg) = self.update.rx.try_recv() {
            self.update.status = match msg {
                UpdateMsg::UpToDate => UpdateStatus::UpToDate,
                UpdateMsg::Available(info) => UpdateStatus::Available(info),
                UpdateMsg::Progress(step) => UpdateStatus::Working(step),
                UpdateMsg::Restarting => std::process::exit(0),
                UpdateMsg::Error(e) => UpdateStatus::Error(e),
            };
        }
    }

    /// Sidebar row: update button / progress / result.
    pub fn update_row(&mut self, ui: &mut egui::Ui) {
        egui::Frame::new()
            .inner_margin(egui::Margin::symmetric(10, 6))
            .show(ui, |ui| match &self.update.status {
                UpdateStatus::Checking => {}
                UpdateStatus::UpToDate => {
                    ui.label(
                        egui::RichText::new(format!(
                            "{} Latest version",
                            egui_phosphor::regular::CHECK
                        ))
                        .size(11.0)
                        .color(theme::INK_3),
                    );
                }
                UpdateStatus::Available(info) => {
                    let text = egui::RichText::new(format!(
                        "{} Update to {}",
                        egui_phosphor::regular::DOWNLOAD_SIMPLE,
                        info.tag
                    ))
                    .size(13.0)
                    .color(theme::BG);
                    let clicked = ui
                        .add_sized(
                            [ui.available_width(), 30.0],
                            egui::Button::new(text)
                                .fill(theme::ACCENT)
                                .corner_radius(egui::CornerRadius::same(8)),
                        )
                        .on_hover_text("Downloads the new version and restarts the app")
                        .clicked();
                    if clicked {
                        if let UpdateStatus::Available(info) =
                            std::mem::replace(&mut self.update.status, UpdateStatus::Checking)
                        {
                            let tx = spawn_apply(info.zip_url, ui.ctx().clone());
                            self.update = UpdateState {
                                rx: tx,
                                status: UpdateStatus::Working("Downloading…"),
                            };
                        }
                    }
                }
                UpdateStatus::Working(step) => {
                    ui.label(
                        egui::RichText::new(format!("{} {step}", egui_phosphor::regular::SPINNER))
                            .size(12.0)
                            .color(theme::ACCENT),
                    );
                }
                UpdateStatus::Error(e) => {
                    ui.label(
                        egui::RichText::new(format!("Update failed: {e}"))
                            .size(11.0)
                            .color(theme::ERR),
                    )
                    .on_hover_text(
                        "You can always download the latest version manually from the \
                         GitHub releases page.",
                    );
                }
            });
    }
}

/// `Command` that never flashes a console window (the app itself is windowless).
fn hidden(program: &str) -> Command {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut cmd = Command::new(program);
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd
}

/// Ask the GitHub API for the latest release and compare tags.
fn check_latest() -> Result<UpdateMsg, String> {
    let out = hidden("curl.exe")
        .args([
            "-fsSL",
            "--max-time",
            "30",
            &format!("https://api.github.com/repos/{REPO}/releases/latest"),
        ])
        .output()
        .map_err(|e| format!("could not run curl: {e}"))?;
    if !out.status.success() {
        return Err("could not reach GitHub (offline?)".to_string());
    }
    let json: serde_json::Value =
        serde_json::from_slice(&out.stdout).map_err(|e| format!("bad API response: {e}"))?;
    let tag = json["tag_name"]
        .as_str()
        .ok_or("no tag_name in API response")?;
    if !is_newer(tag, CURRENT_VERSION) {
        return Ok(UpdateMsg::UpToDate);
    }
    let zip_url = json["assets"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|a| {
            a["name"]
                .as_str()
                .is_some_and(|n| n.ends_with("windows-x64.zip"))
        })
        .and_then(|a| a["browser_download_url"].as_str())
        .ok_or("release has no windows-x64.zip asset")?;
    Ok(UpdateMsg::Available(ReleaseInfo {
        tag: tag.to_string(),
        zip_url: zip_url.to_string(),
    }))
}

/// `v0.5.0`-style tag strictly newer than the running `x.y.z` version?
/// Unparseable versions compare as not-newer, so a malformed tag can never
/// trigger an update.
fn is_newer(tag: &str, current: &str) -> bool {
    match (parse_version(tag), parse_version(current)) {
        (Some(t), Some(c)) => t > c,
        _ => false,
    }
}

fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let mut parts = v.trim().trim_start_matches('v').split('.');
    let mut next = || parts.next()?.parse::<u64>().ok();
    let out = (next()?, next()?, next()?);
    parts.next().is_none().then_some(out)
}

/// Run the download-extract-swap-restart pipeline on a worker thread.
fn spawn_apply(zip_url: String, ctx: egui::Context) -> Receiver<UpdateMsg> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        if let Err(e) = apply(&zip_url, &tx, &ctx) {
            let _ = tx.send(UpdateMsg::Error(e));
            ctx.request_repaint();
        }
    });
    rx
}

fn apply(zip_url: &str, tx: &Sender<UpdateMsg>, ctx: &egui::Context) -> Result<(), String> {
    let progress = |step| {
        let _ = tx.send(UpdateMsg::Progress(step));
        ctx.request_repaint();
    };

    let exe = std::env::current_exe().map_err(|e| format!("could not resolve current exe: {e}"))?;
    let app_dir = exe
        .parent()
        .ok_or("current exe has no parent directory")?
        .to_path_buf();

    let tmp = std::env::temp_dir().join(format!("bdo-optimizer-update-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).map_err(|e| format!("could not create temp dir: {e}"))?;

    progress("Downloading…");
    let zip = tmp.join("update.zip");
    run(hidden("curl.exe").args([
        "-fSL",
        "--max-time",
        "600",
        "-o",
        zip.to_str().ok_or("temp path is not valid UTF-8")?,
        zip_url,
    ]))?;

    progress("Extracting…");
    // Windows' tar.exe is bsdtar, which reads zip archives natively.
    run(hidden("tar.exe").args([
        "-xf",
        zip.to_str().ok_or("temp path is not valid UTF-8")?,
        "-C",
        tmp.to_str().ok_or("temp path is not valid UTF-8")?,
    ]))?;

    // The zip contains a single versioned folder with all the files in it.
    let inner = std::fs::read_dir(&tmp)
        .map_err(|e| format!("could not read temp dir: {e}"))?
        .flatten()
        .map(|e| e.path())
        .find(|p| p.is_dir())
        .ok_or("no folder inside the downloaded zip")?;

    progress("Installing…");
    for entry in std::fs::read_dir(&inner)
        .map_err(|e| format!("could not read extracted folder: {e}"))?
        .flatten()
    {
        let src = entry.path();
        if !src.is_file() {
            continue;
        }
        let name = entry.file_name();
        let dest = app_dir.join(&name);
        if dest.exists() {
            // Renaming works even for the running exe; deleting would not.
            let old = app_dir.join(format!("{}.old", name.to_string_lossy()));
            let _ = std::fs::remove_file(&old);
            std::fs::rename(&dest, &old)
                .map_err(|e| format!("could not replace {}: {e}", name.to_string_lossy()))?;
        }
        // Copy, not rename: the temp dir can be on a different drive.
        std::fs::copy(&src, &dest)
            .map_err(|e| format!("could not install {}: {e}", name.to_string_lossy()))?;
    }
    let _ = std::fs::remove_dir_all(&tmp);

    progress("Restarting…");
    Command::new(&exe)
        .current_dir(&app_dir)
        .spawn()
        .map_err(|e| format!("update installed, but restart failed: {e}"))?;
    let _ = tx.send(UpdateMsg::Restarting);
    ctx.request_repaint();
    Ok(())
}

/// Wait for a spawned tool and turn a non-zero exit into an error message.
fn run(cmd: &mut Command) -> Result<(), String> {
    let program = cmd.get_program().to_string_lossy().to_string();
    let out = cmd
        .output()
        .map_err(|e| format!("could not run {program}: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    Err(format!(
        "{program} failed: {}",
        stderr.lines().last().unwrap_or("unknown error").trim()
    ))
}

/// Remove `*.old` files a previous update left next to the exe (best-effort:
/// the old exe may still be held open briefly by the exiting instance).
fn cleanup_old_files() {
    let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(Path::to_path_buf))
    else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for path in entries.flatten().map(|e| e.path()) {
        if path.extension().is_some_and(|ext| ext == "old") {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_comparison() {
        assert!(is_newer("v0.5.0", "0.4.0"));
        assert!(is_newer("v0.4.1", "0.4.0"));
        assert!(is_newer("v1.0.0", "0.9.9"));
        assert!(!is_newer("v0.4.0", "0.4.0"));
        assert!(!is_newer("v0.3.9", "0.4.0"));
        assert!(!is_newer("garbage", "0.4.0"));
        assert!(!is_newer("v0.4", "0.4.0"));
        assert!(!is_newer("v0.4.0.1", "0.4.0"));
    }

    #[test]
    fn old_file_cleanup_only_touches_old_extension() {
        let dir = std::env::temp_dir().join(format!("bdo-update-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let keep = dir.join("bdo-optimizer.exe");
        let old = dir.join("bdo-optimizer.exe.old");
        std::fs::write(&keep, b"new").unwrap();
        std::fs::write(&old, b"old").unwrap();
        // cleanup_old_files works on the current exe's dir, so exercise the
        // same filter logic directly.
        for path in std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.path()) {
            if path.extension().is_some_and(|ext| ext == "old") {
                std::fs::remove_file(path).unwrap();
            }
        }
        assert!(keep.exists());
        assert!(!old.exists());
        std::fs::remove_file(keep).unwrap();
        std::fs::remove_dir(dir).unwrap();
    }
}
