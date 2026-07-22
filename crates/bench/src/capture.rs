//! PresentMon capture driver.
//!
//! # Anti-cheat-safe, out-of-process design
//!
//! Black Desert Online's anti-cheat blocks any process that opens a handle into the
//! running game. This crate therefore never touches `BlackDesert64.exe` directly. FPS
//! is captured entirely **out of process** by Intel PresentMon, which reads the
//! Windows ETW present-event stream — the same mechanism the OS itself uses — and
//! writes a CSV. We only ever: spawn PresentMon, wait for it, and read its file. No
//! injection, no game-process handles, nothing the anti-cheat can flag.
//!
//! PresentMon's ETW session requires administrator rights; see [`start_capture`].
//!
//! The argument-building and process-detection helpers are pure and cross-platform so
//! they can be unit-tested anywhere; only the actual [`start_capture`] spawn is gated
//! to Windows (other platforms return [`BenchError::UnsupportedPlatform`]).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use sysinfo::{Pid, ProcessesToUpdate, System};

use crate::error::BenchError;

/// Configuration for one PresentMon capture session.
#[derive(Debug, Clone)]
pub struct CaptureConfig {
    /// Path to the bundled `PresentMon.exe`.
    pub presentmon_path: PathBuf,
    /// Target process image name, e.g. `"BlackDesert64.exe"`.
    pub process_name: String,
    /// Destination CSV path PresentMon writes per-frame data to.
    pub output_csv: PathBuf,
}

/// Build the PresentMon CLI argument vector for `cfg` (pure — no IO).
///
/// Produces:
/// `--process_name <name> --output_file <csv> --terminate_on_proc_exit --stop_existing_session`
///
/// * `--terminate_on_proc_exit` makes PresentMon exit when the game closes, so the
///   capture bounds the game's lifetime automatically.
/// * `--stop_existing_session` clears any orphaned ETW session left by a prior crash.
pub fn build_presentmon_args(cfg: &CaptureConfig) -> Vec<String> {
    vec![
        "--process_name".to_string(),
        cfg.process_name.clone(),
        "--output_file".to_string(),
        cfg.output_csv.to_string_lossy().into_owned(),
        "--terminate_on_proc_exit".to_string(),
        "--stop_existing_session".to_string(),
    ]
}

/// A handle to a running PresentMon capture child process.
///
/// Dropping the handle does **not** kill PresentMon; call [`CaptureHandle::stop`] to
/// terminate it explicitly (or rely on `--terminate_on_proc_exit` when the game quits).
pub struct CaptureHandle {
    #[cfg(windows)]
    child: std::process::Child,
}

impl CaptureHandle {
    /// Stop the capture, terminating the PresentMon process.
    ///
    /// # Errors
    /// Returns [`BenchError::Spawn`] if the kill or wait fails.
    pub fn stop(self) -> Result<(), BenchError> {
        #[cfg(windows)]
        {
            let mut child = self.child;
            // Best-effort terminate; ignore "already exited".
            let _ = child.kill();
            child.wait().map_err(|e| BenchError::Spawn(e.to_string()))?;
            Ok(())
        }
        #[cfg(not(windows))]
        {
            Err(BenchError::UnsupportedPlatform)
        }
    }

    /// Returns `true` while the PresentMon process is still running.
    pub fn is_running(&mut self) -> bool {
        #[cfg(windows)]
        {
            matches!(self.child.try_wait(), Ok(None))
        }
        #[cfg(not(windows))]
        {
            false
        }
    }
}

/// Spawn PresentMon for the given configuration.
///
/// PresentMon's ETW session requires the process to be **elevated (administrator)**.
/// The GUI is expected to request elevation up front; if the OS refuses the spawn with
/// an access-denied error this returns [`BenchError::NeedsElevation`] so the UI can
/// explain the UAC requirement rather than showing a generic failure.
///
/// # Errors
/// * [`BenchError::PresentMonNotFound`] if the executable path does not exist.
/// * [`BenchError::NeedsElevation`] on an access-denied spawn.
/// * [`BenchError::Spawn`] on any other spawn failure.
/// * [`BenchError::UnsupportedPlatform`] on non-Windows targets.
pub fn start_capture(cfg: &CaptureConfig) -> Result<CaptureHandle, BenchError> {
    if !cfg.presentmon_path.exists() {
        return Err(BenchError::PresentMonNotFound(cfg.presentmon_path.clone()));
    }

    #[cfg(windows)]
    {
        use std::process::Command;
        let args = build_presentmon_args(cfg);
        match Command::new(&cfg.presentmon_path).args(&args).spawn() {
            Ok(child) => Ok(CaptureHandle { child }),
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                Err(BenchError::NeedsElevation)
            }
            Err(e) => Err(BenchError::Spawn(e.to_string())),
        }
    }
    #[cfg(not(windows))]
    {
        let _ = cfg;
        Err(BenchError::UnsupportedPlatform)
    }
}

/// Return the PID of the first running process whose image name matches `name`
/// (case-insensitive), or `None` if it is not currently running.
pub fn is_process_running(name: &str) -> Option<Pid> {
    let mut sys = System::new();
    sys.refresh_processes(ProcessesToUpdate::All, true);
    find_pid(&sys, name)
}

/// Poll until a process named `name` appears, or until `timeout` elapses.
///
/// Refreshes the process list every `poll_interval`. Returns the PID as soon as the
/// process is seen, or `None` if `timeout` passes first. Intended for auto-starting a
/// capture the moment the game launches.
pub fn wait_for_process(name: &str, poll_interval: Duration, timeout: Duration) -> Option<Pid> {
    let mut sys = System::new();
    let start = Instant::now();
    loop {
        sys.refresh_processes(ProcessesToUpdate::All, true);
        if let Some(pid) = find_pid(&sys, name) {
            return Some(pid);
        }
        if start.elapsed() >= timeout {
            return None;
        }
        std::thread::sleep(poll_interval);
    }
}

/// Find a PID by case-insensitive image-name match within an already-refreshed system.
fn find_pid(sys: &System, name: &str) -> Option<Pid> {
    let want = name.trim();
    for (pid, proc_) in sys.processes() {
        if proc_.name().to_string_lossy().eq_ignore_ascii_case(want) {
            return Some(*pid);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> CaptureConfig {
        CaptureConfig {
            presentmon_path: PathBuf::from("vendor/presentmon/PresentMon.exe"),
            process_name: "BlackDesert64.exe".to_string(),
            output_csv: PathBuf::from("out.csv"),
        }
    }

    #[test]
    fn args_contain_all_flags_in_order() {
        let args = build_presentmon_args(&cfg());
        assert_eq!(
            args,
            vec![
                "--process_name",
                "BlackDesert64.exe",
                "--output_file",
                "out.csv",
                "--terminate_on_proc_exit",
                "--stop_existing_session",
            ]
        );
    }

    #[test]
    fn args_reflect_custom_process_and_path() {
        let mut c = cfg();
        c.process_name = "Game.exe".to_string();
        c.output_csv = PathBuf::from("C:/tmp/frames.csv");
        let args = build_presentmon_args(&c);
        let joined = args.join(" ");
        assert!(joined.contains("Game.exe"));
        assert!(joined.contains("frames.csv"));
    }

    #[test]
    fn start_capture_missing_exe_reports_not_found() {
        let mut c = cfg();
        c.presentmon_path = PathBuf::from("definitely/not/here/PresentMon.exe");
        assert!(matches!(
            start_capture(&c),
            Err(BenchError::PresentMonNotFound(_))
        ));
    }

    #[test]
    fn nonexistent_process_not_running() {
        assert!(is_process_running("this_process_should_not_exist_zzz.exe").is_none());
    }

    #[test]
    fn wait_for_process_times_out() {
        let got = wait_for_process(
            "this_process_should_not_exist_zzz.exe",
            Duration::from_millis(10),
            Duration::from_millis(30),
        );
        assert!(got.is_none());
    }
}
