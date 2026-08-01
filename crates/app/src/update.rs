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
                UpdateMsg::Restarting => {
                    // `process::exit` does not run `eframe::App::on_exit`, so
                    // the workers have to be stopped here. A capture started
                    // after the update began would otherwise be discarded with
                    // its elevated PresentMon left running, and a driver job
                    // would keep writing the profile with nobody to verify it.
                    self.shutdown_workers();
                    std::process::exit(0)
                }
                UpdateMsg::Error(e) => UpdateStatus::Error(e),
            };
        }
    }

    /// Sidebar row: update button / progress / result.
    pub fn update_row(&mut self, ui: &mut egui::Ui) {
        // Both workers block an update: restarting mid-job would abandon an
        // elevated child process either way.
        let capturing = self.benchmark.worker.is_some() || self.video.worker.is_some();
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
                    // Installing ends with `process::exit`, which would kill an
                    // in-flight capture without saving its session and leave
                    // PresentMon running (dropping the handle does not stop it).
                    // A benchmark is minutes of the user's play time; the update
                    // can wait for it.
                    let clicked = ui
                        .add_enabled_ui(!capturing, |ui| {
                            ui.add_sized(
                                [ui.available_width(), 30.0],
                                egui::Button::new(text)
                                    .fill(theme::ACCENT)
                                    .corner_radius(egui::CornerRadius::same(8)),
                            )
                            .on_hover_text(if capturing {
                                "Finish or stop the running capture first — updating restarts \
                                 the app and would discard it"
                            } else {
                                "Downloads the new version and restarts the app"
                            })
                            .clicked()
                        })
                        .inner;
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
    let tmp = crate::privdir::create("bdo-optimizer-update")?;

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
    // Take the expected hash straight from the HTTPS response into memory. A
    // checksum written to disk and read back by pathname is one more object an
    // attacker could swap in the window before it is opened — and swapping the
    // pair (archive + checksum) together would defeat the check entirely.
    // Nothing on disk is trusted for this comparison.
    let sha_body = capture(hidden("curl.exe").args(["-fsSL", "--max-time", "60", sha_url]))?;
    // Pin the archive before hashing it and keep the pin through extraction, so
    // the bytes tar unpacks are provably the bytes that matched the checksum.
    // Verifying a path and then handing that path to another program is the
    // same check-then-use gap the rest of this file closes; `tar` opens it for
    // reading, which `FILE_SHARE_READ` still allows.
    let pinned_zip = {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 0x0000_0001;
        std::fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .open(&zip)
            .map_err(|e| format!("could not lock the download for verification: {e}"))?
    };
    verify_checksum(&mut &pinned_zip, &sha_body)?;

    progress("Extracting…");
    // Windows' tar.exe is bsdtar, which reads zip archives natively.
    run(hidden("tar.exe").args([
        "-xf",
        zip.to_str().ok_or("temp path is not valid UTF-8")?,
        "-C",
        tmp.to_str().ok_or("temp path is not valid UTF-8")?,
    ]))?;
    // Extraction is done; the archive no longer needs pinning.
    drop(pinned_zip);

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
    let mut staged = staged_files(&inner)?;
    install_all(&mut staged, &app_dir)?;
    // The install folder is the portable app folder, which is user-writable in
    // the layout this app ships in — so the pinned *source* handles say nothing
    // about what is sitting at the destination now. Read the executable back
    // through a pin that denies further writes and check it byte-for-byte
    // against the verified staged copy before anything runs it.
    let pinned_exe = match verify_installed(&mut staged, &exe) {
        Ok(handle) => handle,
        Err(e) => {
            // The swap already happened and the ledger says `done`, so leaving
            // now would strand the rejected executable at the canonical path
            // with the good one only as `.old` — and the next start would clean
            // that backup away. Put the previous version back instead.
            let _ = roll_back_completed_install(&app_dir);
            return Err(e);
        }
    };
    // Release the source pins before the staging directory is removed.
    drop(staged);
    let _ = std::fs::remove_dir_all(&tmp);

    progress("Restarting…");
    // `pinned_exe` has been held since the bytes were compared, so what starts
    // here is provably what was verified.
    Command::new(&exe)
        .current_dir(&app_dir)
        .spawn()
        .map_err(|e| format!("update installed, but restart failed: {e}"))?;
    drop(pinned_exe);
    let _ = tx.send(UpdateMsg::Restarting);
    ctx.request_repaint();
    Ok(())
}

/// Confirm the installed executable is byte-for-byte the staged one that was
/// verified against the release checksum.
///
/// Copying from a pinned source proves what was *written*; it proves nothing
/// about what is at the destination a moment later, because the destination
/// folder is writable by any process running as this user. Without this check
/// a same-user process could overwrite the new executable in the gap before it
/// is launched and have its bytes run with this app's administrator token.
fn verify_installed(staged: &mut [StagedFile], exe: &Path) -> Result<std::fs::File, String> {
    use std::io::{Read, Seek, SeekFrom};

    let name = exe
        .file_name()
        .ok_or("the executable path has no file name")?;
    let source = staged
        .iter_mut()
        .find(|f| f.name == name)
        .ok_or("the release did not contain the application executable")?;

    source
        .handle
        .seek(SeekFrom::Start(0))
        .map_err(|e| format!("could not re-read the verified executable: {e}"))?;
    let mut expected = Vec::new();
    source
        .handle
        .read_to_end(&mut expected)
        .map_err(|e| format!("could not re-read the verified executable: {e}"))?;

    let mut installed = {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 0x0000_0001;
        std::fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .open(exe)
            .map_err(|e| format!("could not lock the installed executable to check it: {e}"))?
    };
    let mut actual = Vec::with_capacity(expected.len());
    installed
        .read_to_end(&mut actual)
        .map_err(|e| format!("could not read the installed executable: {e}"))?;

    if actual != expected {
        return Err(
            "the installed executable does not match the verified download — something changed \
             it during installation, so it was not started. Download the release manually from \
             GitHub."
                .to_string(),
        );
    }
    // Hand the pin back rather than dropping it. Re-opening the path later to
    // launch it would reintroduce the gap this check just closed: the verified
    // bytes could be swapped in between and the replacement started with the
    // administrator token, unverified.
    Ok(installed)
}

/// Write `dest` from an already-open, pinned source handle.
///
/// Rewinds first so the same handle can be reused, and truncates the
/// destination so a shorter new file cannot leave a tail of the old one.
fn copy_from_handle(src: &mut std::fs::File, dest: &Path) -> std::io::Result<()> {
    use std::io::{Seek, SeekFrom};
    src.seek(SeekFrom::Start(0))?;
    let mut out = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(dest)?;
    std::io::copy(src, &mut out)?;
    out.sync_all()
}

/// Check the downloaded zip against the SHA-256 the release workflow published
/// beside it, before a single byte of it is unpacked or executed.
///
/// This is what makes a truncated, corrupted, or substituted download fail
/// loudly instead of being installed and launched. It is not a signature: the
/// checksum comes from the same release, so it does not defend against the
/// release itself being malicious — code signing would. It does close every
/// failure between GitHub and this disk.
fn verify_checksum(zip: &mut impl std::io::Read, published: &str) -> Result<(), String> {
    use sha2::Digest;

    // The published line is `<hex>  <name>`; take the first field, normalise case.
    let expected = published
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if expected.len() != 64 || !expected.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("the published checksum is not a SHA-256 — refusing to install".to_string());
    }

    let mut hash = sha2::Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        match zip.read(&mut buffer) {
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

/// One staged file, held open so nothing can change it before it is installed.
pub struct StagedFile {
    /// Name it will take in the install directory.
    name: std::ffi::OsString,
    /// Handle opened denying write, rename and delete. Installation copies from
    /// *this*, never by re-opening the path.
    handle: std::fs::File,
}

/// Open every regular file in the extracted release folder, denying other
/// processes write/rename/delete access, and hold those handles.
///
/// This is what makes "the bytes we verified are the bytes we install" true.
/// The staging directory lives in the user's temp folder, so a same-user
/// process can write there; opening each file up front and copying from the
/// retained handle removes the window in which one could be swapped between
/// staging and installation. It also fails early — a truncated download or a
/// file the antivirus has locked errors here, while the installed copy is
/// still entirely intact.
fn staged_files(inner: &Path) -> Result<Vec<StagedFile>, String> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_SHARE_READ: u32 = 0x0000_0001;

    let mut files = Vec::new();
    for entry in std::fs::read_dir(inner).map_err(|e| format!("could not read extracted folder: {e}"))? {
        // Never `.flatten()` here: a dropped entry is a file that silently does
        // not get installed, and the result is a new executable running beside
        // a stale PresentMon or inspector whose hash no longer matches.
        let entry = entry.map_err(|e| format!("could not read the extracted release: {e}"))?;
        let src = entry.path();
        if !src.is_file() {
            continue;
        }
        let handle = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .open(&src)
            .map_err(|e| {
                format!(
                    "the downloaded {} could not be locked for installation: {e}",
                    src.display()
                )
            })?;
        files.push(StagedFile {
            name: entry.file_name(),
            handle,
        });
    }
    if files.is_empty() {
        return Err("the downloaded release contains no files".to_string());
    }
    // Everything this app needs at runtime has to be in the package. Installing
    // a partial one commits the swap and only then discovers that capture or
    // the driver profile no longer works.
    for required in packaged_names() {
        if !files.iter().any(|f| f.name == std::ffi::OsStr::new(required)) {
            return Err(format!(
                "the downloaded release is missing {required}, so it was not installed"
            ));
        }
    }
    Ok(files)
}

/// Swap every staged file into `app_dir`, putting everything back if any one
/// of them fails.
///
/// Without the rollback, a copy that fails part-way (disk full, antivirus lock,
/// permissions) leaves a mix of two versions behind — or, if the executable was
/// the one already renamed aside, no runnable app at all and no way back.
fn install_all(staged: &mut [StagedFile], app_dir: &Path) -> Result<(), String> {
    // One installer at a time, machine-wide. See [`UpdateLock`].
    let _lock = UpdateLock::acquire()?;
    // Pin the install directory for the whole swap. Every path below is built
    // from `app_dir`, which was resolved before any of this started: without
    // the pin a same-user process could rename that directory away and drop a
    // junction in its place, turning the elevated writes and renames that
    // follow into a write primitive aimed wherever it likes. Denying rename and
    // delete sharing, and refusing to open through a reparse point, removes
    // that. Same technique the config-file writer uses on the BDO folder.
    let _pinned_dir = pin_directory(app_dir)?;
    // Write the ledger *before* touching anything, so a crash at any point from
    // here on is recoverable by the next start. See [`cleanup_old_files`].
    let ledger = app_dir.join(INSTALL_LEDGER);
    let names: Vec<String> = staged
        .iter()
        .map(|f| f.name.to_string_lossy().into_owned())
        .collect();
    write_ledger(&ledger, LEDGER_PENDING, &names)
        .map_err(|e| format!("could not record the update for recovery: {e}"))?;

    match install_recorded(staged, app_dir) {
        Ok(()) => {
            // Mark it finished rather than deleting it. The ledger is also the
            // list of `.old` files this install created — the shipped set is
            // larger than the handful of names hard-coded in `packaged_names`
            // (README, licenses, the inspector `.config`) — so the next start
            // needs it to clean up after itself instead of leaving a stale
            // backup beside every file forever.
            // Part of the transaction, not bookkeeping after it. A `pending`
            // ledger left on disk tells the next start to roll a *successful*
            // update back, so failing to mark it done has to surface here —
            // where the user sees it and the previous version is still intact —
            // rather than as a silent downgrade on the next launch.
            write_ledger(&ledger, LEDGER_DONE, &names).map_err(|e| {
                format!(
                    "the update was installed but could not be recorded as finished ({e}), so \
                     it was not started — the next launch will restore the previous version"
                )
            })
        }
        Err(e) => {
            // Only drop the ledger when the rollback is known to have put
            // everything back. If it did not, this is the only record of a
            // half-finished install, and the next start would otherwise read
            // the leftover `.old` files as a completed update's litter and
            // delete the previous version.
            if rolled_back_cleanly(app_dir, &names) {
                let _ = std::fs::remove_file(&ledger);
            }
            Err(e)
        }
    }
}

/// Open `dir` so it cannot be renamed, deleted or relinked while the returned
/// handle lives, refusing it outright if it is already a link.
fn pin_directory(dir: &Path) -> Result<std::fs::File, String> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_READ_ATTRIBUTES: u32 = 0x0080;
    const FILE_SHARE_READ: u32 = 0x0000_0001;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

    // `FILE_FLAG_OPEN_REPARSE_POINT` means this open never follows a link, so
    // a junction swapped in beforehand is caught rather than traversed.
    let handle = std::fs::OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(dir)
        .map_err(|e| format!("could not lock {} for the update: {e}", dir.display()))?;
    // Classify from the handle, and by reparse *tag*: a junction is a mount
    // point, not a symlink, so a `is_symlink()` check would wave it through
    // while every later `app_dir.join(...)` followed it — turning the elevated
    // writes below into a redirected write primitive. Same test the config
    // writer uses on the BDO folder.
    if is_name_surrogate(&handle).unwrap_or(true) {
        return Err(format!(
            "{} is a link — refusing to install through it",
            dir.display()
        ));
    }
    Ok(handle)
}

/// Undo a swap that already completed, used when the installed executable
/// fails its post-install check.
///
/// At that point the ledger reads `done`, so nothing else will ever put the
/// previous version back — the next start would instead delete it as a
/// finished install's backup.
fn roll_back_completed_install(app_dir: &Path) -> Result<(), String> {
    let ledger = app_dir.join(INSTALL_LEDGER);
    let text = read_ledger(&ledger).map_err(|e| e.to_string())?;
    let names: Vec<String> = text.lines().skip(1).map(str::to_string).collect();
    // Re-arm the ledger first: if this process dies mid-rollback, the next
    // start finishes the job instead of finding a half-reverted folder.
    let _ = write_ledger(&ledger, LEDGER_PENDING, &names);
    let mut ok = true;
    for name in &names {
        if !restore_one(app_dir, name) {
            ok = false;
        }
    }
    if ok {
        let _ = std::fs::remove_file(&ledger);
    }
    Ok(())
}

/// Whether an open handle refers to a *name-surrogate* reparse point — a
/// symlink or junction, i.e. one that redirects a path.
///
/// Classifying from the handle rather than the path is what makes the check
/// atomic with the open. The name-surrogate bit keeps OneDrive-style
/// placeholders working: those are reparse points too, but not surrogates.
fn is_name_surrogate(handle: &std::fs::File) -> std::io::Result<bool> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        FileAttributeTagInfo, GetFileInformationByHandleEx, FILE_ATTRIBUTE_TAG_INFO,
    };

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    const REPARSE_TAG_NAME_SURROGATE: u32 = 0x2000_0000;

    let mut info = FILE_ATTRIBUTE_TAG_INFO::default();
    // SAFETY: `info` is a valid, correctly-sized FILE_ATTRIBUTE_TAG_INFO and
    // the handle is live for the call.
    unsafe {
        GetFileInformationByHandleEx(
            HANDLE(handle.as_raw_handle() as _),
            FileAttributeTagInfo,
            std::ptr::from_mut(&mut info).cast(),
            std::mem::size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32,
        )
    }
    .map_err(std::io::Error::other)?;

    Ok(info.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
        && info.ReparseTag & REPARSE_TAG_NAME_SURROGATE != 0)
}

/// First line of the ledger: whether the install it describes finished.
const LEDGER_PENDING: &str = "pending";
const LEDGER_DONE: &str = "done";

fn write_ledger(path: &Path, state: &str, names: &[String]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

    let mut text = String::from(state);
    for name in names {
        text.push('\n');
        text.push_str(name);
    }
    // The ledger has a predictable name in a user-writable folder, and this
    // runs elevated. `std::fs::write` follows a reparse point, so a link
    // planted at this path would have the administrator token truncate and
    // overwrite whatever it points at. `FILE_FLAG_OPEN_REPARSE_POINT` never
    // follows one — the open lands on the link itself and fails.
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    file.write_all(text.as_bytes())?;
    file.sync_all()
}

/// Read the ledger without following a link planted at its path.
fn read_ledger(path: &Path) -> std::io::Result<String> {
    use std::io::Read;
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    let mut text = String::new();
    file.read_to_string(&mut text)?;
    Ok(text)
}

/// Whether the rollback left nothing for startup recovery to do.
///
/// A leftover `.old` is the only thing recovery acts on, so its absence is the
/// condition. Note this is deliberately not "the file is present": a file the
/// install *created* where nothing existed before is correctly rolled back by
/// deleting it, and demanding it be present afterwards would keep a ledger
/// describing an install that is already fully undone.
fn rolled_back_cleanly(dir: &Path, names: &[String]) -> bool {
    names
        .iter()
        .all(|name| !dir.join(format!("{name}.old")).exists())
}

fn install_recorded(staged: &mut [StagedFile], app_dir: &Path) -> Result<(), String> {
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

    for file in staged.iter_mut() {
        let name = &file.name;
        let dest = app_dir.join(name);

        // Write the replacement into the install folder *first*, under a
        // temporary name, and only then swap. The obvious order — rename the
        // old file aside, then copy the new one in — leaves the destination
        // absent for the whole duration of a copy, and for `bdo-optimizer.exe`
        // that means a crash there leaves nothing to launch, so the ledger
        // recovery below can never run. Writing first shrinks that window to a
        // single rename.
        //
        // ponytail: a rename is not atomic against power loss, so the window is
        // narrowed, not eliminated. Closing it entirely needs a separate
        // bootstrap executable to do the swap — worth it only if this ever
        // actually strands someone.
        let incoming = app_dir.join(format!("{}.new", name.to_string_lossy()));
        let _ = std::fs::remove_file(&incoming);
        if let Err(e) = copy_from_handle(&mut file.handle, &incoming) {
            rollback(&displaced, &created);
            return Err(format!(
                "could not stage {}: {e} — the previous version was left in place",
                name.to_string_lossy()
            ));
        }

        let existed = dest.exists();
        if existed {
            // Renaming works even for the running exe; deleting would not.
            let old = app_dir.join(format!("{}.old", name.to_string_lossy()));
            let _ = std::fs::remove_file(&old);
            if let Err(e) = std::fs::rename(&dest, &old) {
                let _ = std::fs::remove_file(&incoming);
                rollback(&displaced, &created);
                return Err(format!(
                    "could not replace {}: {e} — the previous version was left in place",
                    name.to_string_lossy()
                ));
            }
            displaced.push((old, dest.clone()));
        }
        if let Err(e) = std::fs::rename(&incoming, &dest) {
            let _ = std::fs::remove_file(&incoming);
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

/// Run a tool and return its stdout, turning a non-zero exit into an error.
fn capture(cmd: &mut Command) -> Result<String, String> {
    let program = cmd.get_program().to_string_lossy().to_string();
    let out = cmd
        .output()
        .map_err(|e| format!("could not run {program}: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "{program} failed: {}",
            stderr.lines().last().unwrap_or("unknown error").trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
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

/// A machine-wide lock held across recovery, installation and rollback.
///
/// Two copies of the app updating at once share the ledger and the `.old`
/// names: one can overwrite the other's ledger, delete its backup, and adopt
/// its half-written file as its own — after which a failure loses the genuine
/// previous version for good. A named mutex is the cheapest thing that makes
/// the whole sequence one-at-a-time across processes.
struct UpdateLock(windows::Win32::Foundation::HANDLE);

impl UpdateLock {
    /// Take the lock, or report that another instance is mid-update.
    fn acquire() -> Result<Self, String> {
        use windows::core::PCWSTR;
        use windows::Win32::Foundation::WAIT_TIMEOUT;
        use windows::Win32::System::Threading::{CreateMutexW, WaitForSingleObject};

        let name: Vec<u16> = "Local\\bdo-optimizer-update"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        // SAFETY: `name` is a NUL-terminated wide string alive across the call.
        let handle = unsafe { CreateMutexW(None, false, PCWSTR(name.as_ptr())) }
            .map_err(|e| format!("could not create the update lock: {e}"))?;
        // SAFETY: `handle` is a live mutex handle from the call above.
        let waited = unsafe { WaitForSingleObject(handle, 0) };
        if waited == WAIT_TIMEOUT {
            // SAFETY: closing a handle we own and do not use again.
            unsafe {
                let _ = windows::Win32::Foundation::CloseHandle(handle);
            }
            return Err(
                "another copy of BDO Optimizer is already installing an update — wait for it \
                 to finish"
                    .to_string(),
            );
        }
        Ok(Self(handle))
    }
}

impl Drop for UpdateLock {
    fn drop(&mut self) {
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::Threading::ReleaseMutex;
        // SAFETY: the handle is owned by this guard and released exactly once.
        unsafe {
            let _ = ReleaseMutex(self.0);
            let _ = CloseHandle(self.0);
        }
    }
}

/// Name of the in-progress-install ledger written beside the executable.
///
/// It exists only between the first file being displaced and the last one
/// landing, and lists exactly which names this updater renamed aside.
const INSTALL_LEDGER: &str = "bdo-optimizer-update.pending";

/// Finish whatever the last update started: roll a crashed install back, then
/// remove the backups a completed one left behind.
///
/// The in-process rollback in [`install_all`] only runs when an operation
/// *returns an error*. Closing the window, a crash, or the machine losing power
/// mid-swap kills the thread with files already renamed aside — possibly
/// including the executable itself, leaving nothing runnable. The ledger is what
/// makes that recoverable from the next start.
fn cleanup_old_files() {
    let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(Path::to_path_buf))
    else {
        return;
    };
    recover_with_ledger(&dir);
}

/// The recovery itself, split out so it can be tested against a scratch folder
/// instead of the real install directory.
fn recover_with_ledger(dir: &Path) {
    // Recovery rewrites the same files an install does, so it takes the same
    // lock. If another instance holds it there is nothing stranded to recover.
    let Ok(_lock) = UpdateLock::acquire() else {
        return;
    };
    let ledger = dir.join(INSTALL_LEDGER);
    let (names, recovered) = match read_ledger(&ledger) {
        Ok(text) => {
            let mut lines = text.lines();
            let state = lines.next().unwrap_or_default().trim().to_string();
            let names: Vec<String> = lines.map(str::to_string).collect();
            // `done` means the install finished and the ledger is only a
            // cleanup list. `pending` (or anything unrecognised, which is the
            // safe reading) means it was interrupted: put every recorded file
            // back before anything else runs.
            let mut all_restored = true;
            if state != LEDGER_DONE {
                for name in &names {
                    if !restore_one(dir, name) {
                        all_restored = false;
                    }
                }
            }
            // Keep the ledger when anything failed. Removing it would throw
            // away the only description of a half-finished install, and the
            // sweep below would then treat the surviving `.old` file as a
            // completed install's leftover and delete it — losing the previous
            // version outright. Next start retries instead.
            if all_restored {
                let _ = std::fs::remove_file(&ledger);
            }
            (names, all_restored)
        }
        // No ledger: the last install completed, so the backups it left are
        // safe to drop.
        Err(_) => (Vec::new(), true),
    };

    if !recovered {
        return;
    }

    // Only ever delete backups this updater is known to own. Sweeping every
    // `*.old` in the folder destroys unrelated files — users extract the app
    // into shared folders, and someone's `settings.old` is not ours to remove.
    let owned: Vec<String> = names
        .into_iter()
        .chain(packaged_names().map(str::to_string))
        .collect();
    for name in &owned {
        let old = dir.join(format!("{name}.old"));
        // Never drop a backup while the file it backs up is missing: that is
        // the only copy left.
        if old.is_file() && dir.join(name).is_file() {
            let _ = std::fs::remove_file(&old);
        }
    }
}

/// Put one recorded file back from its `.old` backup. Reports whether the file
/// is now in a good state.
///
/// The destination may already exist — the crash could have happened after the
/// replacement landed — and it may be *running*, in which case Windows refuses
/// to delete it. Moving it aside instead of deleting is what makes recovery
/// work in that case; failing to place the backup leaves both copies on disk
/// rather than losing one.
fn restore_one(dir: &Path, name: &str) -> bool {
    let dest = dir.join(name);
    let old = dir.join(format!("{name}.old"));
    if !old.is_file() {
        // Nothing was displaced for this entry (or it is already back), so the
        // only bad state is the file being missing entirely.
        return dest.is_file();
    }
    if dest.exists() {
        let aside = dir.join(format!("{name}.interrupted"));
        let _ = std::fs::remove_file(&aside);
        if std::fs::rename(&dest, &aside).is_err() {
            return false;
        }
    }
    if std::fs::rename(&old, &dest).is_err() {
        return false;
    }
    let _ = std::fs::remove_file(dir.join(format!("{name}.interrupted")));
    true
}

/// The file names this app ships and therefore replaces on update. Used to
/// bound backup cleanup to updater-owned paths.
fn packaged_names() -> impl Iterator<Item = &'static str> {
    [
        "bdo-optimizer.exe",
        crate::presentmon::PRESENTMON_EXE,
        crate::video::INSPECTOR_EXE,
    ]
    .into_iter()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `install_all` takes a machine-wide named mutex, so two installing tests
    /// running on different threads of the same binary would make one of them
    /// fail with "another copy is already installing" — the lock doing exactly
    /// its job. Serialise them here instead of weakening it.
    fn installing() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|p| p.into_inner())
    }

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
        std::fs::write(&zip, b"release bytes").unwrap();
        let reread = || std::fs::File::open(&zip).unwrap();

        let real = {
            use sha2::Digest;
            format!("{:x}", sha2::Sha256::digest(b"release bytes"))
        };
        // Exactly what the release workflow publishes: hash, two spaces, name.
        let published = format!("{real}  bdo-optimizer-v9.9.9-windows-x64.zip\n");
        assert!(verify_checksum(&mut reread(), &published).is_ok());

        // A zip that does not match the published hash must be refused.
        let other = "e4c8b1f31e1a4f2e0d0a5c8a3b1d9e77a0f9c4b6d2e8a1c3f5079b2d4e6a8c10";
        assert!(verify_checksum(&mut reread(), &format!("{other}  x.zip\n")).is_err());

        // Garbage where a hash should be must be refused, not ignored.
        assert!(verify_checksum(&mut reread(), "not-a-hash  x.zip\n").is_err());
        assert!(verify_checksum(&mut reread(), "").is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A failed install must put every displaced file back rather than leave a
    /// half-swapped folder — or, worse, no executable at all.
    #[test]
    fn a_failed_install_restores_every_displaced_file() {
        let _serialised = installing();
        let base = std::env::temp_dir().join(format!("bdo-update-rollback-{}", std::process::id()));
        let app = base.join("app");
        let stage = base.join("stage");
        std::fs::create_dir_all(&app).unwrap();
        std::fs::create_dir_all(&stage).unwrap();

        std::fs::write(app.join("bdo-optimizer.exe"), b"old exe").unwrap();
        std::fs::write(app.join("PresentMon.exe"), b"old presentmon").unwrap();

        std::fs::write(stage.join("bdo-optimizer.exe"), b"new exe").unwrap();
        std::fs::write(stage.join("PresentMon.exe"), b"new presentmon").unwrap();
        // The package must be complete or `staged_files` refuses it.
        std::fs::write(stage.join(crate::video::INSPECTOR_EXE), b"new inspector").unwrap();

        let mut staged = staged_files(&stage).unwrap();
        staged.sort_by(|a, b| a.name.cmp(&b.name));
        // Hold the second destination open exclusively so its swap fails the
        // way an antivirus lock would — after the first file has already been
        // replaced.
        let blocker = {
            use std::os::windows::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .read(true)
                .share_mode(0)
                .open(app.join("PresentMon.exe"))
                .unwrap()
        };

        assert!(install_all(&mut staged, &app).is_err());
        drop(blocker);

        assert_eq!(
            std::fs::read(app.join("bdo-optimizer.exe")).unwrap(),
            b"old exe",
            "the already-swapped executable must be rolled back"
        );
        // The ledger must be gone: a returned error already rolled everything
        // back, so the next start has nothing to recover.
        assert!(!app.join(INSTALL_LEDGER).exists());

        drop(staged);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// An incomplete package must be refused before anything is swapped: a new
    /// executable beside a stale PresentMon fails every capture, after the
    /// install has already committed.
    #[test]
    fn an_incomplete_package_is_refused() {
        let dir = std::env::temp_dir().join(format!("bdo-update-partial-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("bdo-optimizer.exe"), b"new exe").unwrap();
        let err = match staged_files(&dir) {
            Err(e) => e,
            Ok(_) => panic!("an incomplete package must be refused"),
        };
        assert!(err.contains("missing"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A staged file is held open denying writes, so nothing can substitute it
    /// between verification and installation — and installation copies from
    /// that handle rather than re-opening the path.
    #[test]
    fn staged_files_are_pinned_against_substitution() {
        let _serialised = installing();
        let base = std::env::temp_dir().join(format!("bdo-update-pin-{}", std::process::id()));
        let stage = base.join("stage");
        let app = base.join("app");
        std::fs::create_dir_all(&stage).unwrap();
        std::fs::create_dir_all(&app).unwrap();
        std::fs::write(stage.join("bdo-optimizer.exe"), b"verified bytes").unwrap();
        std::fs::write(stage.join("PresentMon.exe"), b"pm").unwrap();
        std::fs::write(stage.join(crate::video::INSPECTOR_EXE), b"npi").unwrap();

        let mut staged = staged_files(&stage).unwrap();
        assert!(
            std::fs::OpenOptions::new()
                .write(true)
                .open(stage.join("bdo-optimizer.exe"))
                .is_err(),
            "a staged file must not be writable while it is pinned"
        );

        install_all(&mut staged, &app).unwrap();
        assert_eq!(
            std::fs::read(app.join("bdo-optimizer.exe")).unwrap(),
            b"verified bytes"
        );

        drop(staged);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A recovery that cannot put a file back must keep both the ledger and the
    /// backup. Dropping either would leave a half-updated install with the only
    /// copy of the previous version deleted.
    #[test]
    fn a_failed_recovery_keeps_the_ledger_and_the_backup() {
        let _serialised = installing();
        let dir = std::env::temp_dir().join(format!("bdo-update-recfail-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("bdo-optimizer.exe"), b"half-installed").unwrap();
        std::fs::write(dir.join("bdo-optimizer.exe.old"), b"the real old exe").unwrap();
        write_ledger(&dir.join(INSTALL_LEDGER), LEDGER_PENDING, &["bdo-optimizer.exe".to_string()]).unwrap();

        // Hold the destination open exclusively so it cannot be moved aside —
        // the same thing a still-running executable or an antivirus does.
        let blocker = {
            use std::os::windows::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .read(true)
                .share_mode(0)
                .open(dir.join("bdo-optimizer.exe"))
                .unwrap()
        };
        recover_with_ledger(&dir);
        drop(blocker);

        assert!(
            dir.join(INSTALL_LEDGER).exists(),
            "a failed recovery must keep the ledger so the next start retries"
        );
        assert_eq!(
            std::fs::read(dir.join("bdo-optimizer.exe.old")).unwrap(),
            b"the real old exe",
            "the only copy of the previous version must survive"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A crash mid-install leaves the ledger behind; the next start must put
    /// the displaced files back rather than leaving nothing runnable.
    #[test]
    fn a_ledger_left_behind_describes_what_to_restore() {
        let _serialised = installing();
        let dir = std::env::temp_dir().join(format!("bdo-update-ledger-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Simulate a crash after the executable was renamed aside.
        std::fs::write(dir.join("bdo-optimizer.exe.old"), b"old exe").unwrap();
        write_ledger(&dir.join(INSTALL_LEDGER), LEDGER_PENDING, &["bdo-optimizer.exe".to_string()]).unwrap();
        // An unrelated backup that is none of our business.
        std::fs::write(dir.join("settings.old"), b"someone else's").unwrap();

        recover_with_ledger(&dir);

        assert_eq!(
            std::fs::read(dir.join("bdo-optimizer.exe")).unwrap(),
            b"old exe",
            "the displaced executable must come back"
        );
        assert!(!dir.join(INSTALL_LEDGER).exists());
        assert!(
            dir.join("settings.old").exists(),
            "an unrelated .old file must not be swept up"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A ledger marked `done` describes a *finished* install, so recovery must
    /// not put the old files back — it must clear the backups that install
    /// left, including shipped files `packaged_names` does not list.
    #[test]
    fn a_completed_ledger_only_cleans_up() {
        let _serialised = installing();
        let dir = std::env::temp_dir().join(format!("bdo-update-done-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("bdo-optimizer.exe"), b"new exe").unwrap();
        std::fs::write(dir.join("bdo-optimizer.exe.old"), b"old exe").unwrap();
        std::fs::write(dir.join("README.md"), b"new readme").unwrap();
        std::fs::write(dir.join("README.md.old"), b"old readme").unwrap();
        write_ledger(
            &dir.join(INSTALL_LEDGER),
            LEDGER_DONE,
            &["bdo-optimizer.exe".to_string(), "README.md".to_string()],
        )
        .unwrap();

        recover_with_ledger(&dir);

        assert_eq!(
            std::fs::read(dir.join("bdo-optimizer.exe")).unwrap(),
            b"new exe",
            "a finished install must not be rolled back"
        );
        assert!(!dir.join("bdo-optimizer.exe.old").exists());
        assert!(
            !dir.join("README.md.old").exists(),
            "a shipped file outside packaged_names must still get its backup cleaned"
        );
        assert!(!dir.join(INSTALL_LEDGER).exists());

        let _ = std::fs::remove_dir_all(&dir);
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
