//! Incremental ("live") reading of a PresentMon CSV **while it is still growing**.
//!
//! During a capture the app wants real-time stats — current FPS, running average,
//! percentile lows — without waiting for PresentMon to finish. PresentMon appends
//! one CSV row per present, so [`LiveReader`] tails that file: on each [`poll`] it
//! reads only the bytes appended since last time, parses every **complete**
//! (newline-terminated) line into a frame time, and accumulates them.
//!
//! It reuses the exact header-name resolution and frame-time acceptance rules of the
//! batch [`parse_presentmon_csv`](crate::csv_parse::parse_presentmon_csv) via the
//! shared [`HeaderLayout`] / [`parse_frame_time`] helpers, so live and final numbers
//! can never diverge. The final save still re-parses the whole CSV as the source of
//! truth; the live series is only for the on-screen readout.
//!
//! # Robustness to a file being written concurrently
//!
//! * **File not created yet** — PresentMon creates the CSV lazily, so [`poll`]
//!   treats a missing file as "nothing yet" and returns `Ok(0)`, never an error.
//! * **Partial trailing line** — a row still mid-write (no `\n` yet) is buffered and
//!   *not* consumed until its terminating newline arrives on a later poll.
//! * **Header not written yet** — lines before a resolvable header row are skipped;
//!   parsing of data rows begins only once the header column layout is known.

use std::fs::File;
use std::io::{ErrorKind, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::csv_parse::{parse_frame_time, HeaderLayout};
use crate::error::BenchError;
use crate::metrics::current_fps;

/// Default window (in frames) used by [`LiveReader::current_fps`] to smooth the
/// "current FPS" readout — roughly the last half-second at 60 FPS.
pub const CURRENT_FPS_WINDOW: usize = 30;

/// A cheap snapshot of the live series: how many frames so far and the smoothed
/// current FPS. The full accumulated series stays inside the [`LiveReader`]
/// (borrow it via [`LiveReader::frames`]) so this stays copy-cheap.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LiveStats {
    /// Smoothed instantaneous FPS over the last [`CURRENT_FPS_WINDOW`] frames.
    pub current_fps: f64,
    /// Total frames accumulated so far.
    pub frame_count: usize,
}

/// Tails a growing PresentMon CSV, accumulating parsed frame times across polls.
pub struct LiveReader {
    /// Path to the CSV PresentMon is writing (may not exist yet).
    path: PathBuf,
    /// Optional process-name filter (e.g. `BlackDesert64.exe`), applied only when
    /// the CSV actually has a process-name column.
    process_filter: Option<String>,
    /// Byte offset up to which the file has already been consumed.
    offset: u64,
    /// Bytes read past the last newline, awaiting completion on a later poll.
    partial: String,
    /// Resolved column layout, once a header row has been seen.
    layout: Option<HeaderLayout>,
    /// All accepted frame times so far, in file order.
    frames: Vec<f64>,
}

impl LiveReader {
    /// Create a reader for `path`, optionally filtering to one process name.
    ///
    /// The file need not exist yet; nothing is opened until the first [`poll`].
    pub fn new(path: impl Into<PathBuf>, process_filter: Option<String>) -> Self {
        Self {
            path: path.into(),
            process_filter,
            offset: 0,
            partial: String::new(),
            layout: None,
            frames: Vec::new(),
        }
    }

    /// Read any newly-appended complete lines and accumulate their frame times.
    ///
    /// Returns the number of frame times **added** by this call (0 when the file
    /// does not exist yet, nothing new was written, or only a partial line arrived).
    ///
    /// # Errors
    /// [`BenchError::Io`] on a read/seek failure other than the file simply not
    /// existing yet (which is treated as "nothing yet", `Ok(0)`).
    pub fn poll(&mut self) -> Result<usize, BenchError> {
        let mut file = match File::open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(BenchError::Io(e.to_string())),
        };

        // Resume from where we left off. If the file is shorter than our offset
        // (e.g. it was truncated/recreated), start over from the beginning.
        let len = file.metadata().map(|m| m.len()).unwrap_or(0);
        if len < self.offset {
            self.offset = 0;
            self.partial.clear();
            self.layout = None;
            self.frames.clear();
        }
        file.seek(SeekFrom::Start(self.offset))
            .map_err(|e| BenchError::Io(e.to_string()))?;

        let mut bytes = Vec::new();
        let read = file
            .read_to_end(&mut bytes)
            .map_err(|e| BenchError::Io(e.to_string()))?;
        self.offset += read as u64;
        if bytes.is_empty() {
            return Ok(0);
        }

        // PresentMon CSVs are ASCII; lossy decode also tolerates a multibyte char
        // that happens to straddle the current end-of-file between two polls.
        self.partial.push_str(&String::from_utf8_lossy(&bytes));

        let before = self.frames.len();
        while let Some(nl) = self.partial.find('\n') {
            // Drain the completed line (including the '\n') out of the buffer.
            let line: String = self.partial.drain(..=nl).collect();
            self.process_line(line.trim_end_matches(['\r', '\n']));
        }
        Ok(self.frames.len() - before)
    }

    /// Parse one complete line: resolve the header if we have not yet, else treat
    /// it as a data row and push its frame time if it passes the filter and rules.
    fn process_line(&mut self, line: &str) {
        if line.is_empty() {
            return;
        }
        let fields: Vec<&str> = line.split(',').collect();

        let Some(layout) = self.layout else {
            // Still looking for the header row; a line that does not resolve is
            // pre-header noise and is simply skipped.
            self.layout = HeaderLayout::resolve(&fields);
            return;
        };

        // Process filter, only when the CSV has a process column and we want one.
        if let (Some(pi), Some(want)) = (layout.process, self.process_filter.as_deref()) {
            match fields.get(pi) {
                Some(name) if name.trim().eq_ignore_ascii_case(want.trim()) => {}
                _ => return,
            }
        }

        if let Some(v) = fields
            .get(layout.frame_time)
            .and_then(|f| parse_frame_time(f))
        {
            self.frames.push(v);
        }
    }

    /// The full accumulated frame-time series so far (milliseconds), in file order.
    ///
    /// Cheap to hand to [`Metrics::from_frame_times`](crate::metrics::Metrics::from_frame_times)
    /// each UI tick — a sort over a few hundred-thousand frames per second is fine.
    pub fn frames(&self) -> &[f64] {
        &self.frames
    }

    /// Smoothed current FPS over the last [`CURRENT_FPS_WINDOW`] frames.
    pub fn current_fps(&self) -> f64 {
        current_fps(&self.frames, CURRENT_FPS_WINDOW)
    }

    /// A copy-cheap [`LiveStats`] snapshot (frame count + current FPS).
    pub fn stats(&self) -> LiveStats {
        LiveStats {
            current_fps: self.current_fps(),
            frame_count: self.frames.len(),
        }
    }

    /// The path being tailed.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Append `text` to `path`, creating it if needed (mimics PresentMon growing
    /// the CSV row by row).
    fn append(path: &Path, text: &str) {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        f.write_all(text.as_bytes()).unwrap();
        f.flush().unwrap();
    }

    #[test]
    fn missing_file_yields_empty_not_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does_not_exist_yet.csv");
        let mut r = LiveReader::new(&path, Some("BlackDesert64.exe".to_string()));
        // PresentMon creates the file lazily — polling before it exists is fine.
        assert_eq!(r.poll().unwrap(), 0);
        assert!(r.frames().is_empty());
    }

    #[test]
    fn incremental_stages_parse_correctly() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("frames.csv");
        let mut r = LiveReader::new(&path, Some("BlackDesert64.exe".to_string()));

        // Stage 1: header only, no data rows yet.
        append(&path, "Application,ProcessID,MsBetweenPresents\n");
        assert_eq!(r.poll().unwrap(), 0);
        assert!(r.frames().is_empty());

        // Stage 2: two complete data rows.
        append(
            &path,
            "BlackDesert64.exe,10,16.6\nBlackDesert64.exe,10,16.9\n",
        );
        assert_eq!(r.poll().unwrap(), 2);
        assert_eq!(r.frames(), &[16.6, 16.9]);

        // Stage 3: a partial (not newline-terminated) row must NOT be consumed.
        append(&path, "BlackDesert64.exe,10,17.0");
        assert_eq!(r.poll().unwrap(), 0);
        assert_eq!(r.frames(), &[16.6, 16.9]);

        // Stage 4: complete that row → now it is consumed.
        append(&path, "\n");
        assert_eq!(r.poll().unwrap(), 1);
        assert_eq!(r.frames(), &[16.6, 16.9, 17.0]);
    }

    #[test]
    fn process_filter_excludes_other_processes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mixed.csv");
        let mut r = LiveReader::new(&path, Some("BlackDesert64.exe".to_string()));

        append(&path, "Application,MsBetweenPresents\n");
        append(
            &path,
            "chrome.exe,8.0\nBlackDesert64.exe,16.6\nchrome.exe,8.1\n",
        );
        assert_eq!(r.poll().unwrap(), 1);
        assert_eq!(r.frames(), &[16.6]);
    }

    #[test]
    fn skips_blank_and_malformed_rows() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dirty.csv");
        let mut r = LiveReader::new(&path, None);

        append(&path, "MsBetweenPresents\n");
        append(&path, "16.6\n\nNaNish\n-4.0\n0\n17.0\n");
        assert_eq!(r.poll().unwrap(), 2);
        assert_eq!(r.frames(), &[16.6, 17.0]);
    }

    #[test]
    fn header_arriving_in_two_writes_still_resolves() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("split_header.csv");
        let mut r = LiveReader::new(&path, None);

        // Header split mid-line across two polls; only completes on the newline.
        append(&path, "MsBetween");
        assert_eq!(r.poll().unwrap(), 0);
        append(&path, "Presents\n16.6\n");
        assert_eq!(r.poll().unwrap(), 1);
        assert_eq!(r.frames(), &[16.6]);
    }

    #[test]
    fn stats_reports_count_and_current_fps() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stats.csv");
        let mut r = LiveReader::new(&path, None);
        append(&path, "MsBetweenPresents\n");
        // 5 frames of 1000/60 ms → current fps ≈ 60.
        for _ in 0..5 {
            append(&path, &format!("{}\n", 1000.0 / 60.0));
        }
        assert_eq!(r.poll().unwrap(), 5);
        let s = r.stats();
        assert_eq!(s.frame_count, 5);
        assert!((s.current_fps - 60.0).abs() < 1e-6);
    }
}
