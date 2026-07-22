//! CPU-affinity launch, optimized shortcut creation, and read-only affinity
//! verification for Black Desert Online.
//!
//! # Anti-cheat–safe design
//!
//! BDO's anti-cheat **blocks write access to the running game process**:
//! calling `SetProcessAffinityMask` (or setting priority) on the live
//! `BlackDesert64.exe` fails with access-denied and, even where it appears to
//! work, is reverted by the protection layer. This crate is built around that
//! constraint:
//!
//! 1. **Affinity is applied by inheritance, never to the game.** We start
//!    `BlackDesertLauncher.exe` with the mask already set — either by creating
//!    the process `CREATE_SUSPENDED`, calling `SetProcessAffinityMask` on the
//!    *launcher's* handle, then `ResumeThread`
//!    ([`launch_with_affinity`]); or by writing a shortcut that runs
//!    `cmd /c start /affinity <hex> …` ([`create_shortcut`]). The launcher
//!    spawns `BlackDesert64.exe` as a child, and a child **inherits its
//!    parent's affinity mask**. We never open a write handle to the game.
//!
//! 2. **Verification is strictly read-only.** [`read_process_affinity`] opens
//!    the game with `PROCESS_QUERY_LIMITED_INFORMATION` only and calls
//!    `GetProcessAffinityMask`. It never requests `PROCESS_SET_INFORMATION` or
//!    any write right, so it cannot trip the anti-cheat.
//!
//! No injection, no write handles into the protected process — the entire
//! surface here is "launch with the right mask" plus "read back what stuck".
//!
//! # Platform support
//!
//! - **Windows** — the real implementation (this is the only OS BDO runs on).
//! - **Linux** — BDO's anti-cheat blocks Proton, so BDO does not run here. The
//!   Linux functions are honest *generic-game* helpers: `taskset` command
//!   generation and `.desktop` writing. See [`generate_taskset_command`].
//! - **macOS** — has no external-process affinity API, so the apply functions
//!   return [`LaunchError::UnsupportedPlatform`].
//!
//! The crate compiles on all three targets; only the Windows build carries the
//! `windows`/`sysinfo` dependencies.

mod common;
mod error;

pub use common::{
    build_cmd_arguments, build_launch_command_line, desktop_file_content, generate_taskset_command,
    mask_to_cores, mask_to_hex, parse_mask_hex, shortcut_description, validate_mask,
    ShortcutOptions, DEFAULT_SHORTCUT_NAME, GAME_EXE, LAUNCHER_EXE,
};
pub use error::LaunchError;

#[cfg(windows)]
mod win;
#[cfg(windows)]
pub use win::{create_shortcut, find_bdo_install, launch_with_affinity, read_process_affinity};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::{
    create_shortcut, find_bdo_install, launch_with_affinity, read_process_affinity,
    write_desktop_file,
};

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::{create_shortcut, find_bdo_install, launch_with_affinity, read_process_affinity};
