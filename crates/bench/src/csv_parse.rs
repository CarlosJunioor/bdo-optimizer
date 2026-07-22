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
const FRAME_TIME_HEADERS: &[&str] = &["MsBetweenPresents", "FrameTime", "msBetweenPresents"];

/// Candidate header names for the process-name column.
const PROCESS_HEADERS: &[&str] = &["Application", "ProcessName", "process"];

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

    let ft_idx = find_header(&headers, FRAME_TIME_HEADERS).ok_or(BenchError::NoFrameTimeColumn)?;
    let proc_idx = find_header(&headers, PROCESS_HEADERS);

    let mut frames = Vec::new();
    let mut record = csv::StringRecord::new();
    loop {
        match rdr.read_record(&mut record) {
            Ok(true) => {}
            Ok(false) => break,
            // Skip a malformed record instead of failing the whole parse.
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
        match record.get(ft_idx).map(str::trim) {
            Some(s) if !s.is_empty() => {
                if let Ok(v) = s.parse::<f64>() {
                    if v.is_finite() && v > 0.0 {
                        frames.push(v);
                    }
                }
            }
            _ => {}
        }
    }

    if frames.is_empty() {
        return Err(BenchError::NoFrameData);
    }
    Ok(frames)
}

/// Case-insensitive lookup of the first matching header, returning its column index.
fn find_header(headers: &csv::StringRecord, candidates: &[&str]) -> Option<usize> {
    for cand in candidates {
        for (i, h) in headers.iter().enumerate() {
            if h.trim().eq_ignore_ascii_case(cand) {
                return Some(i);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

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
