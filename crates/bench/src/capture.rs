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

/// Maximum number of stderr characters carried in [`BenchError::PresentMonFailed`].
const STDERR_SNIPPET_LEN: usize = 500;

/// Classify why a PresentMon process that exited **nonzero** failed, from its
/// exit code and captured stderr (pure — no IO, so it is unit-testable).
///
/// PresentMon 2.5.x, run without administrator rights, prints
/// `error: failed to start trace session: access denied.` and exits before
/// writing any CSV. When the stderr text names an access-denied / trace-session
/// failure (matched case-insensitively) this returns
/// [`BenchError::NeedsElevation`] so the UI can explain the UAC requirement.
///
/// Any other nonzero exit maps to [`BenchError::PresentMonFailed`], carrying the
/// exit code and the first [`STDERR_SNIPPET_LEN`] characters of stderr so the UI
/// can surface PresentMon's real message.
///
/// This must only be called for a genuinely nonzero exit: on a normal capture
/// (ended by `--terminate_on_proc_exit`) PresentMon exits `0`, and it may print
/// harmless warnings to stderr even on success — those must not be classified as
/// failures.
pub fn classify_presentmon_failure(exit_code: Option<i32>, stderr: &str) -> BenchError {
    let lower = stderr.to_ascii_lowercase();
    if lower.contains("access denied") || lower.contains("failed to start trace session") {
        return BenchError::NeedsElevation;
    }
    let snippet: String = stderr.trim().chars().take(STDERR_SNIPPET_LEN).collect();
    BenchError::PresentMonFailed {
        code: exit_code,
        stderr: snippet,
    }
}

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
/// PresentMon is spawned with stdout and stderr piped; a background thread drains
/// stderr into a shared buffer so the pipe never blocks the child, and the text is
/// available after exit for [`classify_presentmon_failure`].
///
/// Dropping the handle does **not** kill PresentMon; call [`CaptureHandle::finish`]
/// (preferred) or [`CaptureHandle::stop`] to terminate it explicitly, or rely on
/// `--terminate_on_proc_exit` when the game quits.
pub struct CaptureHandle {
    #[cfg(windows)]
    child: std::process::Child,
    #[cfg(windows)]
    stderr_buf: std::sync::Arc<std::sync::Mutex<String>>,
    #[cfg(windows)]
    stderr_reader: Option<std::thread::JoinHandle<()>>,
}

impl CaptureHandle {
    /// Join the stderr-draining thread (if not already joined) and return the text
    /// it captured.
    #[cfg(windows)]
    fn take_stderr(&mut self) -> String {
        if let Some(reader) = self.stderr_reader.take() {
            let _ = reader.join();
        }
        self.stderr_buf
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default()
    }

    /// Stop the capture, terminating the PresentMon process.
    ///
    /// Prefer [`CaptureHandle::finish`], which also reports *why* PresentMon ended;
    /// this remains for callers that only need to force a stop.
    ///
    /// # Errors
    /// Returns [`BenchError::Spawn`] if the kill or wait fails.
    pub fn stop(mut self) -> Result<(), BenchError> {
        #[cfg(windows)]
        {
            // Best-effort terminate; ignore "already exited".
            let _ = self.child.kill();
            self.child
                .wait()
                .map_err(|e| BenchError::Spawn(e.to_string()))?;
            let _ = self.take_stderr();
            Ok(())
        }
        #[cfg(not(windows))]
        {
            Err(BenchError::UnsupportedPlatform)
        }
    }

    /// Terminate the capture (if still running) and report why it ended.
    ///
    /// This is the counterpart the capture worker uses after its poll loop:
    ///
    /// * If PresentMon is **still running** — i.e. we are deliberately stopping it
    ///   (user pressed Stop, or the game exited) — it is killed and `Ok(())` is
    ///   returned; a kill produces a nonzero status that must **not** be treated as
    ///   a PresentMon failure, so the CSV written so far can still be parsed.
    /// * If PresentMon **already exited on its own** with status `0` (the normal
    ///   `--terminate_on_proc_exit` ending), `Ok(())` is returned.
    /// * If PresentMon **already exited on its own nonzero**, its captured stderr is
    ///   classified via [`classify_presentmon_failure`] and returned as the error —
    ///   [`BenchError::NeedsElevation`] for an access-denied / trace-session failure,
    ///   otherwise [`BenchError::PresentMonFailed`].
    ///
    /// `try_wait` distinguishes the "still running" case (`Ok(None)` → we kill) from
    /// the "already exited" case (`Ok(Some(status))` → inspect it); `std` caches the
    /// reaped status, so an earlier [`is_running`](Self::is_running) call that
    /// observed the exit does not perturb this.
    ///
    /// # Errors
    /// The classified PresentMon failure, or [`BenchError::Spawn`] if waiting fails.
    pub fn finish(mut self) -> Result<(), BenchError> {
        #[cfg(windows)]
        {
            match self.child.try_wait() {
                Ok(Some(status)) => {
                    if status.success() {
                        let _ = self.take_stderr();
                        Ok(())
                    } else {
                        let stderr = self.take_stderr();
                        Err(classify_presentmon_failure(status.code(), &stderr))
                    }
                }
                Ok(None) => {
                    // Still running — we are stopping it deliberately.
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    let _ = self.take_stderr();
                    Ok(())
                }
                Err(e) => Err(BenchError::Spawn(e.to_string())),
            }
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
/// The GUI is expected to request elevation up front. Run without it, PresentMon 2.5.x
/// does not fail the *spawn* — it starts, prints `failed to start trace session: access
/// denied.` to stderr, and exits nonzero without writing a CSV. That case is detected
/// after the fact via [`CaptureHandle::finish`], which classifies the captured stderr;
/// this function only maps an OS-level access-denied *spawn* refusal to
/// [`BenchError::NeedsElevation`].
///
/// PresentMon is spawned with stdout and stderr piped so [`CaptureHandle::finish`] can
/// inspect stderr; a background thread drains each pipe to keep the child unblocked.
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
        use std::io::Read;
        use std::os::windows::process::CommandExt;
        use std::process::{Command, Stdio};
        use std::sync::{Arc, Mutex};

        // Spawn PresentMon without a console window. PresentMon is a console
        // program; without this flag Windows allocates a visible black console
        // for it that pops up over the game the moment a capture starts. stdout
        // and stderr are still piped below, so suppressing the window costs us
        // nothing — we already read its output through the pipes.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;

        let args = build_presentmon_args(cfg);
        let mut child = match Command::new(&cfg.presentmon_path)
            .args(&args)
            .creation_flags(CREATE_NO_WINDOW)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                return Err(BenchError::NeedsElevation)
            }
            Err(e) => return Err(BenchError::Spawn(e.to_string())),
        };

        // Drain stdout to a sink so a long capture cannot block on a full pipe.
        if let Some(mut out) = child.stdout.take() {
            std::thread::spawn(move || {
                let mut sink = Vec::new();
                let _ = out.read_to_end(&mut sink);
            });
        }

        // Drain stderr into a shared buffer for post-exit classification.
        let stderr_buf = Arc::new(Mutex::new(String::new()));
        let stderr_reader = child.stderr.take().map(|mut err| {
            let buf = stderr_buf.clone();
            std::thread::spawn(move || {
                let mut s = String::new();
                let _ = err.read_to_string(&mut s);
                if let Ok(mut guard) = buf.lock() {
                    guard.push_str(&s);
                }
            })
        });

        Ok(CaptureHandle {
            child,
            stderr_buf,
            stderr_reader,
        })
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
    fn classify_access_denied_is_needs_elevation() {
        let stderr = "error: failed to start trace session: access denied.\n";
        assert!(matches!(
            classify_presentmon_failure(Some(6), stderr),
            BenchError::NeedsElevation
        ));
    }

    #[test]
    fn classify_trace_session_phrase_is_needs_elevation() {
        // The trace-session phrase alone (any casing) is enough.
        let stderr = "ERROR: Failed To Start Trace Session";
        assert!(matches!(
            classify_presentmon_failure(Some(6), stderr),
            BenchError::NeedsElevation
        ));
    }

    #[test]
    fn classify_other_failure_carries_code_and_stderr() {
        let stderr = "error: could not open output file\n";
        match classify_presentmon_failure(Some(2), stderr) {
            BenchError::PresentMonFailed { code, stderr: s } => {
                assert_eq!(code, Some(2));
                assert_eq!(s, "error: could not open output file");
            }
            other => panic!("expected PresentMonFailed, got {other:?}"),
        }
    }

    #[test]
    fn classify_truncates_long_stderr_to_snippet_len() {
        let stderr = "x".repeat(2000);
        match classify_presentmon_failure(None, &stderr) {
            BenchError::PresentMonFailed { code, stderr: s } => {
                assert_eq!(code, None);
                assert_eq!(s.chars().count(), STDERR_SNIPPET_LEN);
            }
            other => panic!("expected PresentMonFailed, got {other:?}"),
        }
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
