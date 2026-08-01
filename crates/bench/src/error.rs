//! Error type shared across the benchmarking pipeline.

use std::path::PathBuf;

/// Errors produced by the benchmarking pipeline (capture, parsing, metrics, storage).
///
/// The library never panics on bad external input; every fallible path returns one
/// of these variants so the GUI can react (in particular [`BenchError::NeedsElevation`],
/// which the UI turns into a "please allow the UAC prompt" message).
#[derive(Debug, thiserror::Error)]
pub enum BenchError {
    /// The metrics input was empty — no frame times to compute over.
    #[error("no frame-time samples were provided")]
    EmptyInput,

    /// A CSV was read but no recognised frame-time column was found.
    #[error("no recognised frame-time column in CSV (looked for MsBetweenPresents / FrameTime)")]
    NoFrameTimeColumn,

    /// The CSV had a header but produced zero usable frame-time rows.
    #[error("CSV contained no usable frame-time rows")]
    NoFrameData,

    /// Underlying CSV reader/parser failure.
    #[error("failed to parse CSV: {0}")]
    Csv(String),

    /// PresentMon (or the OS) refused the ETW session for lack of admin rights.
    ///
    /// PresentMon uses ETW, which requires an elevated (administrator) process. When
    /// spawning fails with an access-denied error, the driver maps it to this variant
    /// so the UI can explain that the app must be re-launched elevated.
    #[error("PresentMon needs administrator elevation (ETW access was denied)")]
    NeedsElevation,

    /// PresentMon.exe could not be found at the configured path.
    #[error("PresentMon executable not found at {0}")]
    PresentMonNotFound(PathBuf),

    /// The executable did not match the bundled PresentMon build.
    #[error("refusing to run an untrusted PresentMon executable at {0}")]
    UntrustedPresentMon(PathBuf),

    /// PresentMon ran but exited with a nonzero status for a reason other than
    /// missing elevation.
    ///
    /// Carries PresentMon's process exit code (when the OS reported one) and the
    /// first ~500 characters of what it wrote to stderr, so the UI can show the
    /// actual message instead of a misleading "capture file not found" error.
    #[error("PresentMon exited with an error (exit code {code:?}): {stderr}")]
    PresentMonFailed {
        /// The process exit code, if one was reported.
        code: Option<i32>,
        /// The captured stderr (already truncated to ~500 chars).
        stderr: String,
    },

    /// Spawning or controlling the capture child process failed.
    #[error("failed to run capture process: {0}")]
    Spawn(String),

    /// Capture is not supported on this platform (only Windows drives PresentMon).
    #[error("live capture is only supported on Windows")]
    UnsupportedPlatform,

    /// Filesystem / IO failure while reading or writing session data.
    #[error("io error: {0}")]
    Io(String),

    /// (De)serialisation of a session JSON file failed.
    #[error("failed to (de)serialise session: {0}")]
    Serde(String),

    /// The platform data directory could not be determined.
    #[error("could not determine a platform data directory for sessions")]
    NoDataDir,
}

impl From<std::io::Error> for BenchError {
    fn from(e: std::io::Error) -> Self {
        // Deliberately *not* mapping `PermissionDenied` to `NeedsElevation`.
        // This conversion is on every IO path in the crate, including saving
        // and loading sessions — so a read-only sessions folder, an ACL, or an
        // antivirus lock reported "PresentMon needs administrator rights",
        // which is wrong, unactionable, and shown even when no capture ran and
        // the app is already elevated. Elevation is classified only where it
        // can actually be diagnosed: the PresentMon spawn and exit paths in
        // `capture.rs`, which know it is an ETW failure.
        BenchError::Io(e.to_string())
    }
}

impl From<serde_json::Error> for BenchError {
    fn from(e: serde_json::Error) -> Self {
        BenchError::Serde(e.to_string())
    }
}

impl From<csv::Error> for BenchError {
    fn from(e: csv::Error) -> Self {
        BenchError::Csv(e.to_string())
    }
}
