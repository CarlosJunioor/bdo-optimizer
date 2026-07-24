//! Benchmark capture worker.
//!
//! One capture runs on a background thread so the UI never blocks. The thread:
//!
//! 1. spawns PresentMon via [`bdo_bench::start_capture`];
//! 2. polls [`bdo_bench::is_process_running`] and the PresentMon handle,
//!    reporting `Waiting` (game not yet up) then `Capturing { elapsed }`;
//! 3. on stop request, game exit, or PresentMon exit, kills the capture, parses
//!    the CSV, builds a [`Session`], saves it, and reports `Saved`.
//!
//! Progress is delivered over an `mpsc` channel and drained by the UI each
//! frame; the worker calls `request_repaint` so the elapsed timer ticks live. A
//! shared [`AtomicBool`] carries the stop request the other way.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bdo_bench::{
    is_process_running, parse_presentmon_csv, start_capture, BenchError, CaptureConfig, LiveReader,
    Metrics, Session, SessionRole, SessionStore,
};

/// The running game process name PresentMon targets.
pub const GAME_EXE: &str = "BlackDesert64.exe";

/// Version string stamped into saved sessions (the bundled PresentMon build).
pub const PRESENTMON_VERSION: &str = "2.5.1";

/// Poll cadence for the game-process / PresentMon-liveness check.
const POLL: Duration = Duration::from_millis(500);

/// Live stats sampled from the growing capture CSV, sent to the UI ~2×/second so
/// the Benchmark live panel and the always-on-top overlay can update while playing.
///
/// This is a compact, copy-cheap snapshot — the full frame series stays in the
/// worker's [`LiveReader`]. `sparkline` carries only the most recent ~10 s of
/// frame times for the live chart, so the message stays small regardless of how
/// long the capture has run.
#[derive(Debug, Clone, Default)]
pub struct LiveSnapshot {
    /// Seconds since the game was first seen.
    pub elapsed: Duration,
    /// Total frames captured so far.
    pub frames: usize,
    /// Smoothed instantaneous FPS (last ~30 frames).
    pub current_fps: f64,
    /// Whole-session time-weighted average FPS so far.
    pub avg_fps: f64,
    /// 1% low (percentile method) over the series so far.
    pub p1_low_fps: f64,
    /// 1% low integral (CapFrameX method) over the series so far.
    pub one_percent_low_integral_fps: f64,
    /// The most recent ~10 s of frame times (ms) for the live sparkline.
    pub sparkline: Vec<f64>,
}

/// A progress message from the worker to the UI.
pub enum CaptureMsg {
    /// PresentMon is armed but the game process has not appeared yet.
    Waiting,
    /// Actively capturing; `elapsed` since the game was first seen.
    Capturing { elapsed: Duration },
    /// Real-time stats sampled from the growing CSV mid-capture.
    Live(LiveSnapshot),
    /// Capture ended; parsing/saving in progress.
    Saving,
    /// Session written to disk.
    Saved { frames: usize },
    /// PresentMon could not open its ETW session — app must be run as admin.
    NeedsElevation,
    /// Any other failure, already formatted for display.
    Error(String),
}

/// The most recent frame times covering up to `max_seconds` of wall-clock, capped
/// at `max_points` samples, for a bounded-size live sparkline.
fn recent_frame_times(frames: &[f64], max_seconds: f64, max_points: usize) -> Vec<f64> {
    let budget_ms = max_seconds * 1000.0;
    let mut acc = 0.0;
    let mut start = frames.len();
    for (i, &t) in frames.iter().enumerate().rev() {
        if t.is_finite() && t > 0.0 {
            acc += t;
        }
        start = i;
        if acc >= budget_ms {
            break;
        }
    }
    let slice = &frames[start..];
    if slice.len() > max_points {
        slice[slice.len() - max_points..].to_vec()
    } else {
        slice.to_vec()
    }
}

/// Inputs needed to run one capture.
pub struct CaptureParams {
    pub presentmon_path: PathBuf,
    pub output_csv: PathBuf,
    pub label: String,
    pub role: SessionRole,
    pub expected_affinity_mask: String,
    pub observed_affinity_mask: String,
    pub cpu: String,
    pub gpu: String,
    pub store_dir: PathBuf,
}

/// Best-effort cleanup for the transient raw CSV on every worker exit path.
struct RawCaptureCleanup(PathBuf);

impl Drop for RawCaptureCleanup {
    fn drop(&mut self) {
        match std::fs::remove_file(&self.0) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => {}
        }
    }
}

/// Handle to an in-flight capture worker.
pub struct CaptureWorker {
    stop_flag: Arc<AtomicBool>,
    pub rx: Receiver<CaptureMsg>,
}

impl CaptureWorker {
    /// Start a capture on a background thread.
    pub fn start(params: CaptureParams, ctx: egui::Context) -> Self {
        let (tx, rx) = channel();
        let stop_flag = Arc::new(AtomicBool::new(false));
        let flag = stop_flag.clone();
        std::thread::spawn(move || run(params, flag, tx, ctx));
        Self { stop_flag, rx }
    }

    /// Ask the worker to stop at its next poll.
    pub fn request_stop(&self) {
        self.stop_flag.store(true, Ordering::SeqCst);
    }
}

fn capture_elapsed(start: &mut Option<Instant>, running: bool, now: Instant) -> Option<Duration> {
    if running {
        let started = *start.get_or_insert(now);
        Some(now.duration_since(started))
    } else {
        start.map(|started| now.duration_since(started))
    }
}

fn run(p: CaptureParams, stop: Arc<AtomicBool>, tx: Sender<CaptureMsg>, ctx: egui::Context) {
    let cfg = CaptureConfig {
        presentmon_path: p.presentmon_path.clone(),
        process_name: GAME_EXE.to_string(),
        output_csv: p.output_csv.clone(),
    };

    let mut handle = match start_capture(&cfg) {
        Ok(h) => h,
        Err(BenchError::NeedsElevation) => {
            let _ = tx.send(CaptureMsg::NeedsElevation);
            ctx.request_repaint();
            return;
        }
        Err(e) => {
            let _ = tx.send(CaptureMsg::Error(e.to_string()));
            ctx.request_repaint();
            return;
        }
    };
    let _raw_capture_cleanup = RawCaptureCleanup(p.output_csv.clone());

    // Tail the growing CSV for live stats. Same process filter as the final parse,
    // so the running numbers match the saved session.
    let mut live = LiveReader::new(p.output_csv.clone(), Some(GAME_EXE.to_string()));

    let mut start = None;
    let mut game_seen = false;
    loop {
        if stop.load(Ordering::SeqCst) {
            break;
        }
        let running = is_process_running(GAME_EXE).is_some();
        let elapsed = capture_elapsed(&mut start, running, Instant::now());
        if running {
            game_seen = true;
        }
        let _ = tx.send(match elapsed {
            Some(elapsed) => CaptureMsg::Capturing { elapsed },
            None => CaptureMsg::Waiting,
        });

        // Read whatever PresentMon has appended and publish a live snapshot. A
        // transient read error (file mid-write) is ignored — the next poll retries.
        let _ = live.poll();
        let frames = live.frames();
        if !frames.is_empty() {
            if let Ok(m) = Metrics::from_frame_times(frames) {
                let _ = tx.send(CaptureMsg::Live(LiveSnapshot {
                    elapsed: elapsed.unwrap_or_default(),
                    frames: frames.len(),
                    current_fps: live.current_fps(),
                    avg_fps: m.avg_fps,
                    p1_low_fps: m.p1_low_fps,
                    one_percent_low_integral_fps: m.one_percent_low_integral_fps,
                    sparkline: recent_frame_times(frames, 10.0, 1200),
                }));
            }
        }
        ctx.request_repaint();

        // PresentMon exits itself when the game closes (--terminate_on_proc_exit).
        if !handle.is_running() {
            break;
        }
        // Belt-and-braces: the game was up and is now gone.
        if game_seen && !running {
            break;
        }
        std::thread::sleep(POLL);
    }

    // Terminate PresentMon (no-op if it already exited) and inspect *why* it
    // ended. If it exited on its own with a failure — most importantly the
    // unelevated "access denied" case — surface that instead of blaming a missing
    // CSV. A deliberate stop (PresentMon still running) or a clean exit returns Ok.
    if let Err(e) = handle.finish() {
        let _ = tx.send(match e {
            BenchError::NeedsElevation => CaptureMsg::NeedsElevation,
            other => CaptureMsg::Error(other.to_string()),
        });
        ctx.request_repaint();
        return;
    }

    let _ = tx.send(CaptureMsg::Saving);
    ctx.request_repaint();

    // PresentMon exited cleanly (finish() returned Ok), so the CSV should exist.
    // Any problem here is genuinely unexpected rather than the elevation failure.
    let frames = match std::fs::File::open(&p.output_csv) {
        Ok(file) => match parse_presentmon_csv(file, Some(GAME_EXE)) {
            Ok(frames) => frames,
            Err(e) => {
                let _ = tx.send(CaptureMsg::Error(format!(
                    "Capture produced no usable frame data ({e}). Did the game run long enough to record frames?"
                )));
                ctx.request_repaint();
                return;
            }
        },
        Err(e) => {
            let _ = tx.send(CaptureMsg::Error(format!(
                "PresentMon finished but its capture file {} could not be read: {e}",
                p.output_csv.display()
            )));
            ctx.request_repaint();
            return;
        }
    };

    let count = frames.len();
    let mut session = Session::new(p.label, p.cpu, p.gpu, frames);
    session.role = p.role;
    session.expected_affinity_mask = Some(p.expected_affinity_mask.clone());
    session.observed_affinity_mask = Some(p.observed_affinity_mask);
    session.affinity_mask = (p.role == SessionRole::Optimized).then_some(p.expected_affinity_mask);
    session.presentmon_version = Some(PRESENTMON_VERSION.to_string());

    let store = SessionStore::new(p.store_dir);
    match store.save(&session) {
        Ok(_path) => {
            let _ = tx.send(CaptureMsg::Saved { frames: count });
        }
        Err(e) => {
            let _ = tx.send(CaptureMsg::Error(format!("Failed to save session: {e}")));
        }
    }
    ctx.request_repaint();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_frame_times_bounds_by_seconds_and_points() {
        // 1000 frames of 10ms = 10s total. Ask for the last 1s → ~100 frames.
        let frames = vec![10.0; 1000];
        let recent = recent_frame_times(&frames, 1.0, 10_000);
        // Walking back accumulates 10ms each until >= 1000ms → ~100–101 frames.
        assert!((100..=101).contains(&recent.len()), "got {}", recent.len());

        // max_points caps the result regardless of the time budget.
        let capped = recent_frame_times(&frames, 100.0, 50);
        assert_eq!(capped.len(), 50);
    }

    #[test]
    fn recent_frame_times_short_series_returns_all() {
        let frames = vec![16.6, 16.7, 16.8];
        assert_eq!(recent_frame_times(&frames, 10.0, 100), frames);
    }

    #[test]
    fn capture_timer_starts_when_game_appears() {
        let armed = Instant::now();
        let mut start = None;

        assert_eq!(capture_elapsed(&mut start, false, armed), None);
        assert_eq!(
            capture_elapsed(&mut start, true, armed),
            Some(Duration::ZERO)
        );
        assert_eq!(
            capture_elapsed(&mut start, true, armed + Duration::from_secs(2)),
            Some(Duration::from_secs(2))
        );
    }

    #[test]
    fn raw_capture_cleanup_removes_the_csv() {
        let path = std::env::temp_dir().join(format!(
            "bdo-capture-cleanup-test-{}.csv",
            std::process::id()
        ));
        std::fs::write(&path, "frame data").unwrap();
        drop(RawCaptureCleanup(path.clone()));
        assert!(!path.exists());
    }
}
