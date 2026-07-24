//! FPS benchmarking pipeline for the BDO optimizer.
//!
//! This crate drives Intel PresentMon to capture per-frame timing, parses its CSV,
//! computes FPS metrics, and stores/loads benchmark sessions locally.
//!
//! # Anti-cheat-safe, out-of-process capture
//!
//! Black Desert Online's anti-cheat blocks any process that opens a handle into the
//! running game, so this crate **never touches the game process**. All FPS data comes
//! from Intel PresentMon, which reads the OS's ETW present-event stream out of process
//! and writes a CSV. The pipeline only ever spawns PresentMon, waits on it, and reads
//! its file — no injection, no game-process handles, nothing the anti-cheat can flag.
//! (PresentMon's ETW session does require administrator elevation; see [`capture`].)
//!
//! # Layout
//!
//! * [`metrics`] — pure, heavily-tested FPS math (avg / min / max / P1 / P0.1 /
//!   time-weighted 1% & 0.1% low, plus a smoothed live [`current_fps`]). Raw frame
//!   times are the source of truth.
//! * [`csv_parse`] — PresentMon CSV parsing, resolving columns by header name.
//! * [`live`] — incremental tailing of a *growing* capture CSV for real-time stats,
//!   reusing `csv_parse`'s header resolution so live and final numbers agree.
//! * [`capture`] — the PresentMon spawn driver and process-detection helpers.
//! * [`session`] — one-JSON-file-per-run session storage; metrics recomputed on load.
//! * [`error`] — the shared [`BenchError`] type.

#![forbid(unsafe_code)]

pub mod capture;
pub mod csv_parse;
pub mod error;
pub mod live;
pub mod metrics;
pub mod session;

pub use capture::{
    build_presentmon_args, classify_presentmon_failure, is_process_running,
    is_supported_presentmon, start_capture, wait_for_process, CaptureConfig, CaptureHandle,
};
pub use csv_parse::parse_presentmon_csv;
pub use error::BenchError;
pub use live::{LiveReader, LiveStats, CURRENT_FPS_WINDOW};
pub use metrics::{
    current_fps, gain_verdict, GainVerdict, Metrics, LOW_CONFIDENCE_FRAMES,
    MIN_MEANINGFUL_DELTA_FPS, MIN_MEANINGFUL_DELTA_PERCENT,
};
pub use session::{Session, SessionRole, SessionStore};
