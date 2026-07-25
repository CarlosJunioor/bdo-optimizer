//! Parsing of Intel PresentMon capture CSVs into a raw frame-time series.
//!
//! PresentMon's column layout has changed across versions, so we resolve columns **by
//! header name**, never by index:
//!
//! * PresentMon 2.x: `MsBetweenPresents` (milliseconds between successive presents).
//! * A `FrameTime` column is also accepted (some 2.x builds / exports use it).
//! * PresentMon 1.x: `msBetweenPresents` (lower camel case).
//!
//! Matching is case-insensitive. Rows that are malformed (missing/blank/non-numeric
//! frame time) are skipped rather than aborting the parse. If a process-name column
//! is present and a filter is supplied, only rows for that process are kept.

use std::io::Read;

use crate::error::BenchError;

/// Candidate header names for the per-frame time column, in priority order.
pub(crate) const FRAME_TIME_HEADERS: &[&str] =
    &["MsBetweenPresents", "FrameTime", "msBetweenPresents"];

/// Candidate header names for the process-name column.
pub(crate) const PROCESS_HEADERS: &[&str] = &["Application", "ProcessName", "process"];

/// Resolved column positions for a PresentMon CSV, found by header **name**.
///
/// This is the single source of truth for "which column holds the frame time and
/// which (if any) holds the process name", shared by the batch
/// [`parse_presentmon_csv`] parser and the incremental
/// [`LiveReader`](crate::live::LiveReader) so the two can never drift apart.
#[derive(Debug, Clone, Copy)]
pub(crate) struct HeaderLayout {
    /// Column index of the frame-time field.
    pub(crate) frame_time: usize,
    /// Column index of the process-name field, when present.
    pub(crate) process: Option<usize>,
}

impl HeaderLayout {
    /// Resolve a header row (already split into trimmed-or-not fields) into a layout.
    ///
    /// Returns `None` when no recognised frame-time header is present — the caller
    /// decides whether that is an error (batch parse) or simply "header not written
    /// yet, keep waiting" (live parse).
    pub(crate) fn resolve<S: AsRef<str>>(fields: &[S]) -> Option<HeaderLayout> {
        let frame_time = find_header_idx(fields, FRAME_TIME_HEADERS)?;
        let process = find_header_idx(fields, PROCESS_HEADERS);
        Some(HeaderLayout {
            frame_time,
            process,
        })
    }
}

/// Case-insensitive lookup of the first matching header, returning its column index.
pub(crate) fn find_header_idx<S: AsRef<str>>(fields: &[S], candidates: &[&str]) -> Option<usize> {
    for cand in candidates {
        for (i, h) in fields.iter().enumerate() {
            if h.as_ref().trim().eq_ignore_ascii_case(cand) {
                return Some(i);
            }
        }
    }
    None
}

/// Parse one frame-time field, keeping only finite, strictly-positive values.
///
/// Blank, non-numeric, zero, negative and non-finite samples yield `None` and are
/// skipped by callers rather than aborting the parse. Shared so batch and live
/// parsing apply identical acceptance rules.
pub(crate) fn parse_frame_time(field: &str) -> Option<f64> {
    let s = field.trim();
    if s.is_empty() {
        return None;
    }
    match s.parse::<f64>() {
        Ok(v) if v.is_finite() && v > 0.0 && v <= crate::metrics::MAX_FRAME_TIME_MS => Some(v),
        _ => None,
    }
}

/// Parse a PresentMon CSV, returning per-frame times in milliseconds.
///
/// `process_filter`, when `Some`, keeps only rows whose process-name column matches
/// (case-insensitive) — useful to isolate `BlackDesert64.exe` from any other presenting
/// process captured in the same session. When the CSV has no process column the filter
/// is ignored.
///
/// # Errors
/// * [`BenchError::NoFrameTimeColumn`] if no recognised frame-time header exists.
/// * [`BenchError::NoFrameData`] if the header was found but no usable rows remained.
/// * [`BenchError::Csv`] on an underlying reader failure.
pub fn parse_presentmon_csv(
    reader: impl Read,
    process_filter: Option<&str>,
) -> Result<Vec<f64>, BenchError> {
    let mut rdr = csv::ReaderBuilder::new()
        .has_headers(true)
        .flexible(true) // tolerate rows with differing field counts
        .from_reader(reader);

    let headers = rdr.headers()?.clone();

    let header_fields: Vec<&str> = headers.iter().collect();
    let layout = HeaderLayout::resolve(&header_fields).ok_or(BenchError::NoFrameTimeColumn)?;
    let ft_idx = layout.frame_time;
    let proc_idx = layout.process;

    let mut frames = Vec::new();
    let mut record = csv::StringRecord::new();
    loop {
        match rdr.read_record(&mut record) {
            Ok(true) => {}
            Ok(false) => break,
            // A parse-level error consumed the bad record, so skipping it makes
            // progress. An IO error does *not* advance the reader, so `continue`
            // would spin forever — stop and keep whatever we already parsed.
            Err(e) if matches!(e.kind(), csv::ErrorKind::Io(_)) => break,
            Err(_) => continue,
        }

        // Process filter (only when both a column and a filter exist).
        if let (Some(pi), Some(want)) = (proc_idx, process_filter) {
            match record.get(pi) {
                Some(name) if name.trim().eq_ignore_ascii_case(want.trim()) => {}
                _ => continue,
            }
        }

        // Frame time: skip blanks / non-numeric / non-positive silently.
        if let Some(v) = record.get(ft_idx).and_then(parse_frame_time) {
            frames.push(v);
        }
    }

    if frames.is_empty() {
        return Err(BenchError::NoFrameData);
    }
    Ok(frames)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Yields one valid header + row, then fails every subsequent read. A csv
    /// IO error does not consume a record, so a `continue` here would spin.
    struct HeaderThenIoError {
        sent: bool,
    }

    impl std::io::Read for HeaderThenIoError {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.sent {
                return Err(std::io::Error::other("read failure mid-capture"));
            }
            self.sent = true;
            let head = b"MsBetweenPresents\n16.6\n";
            let n = head.len().min(buf.len());
            buf[..n].copy_from_slice(&head[..n]);
            Ok(n)
        }
    }

    #[test]
    fn io_error_terminates_instead_of_looping_forever() {
        // Before the fix this never returned. Reaching the assert at all is the test.
        let frames = parse_presentmon_csv(HeaderThenIoError { sent: false }, None).unwrap();
        assert_eq!(frames, vec![16.6]);
    }

    #[test]
    fn absurd_frame_time_is_rejected() {
        // A corrupt row would otherwise overflow the session duration and panic
        // the UI's Duration::from_secs_f64.
        assert_eq!(parse_frame_time("1e+308"), None);
        assert_eq!(parse_frame_time("60001"), None);
        assert_eq!(parse_frame_time("60000"), Some(60_000.0));
        assert_eq!(parse_frame_time("16.6"), Some(16.6));
    }

    #[test]
    fn parses_2x_msbetweenpresents() {
        let csv = "Application,ProcessID,MsBetweenPresents,MsGPUActive\n\
                   BlackDesert64.exe,1234,16.6,10.0\n\
                   BlackDesert64.exe,1234,16.9,10.1\n\
                   BlackDesert64.exe,1234,17.0,10.2\n";
        let frames = parse_presentmon_csv(csv.as_bytes(), None).unwrap();
        assert_eq!(frames, vec![16.6, 16.9, 17.0]);
    }

    #[test]
    fn parses_frametime_header() {
        let csv = "FrameTime,Other\n16.6,x\n33.3,y\n";
        let frames = parse_presentmon_csv(csv.as_bytes(), None).unwrap();
        assert_eq!(frames, vec![16.6, 33.3]);
    }

    #[test]
    fn parses_1x_lowercase_header() {
        let csv = "Application,msBetweenPresents\nGame.exe,16.6\nGame.exe,16.7\n";
        let frames = parse_presentmon_csv(csv.as_bytes(), None).unwrap();
        assert_eq!(frames, vec![16.6, 16.7]);
    }

    #[test]
    fn case_insensitive_header_match() {
        let csv = "MSBETWEENPRESENTS\n16.6\n16.7\n";
        let frames = parse_presentmon_csv(csv.as_bytes(), None).unwrap();
        assert_eq!(frames, vec![16.6, 16.7]);
    }

    #[test]
    fn missing_frame_time_column_errors() {
        let csv = "Application,ProcessID\nGame.exe,1\n";
        assert!(matches!(
            parse_presentmon_csv(csv.as_bytes(), None),
            Err(BenchError::NoFrameTimeColumn)
        ));
    }

    #[test]
    fn skips_malformed_and_blank_rows() {
        let csv = "MsBetweenPresents\n16.6\n\nNaNish\n-4.0\n0\n17.0\n";
        let frames = parse_presentmon_csv(csv.as_bytes(), None).unwrap();
        assert_eq!(frames, vec![16.6, 17.0]);
    }

    #[test]
    fn all_rows_bad_yields_no_frame_data() {
        let csv = "MsBetweenPresents\n\nabc\n-1\n";
        assert!(matches!(
            parse_presentmon_csv(csv.as_bytes(), None),
            Err(BenchError::NoFrameData)
        ));
    }

    #[test]
    fn process_filter_selects_target() {
        let csv = "Application,MsBetweenPresents\n\
                   BlackDesert64.exe,16.6\n\
                   chrome.exe,8.0\n\
                   BlackDesert64.exe,17.0\n";
        let frames = parse_presentmon_csv(csv.as_bytes(), Some("BlackDesert64.exe")).unwrap();
        assert_eq!(frames, vec![16.6, 17.0]);
    }

    #[test]
    fn process_filter_case_insensitive() {
        let csv = "Application,MsBetweenPresents\nBLACKDESERT64.EXE,16.6\n";
        let frames = parse_presentmon_csv(csv.as_bytes(), Some("blackdesert64.exe")).unwrap();
        assert_eq!(frames, vec![16.6]);
    }

    #[test]
    fn filter_ignored_when_no_process_column() {
        let csv = "MsBetweenPresents\n16.6\n17.0\n";
        let frames = parse_presentmon_csv(csv.as_bytes(), Some("anything")).unwrap();
        assert_eq!(frames, vec![16.6, 17.0]);
    }
}
