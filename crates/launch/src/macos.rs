//! macOS implementation — none available by OS design.
//!
//! macOS provides **no API to set the CPU affinity of an external process**
//! (its `thread_policy_set` affinity hints are advisory and in-process only),
//! and BDO has no macOS build. Every apply/verify entry point therefore returns
//! [`LaunchError::UnsupportedPlatform`]. The functions exist so the workspace
//! compiles and links on macOS.

use crate::error::LaunchError;
use crate::ShortcutOptions;
use std::path::{Path, PathBuf};

/// Unsupported on macOS — no external-process affinity API.
pub fn launch_with_affinity(
    _launcher_path: &Path,
    _mask: u64,
    _steam: bool,
) -> Result<u32, LaunchError> {
    Err(LaunchError::UnsupportedPlatform)
}

/// Unsupported on macOS.
pub fn create_shortcut(_opts: ShortcutOptions) -> Result<PathBuf, LaunchError> {
    Err(LaunchError::UnsupportedPlatform)
}

/// Unsupported on macOS — no BDO build to verify.
pub fn read_process_affinity(_process_name: &str) -> Result<Option<u64>, LaunchError> {
    Ok(None)
}

/// BDO does not install on macOS.
pub fn find_bdo_install() -> Vec<PathBuf> {
    Vec::new()
}
