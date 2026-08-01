//! Relaunch the current executable **elevated** (Windows only).
//!
//! Benchmarking needs administrator rights for PresentMon's ETW trace session.
//! When the app is not elevated the Benchmark tab offers a "Restart as
//! administrator" button, which calls [`relaunch_as_admin`]. That runs the
//! current `.exe` again through `ShellExecuteW` with the `runas` verb, raising a
//! single UAC prompt. On success the caller closes the current instance; if the
//! user dismisses the prompt we stay open and show nothing scary.

use std::path::{Path, PathBuf};

use windows::core::PCWSTR;
use windows::Win32::Foundation::{GetLastError, ERROR_CANCELLED};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

/// `SE_ERR_ACCESSDENIED` — returned (as the `HINSTANCE`) both when the user
/// declines the UAC prompt and on a genuine access denial (AppLocker/WDAC, an
/// execute-denying ACL, elevation disabled by policy). `GetLastError` tells the
/// two apart: `ERROR_CANCELLED` means the user dismissed the dialog.
const SE_ERR_ACCESSDENIED: isize = 5;

/// `ShellExecuteW` returns an `HINSTANCE` whose numeric value is `> 32` on
/// success and a small error code otherwise (a legacy Win16 convention).
const SHELL_EXECUTE_SUCCESS_THRESHOLD: isize = 32;

/// Environment variable carrying the account that asked for elevation.
///
/// `runas` lets a standard user supply *another* administrator's credentials,
/// and the new process then runs as that administrator — with their `HKCU`,
/// their Documents, their AppData. Everything this app calls "per-user" would
/// silently target the wrong profile: the config edits, the undo records, the
/// saved benchmark sessions. The elevated instance compares this against its
/// own account so it can say so rather than quietly optimising a profile
/// nobody is playing on.
pub const ELEVATION_ORIGIN: &str = "BDO_OPTIMIZER_ELEVATION_ORIGIN";

/// The account this process runs as, `DOMAIN\user` style, lowercased.
pub fn current_account() -> String {
    let user = std::env::var("USERNAME").unwrap_or_default();
    let domain = std::env::var("USERDOMAIN").unwrap_or_default();
    format!("{domain}\\{user}").to_ascii_lowercase()
}

/// The requesting account when elevation landed in a *different* one.
pub fn elevated_into_another_account() -> Option<String> {
    let origin = std::env::var(ELEVATION_ORIGIN).ok()?;
    (origin.to_ascii_lowercase() != current_account()).then_some(origin)
}

/// Result of attempting to relaunch elevated.
pub enum RelaunchOutcome {
    /// An elevated instance was started — the caller should close this one.
    Launched,
    /// The user dismissed the UAC prompt; stay open, say nothing alarming.
    Cancelled,
    /// The relaunch failed for another reason (message is for logging/display).
    Failed(String),
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// NUL-terminated UTF-16 for a Windows path, without a lossy UTF-8 round trip.
fn os_to_wide(s: &std::ffi::OsStr) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    s.encode_wide().chain(std::iter::once(0)).collect()
}

/// Relaunch this process elevated via `ShellExecuteW`/`runas`.
///
/// Uses [`std::env::current_exe`] for the path to re-run. See [`RelaunchOutcome`]
/// for how the three cases (started / user-cancelled / failed) are reported.
pub fn relaunch_as_admin() -> RelaunchOutcome {
    match std::env::current_exe() {
        Ok(exe) => relaunch_path_as_admin(&exe),
        Err(e) => RelaunchOutcome::Failed(format!("could not resolve current exe: {e}")),
    }
}

fn relaunch_path_as_admin(exe: &Path) -> RelaunchOutcome {
    let exe = match validated_relaunch_target(exe) {
        Ok(exe) => exe,
        Err(error) => return RelaunchOutcome::Failed(error),
    };
    // Pin the image for the whole elevation. Releases are unsigned and run from
    // wherever the user unzipped them, so without this a same-user process could
    // overwrite the executable while the UAC prompt is on screen — the prompt
    // would still show our name, and the payload would receive the elevated
    // token. `FILE_SHARE_READ` alone denies write, rename and delete while
    // still letting the elevation service load the image (proved by
    // `bdo_bench::capture::tests::a_locked_executable_still_runs_but_cannot_be_written`).
    //
    // Fatal if it cannot be taken. Continuing without the pin would let an
    // attacker *cause* the failure — hold a conflicting handle, watch us elevate
    // anyway, then swap the image — which is the exact race this closes. Nothing
    // legitimate holds a write handle to a running executable, so refusing here
    // costs an honest user nothing. It closes the race, not the underlying
    // problem: code signing plus an install location under `Program Files` is
    // the real fix. See [`validated_relaunch_target`].
    let _pinned = match pin_image(&exe) {
        Some(handle) => handle,
        None => {
            return RelaunchOutcome::Failed(
                "this executable could not be locked against modification, so it was not \
                 restarted as administrator — another program is holding it open. Close any \
                 antivirus scan or file-sync tool touching the app folder and try again."
                    .to_string(),
            )
        }
    };
    // Record who asked. `ShellExecuteW` hands the child our environment, so the
    // elevated instance can compare and warn if it landed elsewhere.
    // SAFETY: single-threaded startup path; no other thread reads the env here.
    unsafe { std::env::set_var(ELEVATION_ORIGIN, current_account()) };
    let verb_w = to_wide("runas");
    // Encode the path exactly rather than round-tripping through lossy UTF-8,
    // which would rewrite an unpaired surrogate to U+FFFD and hand ShellExecuteW
    // a different path than the one we validated.
    let file_w = os_to_wide(exe.as_os_str());
    // Pin the elevated child's working directory to the exe's own folder. With a
    // null lpDirectory it inherits *our* CWD, and the current directory still
    // precedes PATH in the DLL search order — so launching from a writable
    // location (Downloads, a UNC share) would let a planted DLL load as admin.
    let dir_w = exe.parent().map(|dir| os_to_wide(dir.as_os_str()));
    let dir_ptr = dir_w
        .as_ref()
        .map_or(PCWSTR::null(), |d| PCWSTR(d.as_ptr()));

    // SAFETY: all pointers are to NUL-terminated wide buffers kept alive across
    // the call; ShellExecuteW does not retain them past return.
    let hinst = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(verb_w.as_ptr()),
            PCWSTR(file_w.as_ptr()),
            PCWSTR::null(),
            dir_ptr,
            SW_SHOWNORMAL,
        )
    };

    let code = hinst.0 as isize;
    if code > SHELL_EXECUTE_SUCCESS_THRESHOLD {
        return RelaunchOutcome::Launched;
    }
    if code == SE_ERR_ACCESSDENIED {
        // SAFETY: reading the calling thread's last-error value; always sound.
        if unsafe { GetLastError() } == ERROR_CANCELLED {
            return RelaunchOutcome::Cancelled;
        }
        return RelaunchOutcome::Failed(
            "Windows refused to start an elevated copy. Security policy \
             (AppLocker/WDAC), file permissions, or a disabled UAC prompt can \
             cause this."
                .to_string(),
        );
    }
    RelaunchOutcome::Failed(format!("ShellExecuteW(runas) failed (code {code})"))
}

/// Open the image denying write, rename and delete for as long as the returned
/// handle lives. `None` when it cannot be taken; see the call site for why that
/// is not fatal.
fn pin_image(exe: &Path) -> Option<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_SHARE_READ: u32 = 0x0000_0001;
    std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(exe)
        .ok()
}

/// Resolve the exe path to relaunch, rejecting the obvious nonsense cases.
///
/// This is a sanity check, **not** an integrity guarantee. Nothing here
/// verifies a signature or that the containing directory is not user-writable.
///
/// [`pin_image`] closes the narrow case — the executable itself being swapped
/// while the UAC prompt is up — but it deliberately does not claim more than
/// that. The elevated child still resolves its DLLs the ordinary way, starting
/// with its own directory, so a same-user process that can write into the app
/// folder can pre-place a DLL the child will load and get it running as
/// administrator without touching the pinned image at all. Pinning every
/// loadable file is not a real answer: the set is decided at runtime by the
/// graphics backend, and the app's own layout requires the bundled tools to sit
/// in that same folder.
///
/// The actual fix is code signing plus an install location under
/// `Program Files`, where the folder is not user-writable in the first place.
/// Until then this remains the honest boundary of what a portable unsigned
/// build can defend, and it is documented rather than papered over.
fn validated_relaunch_target(exe: &Path) -> Result<PathBuf, String> {
    let exe = exe
        .canonicalize()
        .map_err(|error| format!("could not canonicalize current exe: {error}"))?;
    let metadata = exe
        .metadata()
        .map_err(|error| format!("could not inspect current exe: {error}"))?;
    if !metadata.is_file() {
        return Err("current executable path is not a regular file".to_string());
    }
    Ok(exe)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relaunch_target_must_be_a_regular_file() {
        let dir =
            std::env::temp_dir().join(format!("bdo-relaunch-target-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(validated_relaunch_target(&dir).is_err());

        let exe = dir.join("bdo-optimizer.exe");
        std::fs::write(&exe, b"test").unwrap();
        assert_eq!(
            validated_relaunch_target(&exe).unwrap(),
            exe.canonicalize().unwrap()
        );

        std::fs::remove_file(exe).unwrap();
        std::fs::remove_dir(dir).unwrap();
    }
}
