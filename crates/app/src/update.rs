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

use std::path::{Path, PathBuf};
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
    /// Direct download URL of that zip's `.sha256` companion, published by the
    /// release workflow. The download is checked against it before anything is
    /// unpacked.
    pub sha_url: String,
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
                            let tx = spawn_apply(info.zip_url, info.sha_url, ui.ctx().clone());
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

/// `Command` for a Windows-supplied tool, resolved to its absolute path in
/// `System32` and never flashing a console window (the app itself is windowless).
///
/// The absolute path is the security-relevant part. A bare `"curl.exe"` is
/// resolved through the executable search path, which begins with the process's
/// current directory — for an elevated relaunch that is the app's own folder,
/// which for a portable install is user-writable. A same-user process could
/// drop a `curl.exe` there and have it run with our administrator token, and the
/// startup version check invokes it with no user action at all.
fn hidden(program: &str) -> Command {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut cmd = Command::new(system32(program));
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd
}

/// Absolute path to a tool inside the real Windows system directory.
///
/// Falls back to the bare name only if the system directory cannot be resolved,
/// which does not happen on a working Windows install.
fn system32(program: &str) -> std::path::PathBuf {
    use windows::Win32::System::SystemInformation::GetSystemDirectoryW;

    let mut buffer = [0u16; 260];
    // SAFETY: `buffer` is a valid, correctly-sized wide buffer for the call.
    let len = unsafe { GetSystemDirectoryW(Some(&mut buffer)) } as usize;
    if len == 0 || len >= buffer.len() {
        return std::path::PathBuf::from(program);
    }
    use std::os::windows::ffi::OsStringExt;
    std::path::PathBuf::from(std::ffi::OsString::from_wide(&buffer[..len])).join(program)
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
    let asset_url = |suffix: &str| -> Option<String> {
        json["assets"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|a| a["name"].as_str().is_some_and(|n| n.ends_with(suffix)))
            .and_then(|a| a["browser_download_url"].as_str())
            .map(str::to_string)
    };
    let zip_url = asset_url("windows-x64.zip").ok_or("release has no windows-x64.zip asset")?;
    // Fail closed: the release workflow always publishes the checksum beside the
    // zip, so its absence means this is not a release we can verify, and an
    // unverified download is one we refuse to unpack and run.
    let sha_url = asset_url("windows-x64.zip.sha256")
        .ok_or("release has no published SHA-256 to verify the download against")?;
    Ok(UpdateMsg::Available(ReleaseInfo {
        tag: tag.to_string(),
        zip_url,
        sha_url,
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
fn spawn_apply(zip_url: String, sha_url: String, ctx: egui::Context) -> Receiver<UpdateMsg> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        if let Err(e) = apply(&zip_url, &sha_url, &tx, &ctx) {
            let _ = tx.send(UpdateMsg::Error(e));
            ctx.request_repaint();
        }
    });
    rx
}

fn apply(
    zip_url: &str,
    sha_url: &str,
    tx: &Sender<UpdateMsg>,
    ctx: &egui::Context,
) -> Result<(), String> {
    let progress = |step| {
        let _ = tx.send(UpdateMsg::Progress(step));
        ctx.request_repaint();
    };

    let exe = std::env::current_exe().map_err(|e| format!("could not resolve current exe: {e}"))?;
    let app_dir = exe
        .parent()
        .ok_or("current exe has no parent directory")?
        .to_path_buf();

    // Stage into a directory this process exclusively owns. `create_dir` is
    // atomic and fails when the name is taken, so winning it proves nobody
    // pre-positioned the path — unlike `create_dir_all`, which happily accepts a
    // directory an attacker created first. That matters because everything
    // staged here is later copied next to the (elevated) executable and run.
    let tmp = create_private_dir()?;

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

    progress("Verifying…");
    let sha = tmp.join("update.zip.sha256");
    run(hidden("curl.exe").args([
        "-fSL",
        "--max-time",
        "60",
        "-o",
        sha.to_str().ok_or("temp path is not valid UTF-8")?,
        sha_url,
    ]))?;
    verify_checksum(&zip, &sha)?;

    progress("Extracting…");
    // Windows' tar.exe is bsdtar, which reads zip archives natively.
    run(hidden("tar.exe").args([
        "-xf",
        zip.to_str().ok_or("temp path is not valid UTF-8")?,
        "-C",
        tmp.to_str().ok_or("temp path is not valid UTF-8")?,
    ]))?;

    // The zip contains a single versioned folder with all the files in it.
    // Requiring *exactly* one is the point: picking "the first directory" would
    // let anything else that appeared here be installed instead.
    let dirs: Vec<PathBuf> = std::fs::read_dir(&tmp)
        .map_err(|e| format!("could not read temp dir: {e}"))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    let inner = match dirs.as_slice() {
        [only] => only.clone(),
        [] => return Err("no folder inside the downloaded zip".to_string()),
        many => {
            return Err(format!(
                "the downloaded zip unpacked to {} folders, so the release could not be \
                 identified — nothing was installed",
                many.len()
            ))
        }
    };

    progress("Installing…");
    let staged = staged_files(&inner)?;
    install_all(&staged, &app_dir)?;
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

/// Check the downloaded zip against the SHA-256 the release workflow published
/// beside it, before a single byte of it is unpacked or executed.
///
/// This is what makes a truncated, corrupted, or substituted download fail
/// loudly instead of being installed and launched. It is not a signature: the
/// checksum comes from the same release, so it does not defend against the
/// release itself being malicious — code signing would. It does close every
/// failure between GitHub and this disk.
fn verify_checksum(zip: &Path, sha_file: &Path) -> Result<(), String> {
    use sha2::Digest;

    let published = std::fs::read_to_string(sha_file)
        .map_err(|e| format!("could not read the published checksum: {e}"))?;
    // The file is `<hex>  <name>`; take the first field and normalise case.
    let expected = published
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if expected.len() != 64 || !expected.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("the published checksum is not a SHA-256 — refusing to install".to_string());
    }

    let mut file = std::fs::File::open(zip)
        .map_err(|e| format!("could not reopen the download to verify it: {e}"))?;
    let mut hash = sha2::Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        match std::io::Read::read(&mut file, &mut buffer) {
            Ok(0) => break,
            Ok(read) => hash.update(&buffer[..read]),
            Err(e) => return Err(format!("could not read the download to verify it: {e}")),
        }
    }
    let actual = format!("{:x}", hash.finalize());
    if actual != expected {
        return Err(format!(
            "the download does not match the checksum published for this release \
             (expected {expected}, got {actual}) — nothing was installed"
        ));
    }
    Ok(())
}

/// Create a fresh, exclusively-owned staging directory under the temp dir.
///
/// `create_dir` is atomic and fails when the path exists, so the first name that
/// succeeds is one nobody else holds. Same contract as the NVIDIA job directory
/// in `video::worker`.
fn create_private_dir() -> Result<PathBuf, String> {
    let base = std::env::temp_dir();
    let pid = std::process::id();
    for attempt in 0..64u32 {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(attempt);
        let dir = base.join(format!("bdo-optimizer-update-{pid}-{nanos}-{attempt}"));
        match std::fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("could not create {}: {e}", dir.display())),
        }
    }
    Err("could not create a private staging directory".to_string())
}

/// Every regular file in the extracted release folder, checked to be readable
/// **before** anything in the install directory is touched.
///
/// Reading each one up front is what makes the swap below worth attempting: a
/// truncated download or a file the antivirus has locked fails here, while the
/// installed copy is still entirely intact.
fn staged_files(inner: &Path) -> Result<Vec<(std::ffi::OsString, PathBuf)>, String> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(inner)
        .map_err(|e| format!("could not read extracted folder: {e}"))?
        .flatten()
    {
        let src = entry.path();
        if !src.is_file() {
            continue;
        }
        std::fs::File::open(&src)
            .map_err(|e| format!("the downloaded {} could not be read: {e}", src.display()))?;
        files.push((entry.file_name(), src));
    }
    if files.is_empty() {
        return Err("the downloaded release contains no files".to_string());
    }
    Ok(files)
}

/// Swap every staged file into `app_dir`, putting everything back if any one
/// of them fails.
///
/// Without the rollback, a copy that fails part-way (disk full, antivirus lock,
/// permissions) leaves a mix of two versions behind — or, if the executable was
/// the one already renamed aside, no runnable app at all and no way back.
fn install_all(staged: &[(std::ffi::OsString, PathBuf)], app_dir: &Path) -> Result<(), String> {
    // (renamed-aside original, its real name) for undo, plus files we created
    // where nothing existed before.
    let mut displaced: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut created: Vec<PathBuf> = Vec::new();

    let rollback = |displaced: &[(PathBuf, PathBuf)], created: &[PathBuf]| {
        for path in created {
            let _ = std::fs::remove_file(path);
        }
        for (old, dest) in displaced {
            let _ = std::fs::remove_file(dest);
            let _ = std::fs::rename(old, dest);
        }
    };

    for (name, src) in staged {
        let dest = app_dir.join(name);
        let existed = dest.exists();
        if existed {
            // Renaming works even for the running exe; deleting would not.
            let old = app_dir.join(format!("{}.old", name.to_string_lossy()));
            let _ = std::fs::remove_file(&old);
            if let Err(e) = std::fs::rename(&dest, &old) {
                rollback(&displaced, &created);
                return Err(format!(
                    "could not replace {}: {e} — the previous version was left in place",
                    name.to_string_lossy()
                ));
            }
            displaced.push((old, dest.clone()));
        }
        // Copy, not rename: the temp dir can be on a different drive.
        if let Err(e) = std::fs::copy(src, &dest) {
            rollback(&displaced, &created);
            return Err(format!(
                "could not install {}: {e} — the previous version was put back",
                name.to_string_lossy()
            ));
        }
        if !existed {
            created.push(dest);
        }
    }
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

    /// A download that does not match the published checksum must never reach
    /// the install step, and the real CI format (`<hex>  <name>`) must parse.
    #[test]
    fn checksum_gate_accepts_the_real_format_and_rejects_tampering() {
        let dir = std::env::temp_dir().join(format!("bdo-update-sha-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let zip = dir.join("update.zip");
        let sha = dir.join("update.zip.sha256");
        std::fs::write(&zip, b"release bytes").unwrap();

        // Exactly what the release workflow writes: hash, two spaces, file name.
        let good = "e4c8b1f31e1a4f2e0d0a5c8a3b1d9e77a0f9c4b6d2e8a1c3f5079b2d4e6a8c10";
        let real = {
            use sha2::Digest;
            format!("{:x}", sha2::Sha256::digest(b"release bytes"))
        };
        std::fs::write(&sha, format!("{real}  bdo-optimizer-v9.9.9-windows-x64.zip\n")).unwrap();
        assert!(verify_checksum(&zip, &sha).is_ok());

        // A zip swapped after the checksum was published must be refused.
        std::fs::write(&sha, format!("{good}  x.zip\n")).unwrap();
        assert!(verify_checksum(&zip, &sha).is_err());

        // Garbage where a hash should be must be refused, not ignored.
        std::fs::write(&sha, "not-a-hash  x.zip\n").unwrap();
        assert!(verify_checksum(&zip, &sha).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A failed install must put every displaced file back rather than leave a
    /// half-swapped folder — or, worse, no executable at all.
    #[test]
    fn a_failed_install_restores_every_displaced_file() {
        let base = std::env::temp_dir().join(format!("bdo-update-rollback-{}", std::process::id()));
        let app = base.join("app");
        let stage = base.join("stage");
        std::fs::create_dir_all(&app).unwrap();
        std::fs::create_dir_all(&stage).unwrap();

        std::fs::write(app.join("bdo-optimizer.exe"), b"old exe").unwrap();
        std::fs::write(app.join("PresentMon.exe"), b"old presentmon").unwrap();

        std::fs::write(stage.join("bdo-optimizer.exe"), b"new exe").unwrap();
        // Second entry is a *directory* at the source path, so the copy fails
        // after the first file has already been swapped.
        std::fs::create_dir(stage.join("PresentMon.exe")).unwrap();

        let staged = vec![
            (
                std::ffi::OsString::from("bdo-optimizer.exe"),
                stage.join("bdo-optimizer.exe"),
            ),
            (
                std::ffi::OsString::from("PresentMon.exe"),
                stage.join("PresentMon.exe"),
            ),
        ];
        assert!(install_all(&staged, &app).is_err());

        assert_eq!(
            std::fs::read(app.join("bdo-optimizer.exe")).unwrap(),
            b"old exe",
            "the already-swapped executable must be rolled back"
        );
        assert_eq!(
            std::fs::read(app.join("PresentMon.exe")).unwrap(),
            b"old presentmon"
        );

        let _ = std::fs::remove_dir_all(&base);
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
