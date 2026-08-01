//! Error type shared by every `bdo-launch` entry point.

use std::fmt;
use std::path::PathBuf;

/// Errors returned by the launch / shortcut / verification APIs.
///
/// The library never panics on these conditions — they are all surfaced as
/// `Err(LaunchError::…)` so callers (the GUI) can render a message instead of
/// crashing.
#[derive(Debug)]
pub enum LaunchError {
    /// The affinity mask string was empty, not valid hex, or parsed to zero.
    InvalidMask(String),
    /// The mask selects a core index outside the machine's logical-core count.
    MaskOutOfRange { mask: u64, logical_cores: usize },
    /// A required path (launcher exe, destination dir) does not exist.
    PathNotFound(PathBuf),
    /// A launcher path had no parent directory to use as the working dir.
    NoParentDir(PathBuf),
    /// The launcher path contains a character that `cmd.exe` would interpret
    /// rather than treat as part of the path. Carries the offending path.
    UnsafeForCmd(String),
    /// This operation is not supported on the current operating system.
    UnsupportedPlatform,
    /// The user dismissed the UAC elevation prompt (`ERROR_CANCELLED`). This is a
    /// user choice, not a failure — surfaced separately so the GUI can show a
    /// calm message instead of a scary OS error.
    Cancelled,
    /// A Windows / COM / OS call failed. Carries a human-readable context string.
    Os(String),
}

impl fmt::Display for LaunchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LaunchError::InvalidMask(s) => {
                write!(f, "invalid affinity mask: {s}")
            }
            LaunchError::MaskOutOfRange {
                mask,
                logical_cores,
            } => write!(
                f,
                "affinity mask {mask:#x} selects a core beyond the {logical_cores} logical cores available"
            ),
            LaunchError::PathNotFound(p) => write!(f, "path not found: {}", p.display()),
            LaunchError::NoParentDir(p) => {
                write!(f, "path has no parent directory: {}", p.display())
            }
            LaunchError::UnsafeForCmd(p) => write!(
                f,
                "the game path {p:?} contains a character cmd.exe would interpret (% or a \
                 control character), so an optimized shortcut for it cannot be built safely — \
                 move or rename the install so its path has none"
            ),
            LaunchError::UnsupportedPlatform => {
                write!(f, "operation not supported on this platform")
            }
            LaunchError::Cancelled => {
                write!(f, "launch cancelled at the UAC prompt")
            }
            LaunchError::Os(s) => write!(f, "OS error: {s}"),
        }
    }
}

impl std::error::Error for LaunchError {}
