//! Local benchmark-session storage: one JSON file per session.
//!
//! Raw frame times are the source of truth; metrics are recomputed on demand via
//! [`Session::metrics`], never persisted, so a formula fix retroactively improves every
//! stored session. Sessions live as individual JSON files under a base directory that
//! is injectable ([`SessionStore::new`]) for tests and GUI display, defaulting to the
//! platform data directory ([`SessionStore::default_store`]).

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::error::BenchError;
use crate::metrics::Metrics;

/// The intended role of a benchmark run in a trusted comparison.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionRole {
    /// Legacy or incomplete session; excluded from trusted comparisons.
    #[default]
    Unknown,
    Baseline,
    Optimized,
}

/// One saved benchmark run: its raw frame times plus the metadata needed to compare
/// runs (affinity mask, hardware, label, when).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    /// Capture time, RFC3339 (e.g. `2026-07-22T14:03:11Z`).
    pub timestamp: String,
    /// Human label, e.g. `"affinity 555"`.
    pub label: String,
    /// The CPU affinity mask this run used, if any (hex string, e.g. `"555"`).
    pub affinity_mask: Option<String>,
    /// Explicit comparison role. Missing in legacy JSON, which loads as `Unknown`.
    #[serde(default)]
    pub role: SessionRole,
    /// Affinity mask intended when capture began.
    #[serde(default)]
    pub expected_affinity_mask: Option<String>,
    /// Affinity mask read from the running process immediately before capture.
    #[serde(default)]
    pub observed_affinity_mask: Option<String>,
    /// CPU model string.
    pub cpu: String,
    /// GPU model string.
    pub gpu: String,
    /// Raw per-frame times in milliseconds — the source of truth.
    pub frames_ms: Vec<f64>,
    /// PresentMon version that produced the capture, if known.
    pub presentmon_version: Option<String>,
}

impl Session {
    /// Create a session stamped with the current UTC time in RFC3339.
    pub fn new(
        label: impl Into<String>,
        cpu: impl Into<String>,
        gpu: impl Into<String>,
        frames_ms: Vec<f64>,
    ) -> Session {
        Session {
            timestamp: OffsetDateTime::now_utc()
                .format(&Rfc3339)
                .unwrap_or_else(|_| "unknown".to_string()),
            label: label.into(),
            affinity_mask: None,
            role: SessionRole::Unknown,
            expected_affinity_mask: None,
            observed_affinity_mask: None,
            cpu: cpu.into(),
            gpu: gpu.into(),
            frames_ms,
            presentmon_version: None,
        }
    }

    /// Compute FPS metrics from this session's raw frame times.
    ///
    /// # Errors
    /// [`BenchError::EmptyInput`] if the session has no usable frames.
    pub fn metrics(&self) -> Result<Metrics, BenchError> {
        Metrics::from_frame_times(&self.frames_ms)
    }

    /// Whether the expected and read-only observed masks match.
    pub fn affinity_verified(&self) -> bool {
        match (
            self.expected_affinity_mask.as_deref(),
            self.observed_affinity_mask.as_deref(),
        ) {
            (Some(expected), Some(observed)) => {
                matches!((parse_mask(expected), parse_mask(observed)), (Some(a), Some(b)) if a == b)
            }
            _ => false,
        }
    }

    /// Filename this session is stored under: `<timestamp>_<label>.json`, sanitised so
    /// it is a valid, collision-resistant filename on all platforms.
    pub fn file_stem(&self) -> String {
        format!("{}_{}", sanitize(&self.timestamp), sanitize(&self.label))
    }
}

fn parse_mask(mask: &str) -> Option<u64> {
    let mask = mask.trim();
    let mask = mask
        .strip_prefix("0x")
        .or_else(|| mask.strip_prefix("0X"))
        .unwrap_or(mask);
    u64::from_str_radix(mask, 16).ok()
}

/// A directory of saved [`Session`] JSON files.
pub struct SessionStore {
    dir: PathBuf,
}

impl SessionStore {
    /// Create a store rooted at `dir` (created on first save). Injectable so tests can
    /// use a temp dir and the GUI can display / relocate the path.
    pub fn new(dir: impl Into<PathBuf>) -> SessionStore {
        SessionStore { dir: dir.into() }
    }

    /// The default per-user store: the platform data directory for
    /// `ProjectDirs("io", "bdo-optimizer", "bdo-optimizer")` plus `sessions/`.
    ///
    /// # Errors
    /// [`BenchError::NoDataDir`] if no data directory can be determined.
    pub fn default_store() -> Result<SessionStore, BenchError> {
        let dirs = directories::ProjectDirs::from("io", "bdo-optimizer", "bdo-optimizer")
            .ok_or(BenchError::NoDataDir)?;
        Ok(SessionStore {
            dir: dirs.data_dir().join("sessions"),
        })
    }

    /// The directory this store reads and writes (for display in the GUI).
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Persist a session as JSON, returning the path written. Creates the directory if
    /// needed. The filename derives from [`Session::file_stem`].
    ///
    /// # Errors
    /// [`BenchError::Io`] / [`BenchError::Serde`] on filesystem or serialisation failure.
    pub fn save(&self, session: &Session) -> Result<PathBuf, BenchError> {
        fs::create_dir_all(&self.dir)?;
        let path = self.dir.join(format!("{}.json", session.file_stem()));
        let json = serde_json::to_string_pretty(session)?;
        fs::write(&path, json)?;
        Ok(path)
    }

    /// List all stored sessions, sorted by timestamp descending (newest first).
    /// Files that fail to parse are skipped rather than aborting the listing.
    ///
    /// # Errors
    /// [`BenchError::Io`] only on failure to read the directory itself. A missing
    /// directory is treated as "no sessions" and returns an empty vec.
    pub fn list(&self) -> Result<Vec<Session>, BenchError> {
        if !self.dir.exists() {
            return Ok(Vec::new());
        }
        let mut sessions = Vec::new();
        for entry in fs::read_dir(&self.dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            if let Ok(text) = fs::read_to_string(&path) {
                if let Ok(session) = serde_json::from_str::<Session>(&text) {
                    sessions.push(session);
                }
            }
        }
        sessions.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        Ok(sessions)
    }

    /// Load a single session by its file stem (as produced by [`Session::file_stem`]).
    ///
    /// # Errors
    /// [`BenchError::Io`] if the file is missing/unreadable, [`BenchError::Serde`] if it
    /// does not deserialise.
    pub fn load(&self, file_stem: &str) -> Result<Session, BenchError> {
        let path = self.session_path(file_stem)?;
        let text = fs::read_to_string(&path)?;
        let session = serde_json::from_str::<Session>(&text)?;
        Ok(session)
    }

    /// Delete a session by its file stem. Missing files are treated as success.
    ///
    /// # Errors
    /// [`BenchError::Io`] on a filesystem failure other than "not found".
    pub fn delete(&self, file_stem: &str) -> Result<(), BenchError> {
        let path = self.session_path(file_stem)?;
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    fn session_path(&self, file_stem: &str) -> Result<PathBuf, BenchError> {
        if file_stem.is_empty()
            || file_stem == "."
            || file_stem == ".."
            || sanitize(file_stem) != file_stem
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "invalid session file stem",
            )
            .into());
        }
        Ok(self.dir.join(format!("{file_stem}.json")))
    }
}

/// Replace any character that is not alphanumeric, `-`, `.` or `_` with `_`, so the
/// result is a safe filename component on Windows, macOS and Linux.
fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '.' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn sample() -> Session {
        Session {
            timestamp: "2026-07-22T14:03:11Z".to_string(),
            label: "affinity 555".to_string(),
            affinity_mask: Some("555".to_string()),
            role: SessionRole::Optimized,
            expected_affinity_mask: Some("555".to_string()),
            observed_affinity_mask: Some("555".to_string()),
            cpu: "Ryzen 9 7900X3D".to_string(),
            gpu: "RTX 4080".to_string(),
            frames_ms: vec![16.6, 16.9, 17.0, 16.7],
            presentmon_version: Some("2.5.0".to_string()),
        }
    }

    #[test]
    fn file_stem_is_sanitised() {
        let s = sample();
        let stem = s.file_stem();
        // No colons or spaces survive.
        assert!(!stem.contains(':'));
        assert!(!stem.contains(' '));
        assert!(stem.contains("affinity_555"));
    }

    #[test]
    fn round_trip_save_load() {
        let dir = TempDir::new().unwrap();
        let store = SessionStore::new(dir.path());
        let s = sample();
        let path = store.save(&s).unwrap();
        assert!(path.exists());
        let loaded = store.load(&s.file_stem()).unwrap();
        assert_eq!(loaded, s);
    }

    #[test]
    fn list_returns_saved_sorted_desc() {
        let dir = TempDir::new().unwrap();
        let store = SessionStore::new(dir.path());
        let mut a = sample();
        a.timestamp = "2026-01-01T00:00:00Z".to_string();
        a.label = "old".to_string();
        let mut b = sample();
        b.timestamp = "2026-12-31T00:00:00Z".to_string();
        b.label = "new".to_string();
        store.save(&a).unwrap();
        store.save(&b).unwrap();
        let list = store.list().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].label, "new"); // newest first
        assert_eq!(list[1].label, "old");
    }

    #[test]
    fn list_missing_dir_is_empty() {
        let dir = TempDir::new().unwrap();
        let store = SessionStore::new(dir.path().join("does-not-exist"));
        assert!(store.list().unwrap().is_empty());
    }

    #[test]
    fn delete_removes_and_is_idempotent() {
        let dir = TempDir::new().unwrap();
        let store = SessionStore::new(dir.path());
        let s = sample();
        store.save(&s).unwrap();
        store.delete(&s.file_stem()).unwrap();
        assert!(store.load(&s.file_stem()).is_err());
        // Deleting again is a no-op success.
        store.delete(&s.file_stem()).unwrap();
    }

    #[test]
    fn load_and_delete_reject_path_traversal() {
        let dir = TempDir::new().unwrap();
        let store_dir = dir.path().join("sessions");
        let store = SessionStore::new(&store_dir);
        fs::create_dir(&store_dir).unwrap();
        let outside = dir.path().join("outside.json");
        fs::write(&outside, serde_json::to_string(&sample()).unwrap()).unwrap();

        assert!(store.load("../outside").is_err());
        assert!(store.delete("../outside").is_err());
        assert!(outside.exists());
    }

    #[test]
    fn metrics_computed_on_demand() {
        let s = sample();
        let m = s.metrics().unwrap();
        assert_eq!(m.frame_count, 4);
        assert!(m.avg_fps > 0.0);
    }

    #[test]
    fn new_stamps_rfc3339_timestamp() {
        let s = Session::new("t", "cpu", "gpu", vec![16.0]);
        // Parses back as RFC3339.
        assert!(OffsetDateTime::parse(&s.timestamp, &Rfc3339).is_ok());
        assert_eq!(s.role, SessionRole::Unknown);
        assert!(!s.affinity_verified());
    }

    #[test]
    fn explicit_role_and_matching_affinity_are_trusted() {
        let mut s = Session::new("baseline", "cpu", "gpu", vec![16.0]);
        s.role = SessionRole::Baseline;
        s.expected_affinity_mask = Some("fff".to_string());
        s.observed_affinity_mask = Some("fff".to_string());
        assert!(s.affinity_verified());

        s.observed_affinity_mask = Some("555".to_string());
        assert!(!s.affinity_verified());

        s.expected_affinity_mask = Some("invalid".to_string());
        s.observed_affinity_mask = Some("also-invalid".to_string());
        assert!(!s.affinity_verified());
    }

    #[test]
    fn legacy_json_loads_as_untrusted_unknown_role() {
        let legacy = r#"{
            "timestamp":"2026-07-22T14:03:11Z",
            "label":"old run",
            "affinity_mask":"555",
            "cpu":"cpu",
            "gpu":"gpu",
            "frames_ms":[16.0],
            "presentmon_version":null
        }"#;
        let session: Session = serde_json::from_str(legacy).unwrap();
        assert_eq!(session.role, SessionRole::Unknown);
        assert!(!session.affinity_verified());
    }

    #[test]
    fn corrupt_file_skipped_in_list() {
        let dir = TempDir::new().unwrap();
        let store = SessionStore::new(dir.path());
        store.save(&sample()).unwrap();
        fs::write(dir.path().join("garbage.json"), "{ not valid").unwrap();
        // The good one still lists; the garbage is skipped.
        assert_eq!(store.list().unwrap().len(), 1);
    }
}
