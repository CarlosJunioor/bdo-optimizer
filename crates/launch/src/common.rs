//! Platform-independent, pure helpers: mask parsing/validation, command-line
//! construction, and Linux `taskset` / `.desktop` text generation.
//!
//! Everything here is deterministic and free of I/O so it can be unit-tested on
//! any operating system.

use crate::error::LaunchError;
use std::path::PathBuf;

/// The launcher executable BDO ships. Used for install detection and as the
/// default target file name inside generated shortcuts.
pub const LAUNCHER_EXE: &str = "BlackDesertLauncher.exe";

/// The running game process. Used only for *read-only* affinity verification.
pub const GAME_EXE: &str = "BlackDesert64.exe";

/// Parse a hexadecimal affinity mask string (with or without a `0x` prefix)
/// into a `u64`.
///
/// Rejects empty strings, non-hex input, and a zero mask (a zero mask would
/// pin the process to no cores at all).
pub fn parse_mask_hex(s: &str) -> Result<u64, LaunchError> {
    let trimmed = s.trim();
    let digits = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
        .unwrap_or(trimmed);
    if digits.is_empty() {
        return Err(LaunchError::InvalidMask(s.to_string()));
    }
    let mask =
        u64::from_str_radix(digits, 16).map_err(|_| LaunchError::InvalidMask(s.to_string()))?;
    if mask == 0 {
        return Err(LaunchError::InvalidMask(s.to_string()));
    }
    Ok(mask)
}

/// Format a mask as a lowercase hex string with no `0x` prefix, matching the
/// `start /affinity <hex>` convention used by the guide (e.g. `555`).
pub fn mask_to_hex(mask: u64) -> String {
    format!("{mask:x}")
}

/// Validate a non-zero mask, optionally against a known logical-core count.
///
/// When `logical_cores` is `Some(n)`, every set bit must fall within the
/// `0..n` range (and `n` must be `1..=64`, the width of the affinity mask).
pub fn validate_mask(mask: u64, logical_cores: Option<usize>) -> Result<(), LaunchError> {
    if mask == 0 {
        return Err(LaunchError::InvalidMask("0".to_string()));
    }
    if let Some(cores) = logical_cores {
        // Above 64 logical processors Windows splits the machine into processor
        // groups and an affinity mask addresses whichever group the process
        // landed in. We cannot know that group when the shortcut is written,
        // groups are not all the same size, and `start /affinity` has no way to
        // name one — so a mask that looks fine here can be silently rejected at
        // launch. Refusing is the honest answer; accepting by clamping to 64
        // would wave through masks that cannot be applied.
        //
        // ponytail: group-aware launching (`SetThreadGroupAffinity` on a
        // suspended process) would lift this. Worth it only if someone actually
        // runs BDO on a >64-thread machine.
        if cores == 0 || cores > 64 {
            return Err(LaunchError::MaskOutOfRange {
                mask,
                logical_cores: cores,
            });
        }
        // Bits at or above `cores` must all be clear.
        let allowed: u64 = if cores == 64 {
            u64::MAX
        } else {
            (1u64 << cores) - 1
        };
        if mask & !allowed != 0 {
            return Err(LaunchError::MaskOutOfRange {
                mask,
                logical_cores: cores,
            });
        }
    }
    Ok(())
}

/// Convert a mask to the sorted list of logical-core indices it selects.
pub fn mask_to_cores(mask: u64) -> Vec<usize> {
    (0..64).filter(|i| mask & (1u64 << i) != 0).collect()
}

/// Build the argument string passed to `cmd.exe` inside an optimized shortcut.
///
/// Produces, for mask `555`, `steam == false`, and launcher dir
/// `C:\Games\BDO`:
/// `/d /c cd /d "C:\Games\BDO" && start "" /affinity 555 "BlackDesertLauncher.exe"`
///
/// # Why `cd /d` and not just the working directory
///
/// Both consumers of this command cross a UAC elevation boundary — the
/// run-as-administrator `.lnk` shortcut and `launch_via_elevated_shell`'s
/// `ShellExecuteExW`/`runas`. Windows does **not** honor a requested working
/// directory across elevation: the elevated `cmd.exe` starts in `system32`, so a
/// bare relative launcher file name (`"BlackDesertLauncher.exe"`) cannot be
/// found — `start` fails and the hidden `cmd` window dies silently, the user's
/// "nothing happens". Making the command self-contained with an explicit
/// `cd /d "<launcher dir>"` first (exactly what the community guide's `.bat`
/// does) fixes this regardless of where the elevated shell starts.
///
/// `&&` ensures `start` only runs if the `cd` succeeded. The empty `""` after
/// `start` is the (ignored) window title, required so that `start` does not
/// mistake a quoted program path for its title argument.
///
/// # Why the path is validated, and why `/v:off` is explicit
///
/// Double quotes protect `&`, `|` and the rest of cmd's metacharacters, but they
/// do **not** stop variable expansion, which happens *before* quotes are parsed:
///
/// * `%NAME%` is always expanded, so a path containing one splices in whatever
///   that variable holds — and a same-user process can set the user's variables
///   — straight into a command line that runs through `runas` and through the
///   run-as-administrator shortcut.
/// * `!NAME!` is expanded too whenever delayed expansion is on, which `HKCU`
///   can enable for every `cmd` on the machine. `/v:off` in the command line
///   overrides that per invocation and is the authoritative fix.
///
/// Both characters are legal in Windows filenames, so neither can be assumed
/// away; `/v:off` closes the `!` case and the validation refuses both rather
/// than resting the elevated path on a single mechanism.
pub fn build_cmd_arguments(
    mask_hex: &str,
    steam: bool,
    launcher_dir: &str,
    launcher_filename: &str,
) -> Result<String, LaunchError> {
    reject_cmd_unsafe(launcher_dir)?;
    reject_cmd_unsafe(launcher_filename)?;
    let mut args = format!(
        "/d /v:off /c cd /d \"{launcher_dir}\" && start \"\" /affinity {mask_hex} \
         \"{launcher_filename}\""
    );
    if steam {
        args.push_str(" -steam");
    }
    Ok(args)
}

/// Refuse a path component that `cmd.exe` would not treat as literal text
/// inside double quotes.
///
/// Three classes get through quoting: `%` and `!` (variable expansion, see
/// [`build_cmd_arguments`]) and control characters — a raw `\r` or `\n` ends the
/// command and starts another. A double quote would also break out, but Windows
/// forbids it in a filename; it is rejected here anyway rather than relied upon.
pub fn reject_cmd_unsafe(path: &str) -> Result<(), LaunchError> {
    if path.contains(['%', '!', '"']) || path.chars().any(|c| c.is_control()) {
        return Err(LaunchError::UnsafeForCmd(path.to_string()));
    }
    Ok(())
}

/// Build the command line for a direct `CreateProcessW` launch: the quoted
/// launcher path, plus ` -steam` when requested.
pub fn build_launch_command_line(launcher_path: &str, steam: bool) -> String {
    let mut line = format!("\"{launcher_path}\"");
    if steam {
        line.push_str(" -steam");
    }
    line
}

/// How a launch was actually carried out.
///
/// `launch_with_affinity` can no longer always hand back a launcher PID: the
/// elevated-shell fallback (needed because `BlackDesertLauncher.exe`'s manifest
/// requires administrator) spawns an intermediate `cmd.exe`, not the launcher,
/// so its PID would be meaningless. This enum reports honestly which path ran.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchMethod {
    /// The launcher was started directly (the current process was already
    /// elevated) via `CreateProcessW` + `CREATE_SUSPENDED` +
    /// `SetProcessAffinityMask` + `ResumeThread`. `pid` is the launcher's PID and
    /// the mask was applied before its first instruction ran.
    Direct { pid: u32 },
    /// The launcher was started through an elevated `cmd.exe`
    /// (`ShellExecuteExW` with verb `runas`, which raises one UAC prompt). The
    /// elevated shell's `start /affinity` applied the mask and the game inherits
    /// it, but no launcher PID is available from this path.
    ElevatedShell,
}

/// One read of a running process's affinity.
///
/// `system_mask` is what `GetProcessAffinityMask` reports as available to that
/// process — the processors in its group. It is the only correct answer to "is
/// this launch unrestricted?": on a machine with more than 64 threads a process
/// in a 32-processor group reports `0xffff_ffff`, not `u64::MAX`, and deriving
/// the comparison from the machine-wide core count marks a perfectly normal
/// launch as restricted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessAffinity {
    pub pid: u32,
    pub process_mask: u64,
    pub system_mask: u64,
}

impl ProcessAffinity {
    /// Whether the process may use every processor its group offers.
    pub fn is_unrestricted(&self) -> bool {
        self.process_mask == self.system_mask
    }
}

/// Options controlling optimized-shortcut creation.
#[derive(Debug, Clone)]
pub struct ShortcutOptions {
    /// Full path to `BlackDesertLauncher.exe`.
    pub launcher_path: PathBuf,
    /// Affinity mask as a hex string (e.g. `"555"`).
    pub mask_hex: String,
    /// Append `-steam` to the launch command.
    pub steam: bool,
    /// Full destination path for the `.lnk` file. When `None`, the platform
    /// impl defaults to `<Desktop>/Black Desert Online (Optimized).lnk`.
    pub destination: Option<PathBuf>,
}

impl ShortcutOptions {
    /// Convenience constructor with `steam = false` and the default desktop
    /// destination.
    pub fn new(launcher_path: impl Into<PathBuf>, mask_hex: impl Into<String>) -> Self {
        Self {
            launcher_path: launcher_path.into(),
            mask_hex: mask_hex.into(),
            steam: false,
            destination: None,
        }
    }
}

/// The default file name for a generated shortcut.
pub const DEFAULT_SHORTCUT_NAME: &str = "Black Desert Online (Optimized).lnk";

/// A human-readable description embedded in the shortcut.
pub fn shortcut_description(mask_hex: &str) -> String {
    format!("Launches Black Desert Online with CPU affinity 0x{mask_hex} applied at the launcher so the game inherits it (anti-cheat safe).")
}

// ---------------------------------------------------------------------------
// Linux generic-game helpers
// ---------------------------------------------------------------------------

/// Build a `taskset` command line that pins `cmd` to the given logical cores.
///
/// Example: `generate_taskset_command(&[0, 2, 4], "mygame")` →
/// `taskset -c 0,2,4 mygame`.
///
/// Note: Black Desert Online does **not** run on Linux — its anti-cheat blocks
/// Proton. This helper exists for generic-game CPU pinning on Linux, not BDO.
pub fn generate_taskset_command(cores: &[usize], cmd: &str) -> String {
    if cores.is_empty() {
        return cmd.to_string();
    }
    let list = cores
        .iter()
        .map(|c| c.to_string())
        .collect::<Vec<_>>()
        .join(",");
    format!("taskset -c {list} {cmd}")
}

/// Render the text of a freedesktop `.desktop` launcher entry.
///
/// `exec` is the full `Exec=` line (typically the output of
/// [`generate_taskset_command`]). As with `taskset`, this targets generic Linux
/// games, not BDO.
pub fn desktop_file_content(name: &str, exec: &str, comment: &str) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name={name}\n\
         Comment={comment}\n\
         Exec={exec}\n\
         Terminal=false\n\
         Categories=Game;\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_and_prefixed_hex() {
        assert_eq!(parse_mask_hex("555").unwrap(), 0x555);
        assert_eq!(parse_mask_hex("0x555").unwrap(), 0x555);
        assert_eq!(parse_mask_hex("0XFF").unwrap(), 0xFF);
        assert_eq!(parse_mask_hex("  1 ").unwrap(), 1);
    }

    #[test]
    fn rejects_bad_masks() {
        assert!(parse_mask_hex("").is_err());
        assert!(parse_mask_hex("0x").is_err());
        assert!(parse_mask_hex("xyz").is_err());
        assert!(parse_mask_hex("0").is_err());
        assert!(parse_mask_hex("0x0").is_err());
    }

    #[test]
    fn mask_hex_roundtrip() {
        assert_eq!(mask_to_hex(0x555), "555");
        assert_eq!(mask_to_hex(1), "1");
        assert_eq!(parse_mask_hex(&mask_to_hex(0xABCDE)).unwrap(), 0xABCDE);
    }

    #[test]
    fn validate_within_core_count() {
        // 0x555 == cores 0,2,4,6,8,10 -> needs at least 11 logical cores.
        assert!(validate_mask(0x555, Some(12)).is_ok());
        assert!(validate_mask(0x555, Some(11)).is_ok());
        assert!(validate_mask(0x555, Some(10)).is_err());
        assert!(validate_mask(0x555, None).is_ok());
    }

    #[test]
    fn validate_edges() {
        assert!(validate_mask(0, Some(8)).is_err());
        assert!(validate_mask(1, Some(0)).is_err());
        assert!(validate_mask(1 << 63, Some(64)).is_ok());
    }

    /// Past 64 logical processors Windows uses processor groups, which
    /// `start /affinity` cannot name and whose sizes differ. A mask accepted
    /// here could be silently rejected at launch, so these are refused rather
    /// than waved through.
    #[test]
    fn multi_group_machines_are_refused_not_guessed() {
        assert!(validate_mask(0x5555, Some(128)).is_err());
        assert!(validate_mask(1 << 63, Some(96)).is_err());
        assert!(validate_mask(1 << 63, Some(64)).is_ok());
    }

    #[test]
    fn cores_from_mask() {
        assert_eq!(mask_to_cores(0x555), vec![0, 2, 4, 6, 8, 10]);
        assert_eq!(mask_to_cores(0b1011), vec![0, 1, 3]);
    }

    #[test]
    fn cmd_arguments_basic() {
        assert_eq!(
            build_cmd_arguments("555", false, r"C:\Games\BDO", "BlackDesertLauncher.exe").unwrap(),
            "/d /v:off /c cd /d \"C:\\Games\\BDO\" && start \"\" /affinity 555 \"BlackDesertLauncher.exe\""
        );
    }

    #[test]
    fn cmd_arguments_steam() {
        assert_eq!(
            build_cmd_arguments("554", true, r"C:\Games\BDO", "BlackDesertLauncher.exe").unwrap(),
            "/d /v:off /c cd /d \"C:\\Games\\BDO\" && start \"\" /affinity 554 \"BlackDesertLauncher.exe\" -steam"
        );
    }

    #[test]
    fn cmd_arguments_cd_prefix_handles_spaces_in_path() {
        // The elevated cmd starts in system32; the command must cd into the
        // launcher's own folder first, even when that folder contains spaces.
        let args = build_cmd_arguments(
            "555",
            false,
            r"C:\BDO EUW\BlackDesert",
            "BlackDesertLauncher.exe",
        )
        .unwrap();
        assert!(
            args.starts_with("/d /v:off /c cd /d \"C:\\BDO EUW\\BlackDesert\" && "),
            "expected a self-contained cd /d prefix, got: {args}"
        );
        assert_eq!(
            args,
            "/d /v:off /c cd /d \"C:\\BDO EUW\\BlackDesert\" && start \"\" /affinity 555 \"BlackDesertLauncher.exe\""
        );
    }

    /// Quotes do not stop `cmd` expanding `%NAME%`, so a path carrying one
    /// would splice an attacker-settable variable into a command line that runs
    /// elevated. Ampersands and spaces *are* safe inside quotes and must keep
    /// working — rejecting those would lock out ordinary install paths.
    #[test]
    fn cmd_arguments_reject_only_what_quoting_cannot_contain() {
        let build = |dir: &str, file: &str| build_cmd_arguments("555", false, dir, file);

        for dir in [
            r"C:\Games\%APPDATA%\BDO",
            "C:\\Games\\BDO\r\nstart calc",
            "C:\\Games\\\"BDO",
        ] {
            assert!(
                matches!(
                    build(dir, "BlackDesertLauncher.exe"),
                    Err(LaunchError::UnsafeForCmd(_))
                ),
                "must refuse to build a command for {dir:?}"
            );
        }
        // The file name is interpolated too, so it gets the same check.
        assert!(matches!(
            build(r"C:\Games\BDO", "%X%.exe"),
            Err(LaunchError::UnsafeForCmd(_))
        ));

        // Legal, common paths must still build: `&` and spaces are literal
        // inside double quotes.
        assert!(build(r"C:\Games & Mods\BDO", "BlackDesertLauncher.exe").is_ok());
        assert!(build(r"D:\SteamLibrary\steamapps\common\Black Desert Online", "x.exe").is_ok());
    }

    #[test]
    fn launch_command_line() {
        assert_eq!(
            build_launch_command_line(r"C:\Games\BDO\BlackDesertLauncher.exe", false),
            "\"C:\\Games\\BDO\\BlackDesertLauncher.exe\""
        );
        assert_eq!(
            build_launch_command_line(r"C:\Games\BDO\BlackDesertLauncher.exe", true),
            "\"C:\\Games\\BDO\\BlackDesertLauncher.exe\" -steam"
        );
    }

    #[test]
    fn taskset_command() {
        assert_eq!(
            generate_taskset_command(&[0, 2, 4], "mygame --opt"),
            "taskset -c 0,2,4 mygame --opt"
        );
        assert_eq!(generate_taskset_command(&[], "mygame"), "mygame");
        assert_eq!(generate_taskset_command(&[3], "g"), "taskset -c 3 g");
    }

    #[test]
    fn desktop_content_shape() {
        let d = desktop_file_content("My Game", "taskset -c 0,1 mygame", "Pinned game");
        assert!(d.starts_with("[Desktop Entry]\n"));
        assert!(d.contains("Name=My Game\n"));
        assert!(d.contains("Exec=taskset -c 0,1 mygame\n"));
        assert!(d.contains("Comment=Pinned game\n"));
        assert!(d.contains("Type=Application\n"));
        assert!(d.ends_with('\n'));
    }
}
