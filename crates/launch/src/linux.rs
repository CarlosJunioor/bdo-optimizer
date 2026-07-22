//! Linux implementation — generic-game CPU pinning, **not** BDO.
//!
//! Black Desert Online cannot run on Linux (its anti-cheat blocks Proton), so
//! there is no launcher to detect or game process to verify. These functions
//! provide honest generic support: `taskset`-based launching and `.desktop`
//! file writing. The BDO-specific apply/verify entry points return
//! [`LaunchError::UnsupportedPlatform`].

use crate::common::{desktop_file_content, generate_taskset_command, mask_to_cores};
use crate::error::LaunchError;
use crate::ShortcutOptions;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Launch an arbitrary command pinned to the cores in `mask` via `taskset`.
///
/// BDO itself does not run on Linux; this treats the launcher path as a generic
/// executable and pins it. Returns the spawned child's PID.
pub fn launch_with_affinity(
    launcher_path: &Path,
    mask: u64,
    _steam: bool,
) -> Result<u32, LaunchError> {
    if !launcher_path.exists() {
        return Err(LaunchError::PathNotFound(launcher_path.to_path_buf()));
    }
    let cores = mask_to_cores(mask);
    let cmd_str = generate_taskset_command(&cores, &launcher_path.to_string_lossy());
    let mut parts = cmd_str.split_whitespace();
    let program = parts
        .next()
        .ok_or_else(|| LaunchError::Os("empty command".to_string()))?;
    let child = Command::new(program)
        .args(parts)
        .spawn()
        .map_err(|e| LaunchError::Os(format!("failed to spawn: {e}")))?;
    Ok(child.id())
}

/// Write a `.desktop` entry that launches the target with the mask's cores
/// pinned via `taskset`. Returns the path written.
///
/// The `.lnk`-style [`create_shortcut`] is Windows-only; on Linux this is the
/// analogous operation.
pub fn write_desktop_file(opts: ShortcutOptions) -> Result<PathBuf, LaunchError> {
    let mask = crate::common::parse_mask_hex(&opts.mask_hex)?;
    let cores = mask_to_cores(mask);
    let exec = generate_taskset_command(&cores, &opts.launcher_path.to_string_lossy());
    let content = desktop_file_content(
        "Black Desert Online (Optimized)",
        &exec,
        "Generic CPU-pinned launch (BDO does not run on Linux).",
    );
    let dest = opts.destination.unwrap_or_else(|| {
        directories::UserDirs::new()
            .and_then(|u| u.desktop_dir().map(|d| d.to_path_buf()))
            .unwrap_or_else(|| PathBuf::from("."))
            .join("bdo-optimized.desktop")
    });
    std::fs::write(&dest, content)
        .map_err(|e| LaunchError::Os(format!("failed to write .desktop: {e}")))?;
    Ok(dest)
}

/// Shortcut creation is Windows-only; on Linux use [`write_desktop_file`].
pub fn create_shortcut(_opts: ShortcutOptions) -> Result<PathBuf, LaunchError> {
    Err(LaunchError::UnsupportedPlatform)
}

/// BDO does not run on Linux, so there is no game process to verify.
pub fn read_process_affinity(_process_name: &str) -> Result<Option<u64>, LaunchError> {
    Ok(None)
}

/// BDO does not install on Linux.
pub fn find_bdo_install() -> Vec<PathBuf> {
    Vec::new()
}
