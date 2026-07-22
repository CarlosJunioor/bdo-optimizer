//! Relaunch the current executable **elevated** (Windows only).
//!
//! Benchmarking needs administrator rights for PresentMon's ETW trace session.
//! When the app is not elevated the Benchmark tab offers a "Restart as
//! administrator" button, which calls [`relaunch_as_admin`]. That runs the
//! current `.exe` again through `ShellExecuteW` with the `runas` verb, raising a
//! single UAC prompt. On success the caller closes the current instance; if the
//! user dismisses the prompt we stay open and show nothing scary.

use std::path::Path;

use windows::core::PCWSTR;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

/// `SE_ERR_ACCESSDENIED` — the value `ShellExecuteW` returns (as its `HINSTANCE`)
/// when the user declines the UAC elevation prompt.
const SE_ERR_ACCESSDENIED: isize = 5;

/// `ShellExecuteW` returns an `HINSTANCE` whose numeric value is `> 32` on
/// success and a small error code otherwise (a legacy Win16 convention).
const SHELL_EXECUTE_SUCCESS_THRESHOLD: isize = 32;

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
    let verb_w = to_wide("runas");
    let file_w = to_wide(&exe.to_string_lossy());

    // SAFETY: all pointers are to NUL-terminated wide buffers kept alive across
    // the call; ShellExecuteW does not retain them past return.
    let hinst = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(verb_w.as_ptr()),
            PCWSTR(file_w.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };

    let code = hinst.0 as isize;
    if code > SHELL_EXECUTE_SUCCESS_THRESHOLD {
        RelaunchOutcome::Launched
    } else if code == SE_ERR_ACCESSDENIED {
        RelaunchOutcome::Cancelled
    } else {
        RelaunchOutcome::Failed(format!("ShellExecuteW(runas) failed (code {code})"))
    }
}
