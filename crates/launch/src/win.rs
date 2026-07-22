//! Windows implementation — the real one. BDO runs only on Windows.
//!
//! See the crate-level docs for the anti-cheat-safety rationale. In short:
//! affinity is applied to `BlackDesertLauncher.exe` (which the game inherits),
//! and verification opens the game read-only.

use crate::common::{
    build_cmd_arguments, build_launch_command_line, shortcut_description, ShortcutOptions,
    DEFAULT_SHORTCUT_NAME, LAUNCHER_EXE,
};
use crate::error::LaunchError;
use std::mem::size_of;
use std::path::{Path, PathBuf};

use windows::core::{Interface, PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, IPersistFile, CLSCTX_INPROC_SERVER,
    COINIT_APARTMENTTHREADED,
};
use windows::Win32::System::Threading::{
    CreateProcessW, GetProcessAffinityMask, OpenProcess, ResumeThread, SetProcessAffinityMask,
    CREATE_SUSPENDED, PROCESS_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION, STARTUPINFOW,
};
use windows::Win32::UI::Shell::{IShellLinkDataList, IShellLinkW, ShellLink, SLDF_RUNAS_USER};

/// `RPC_E_CHANGED_MODE` (0x80010106) — returned by `CoInitializeEx` when COM is
/// already initialized on this thread with a different apartment model. Not a
/// real failure for our purposes.
const RPC_E_CHANGED_MODE: i32 = -2_147_417_850;

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Launch `BlackDesertLauncher.exe` directly with `mask` applied so the game
/// inherits it.
///
/// Uses `CreateProcessW` with `CREATE_SUSPENDED`, then `SetProcessAffinityMask`
/// on the **launcher's** process handle, then `ResumeThread`. We never open a
/// handle to the running game — the mask propagates by parent→child
/// inheritance, which is the only anti-cheat-safe path.
///
/// Working directory is the launcher's parent folder. `-steam` is appended when
/// `steam` is true. Returns the launcher's PID.
pub fn launch_with_affinity(
    launcher_path: &Path,
    mask: u64,
    steam: bool,
) -> Result<u32, LaunchError> {
    if mask == 0 {
        return Err(LaunchError::InvalidMask("0".to_string()));
    }
    if !launcher_path.exists() {
        return Err(LaunchError::PathNotFound(launcher_path.to_path_buf()));
    }
    let workdir = launcher_path
        .parent()
        .ok_or_else(|| LaunchError::NoParentDir(launcher_path.to_path_buf()))?;

    let cmdline = build_launch_command_line(&launcher_path.to_string_lossy(), steam);
    let mut cmdline_w = to_wide(&cmdline);
    let workdir_w = to_wide(&workdir.to_string_lossy());

    let si = STARTUPINFOW {
        cb: size_of::<STARTUPINFOW>() as u32,
        ..Default::default()
    };
    let mut pi = PROCESS_INFORMATION::default();

    unsafe {
        CreateProcessW(
            PCWSTR::null(),
            Some(PWSTR(cmdline_w.as_mut_ptr())),
            None,
            None,
            false,
            CREATE_SUSPENDED,
            None,
            PCWSTR(workdir_w.as_ptr()),
            &si,
            &mut pi,
        )
        .map_err(|e| LaunchError::Os(format!("CreateProcessW failed: {e}")))?;

        // Apply affinity to the launcher; the game will inherit it.
        let set_res = SetProcessAffinityMask(pi.hProcess, mask as usize);
        if let Err(e) = set_res {
            // Best-effort cleanup: resume so we don't leave a suspended zombie,
            // then close handles.
            ResumeThread(pi.hThread);
            let _ = CloseHandle(pi.hThread);
            let _ = CloseHandle(pi.hProcess);
            return Err(LaunchError::Os(format!(
                "SetProcessAffinityMask failed: {e}"
            )));
        }

        // ResumeThread returns the previous suspend count, or u32::MAX on error.
        if ResumeThread(pi.hThread) == u32::MAX {
            let _ = CloseHandle(pi.hThread);
            let _ = CloseHandle(pi.hProcess);
            return Err(LaunchError::Os("ResumeThread failed".to_string()));
        }

        let _ = CloseHandle(pi.hThread);
        let _ = CloseHandle(pi.hProcess);
    }

    // Keep the STARTUPINFOW `si` alive across the unsafe block.
    let _ = &si;
    Ok(pi.dwProcessId)
}

/// Create an optimized `.lnk` desktop shortcut.
///
/// The shortcut runs `cmd.exe /c start "" /affinity <mask> "<launcher>"` from
/// the launcher's folder, so `start` applies the affinity and the game inherits
/// it. The link is flagged **run-as-administrator** (elevation is needed for the
/// affinity/priority context the guide expects).
///
/// Returns the path of the written `.lnk`. Defaults to
/// `<Desktop>/Black Desert Online (Optimized).lnk` when
/// `opts.destination` is `None`.
///
/// COM is initialized per call (`CoInitializeEx`, apartment-threaded) and
/// uninitialized before returning.
pub fn create_shortcut(opts: ShortcutOptions) -> Result<PathBuf, LaunchError> {
    // Validate the mask up front (parse only; the string is what goes in args).
    let _ = crate::common::parse_mask_hex(&opts.mask_hex)?;
    if !opts.launcher_path.exists() {
        return Err(LaunchError::PathNotFound(opts.launcher_path.clone()));
    }
    let workdir = opts
        .launcher_path
        .parent()
        .ok_or_else(|| LaunchError::NoParentDir(opts.launcher_path.clone()))?
        .to_path_buf();

    let filename = opts
        .launcher_path
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or(LAUNCHER_EXE)
        .to_string();

    let dest = match &opts.destination {
        Some(p) => p.clone(),
        None => default_desktop_path()?,
    };

    let comspec =
        std::env::var("ComSpec").unwrap_or_else(|_| r"C:\Windows\System32\cmd.exe".into());
    let args = build_cmd_arguments(&opts.mask_hex, opts.steam, &filename);
    let description = shortcut_description(&opts.mask_hex);
    let icon = opts.launcher_path.to_string_lossy().to_string();

    unsafe {
        let hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let need_uninit = hr.is_ok();
        if hr.is_err() && hr.0 != RPC_E_CHANGED_MODE {
            return Err(LaunchError::Os(format!("CoInitializeEx failed: {hr:?}")));
        }

        let result = write_lnk(
            &comspec,
            &args,
            &workdir.to_string_lossy(),
            &description,
            &icon,
            &dest,
        );

        if need_uninit {
            CoUninitialize();
        }
        result?;
    }

    Ok(dest)
}

/// Inner COM work, kept in its own scope so the COM interface objects are
/// dropped before `CoUninitialize` runs in the caller.
///
/// # Run-as-admin approach
///
/// We use the **`IShellLinkDataList::SetFlags(SLDF_RUNAS_USER)`** COM approach:
/// cast the `IShellLinkW` to `IShellLinkDataList`, OR the `SLDF_RUNAS_USER` bit
/// into its flags before saving. This is the documented, non-fragile method and
/// avoids hand-patching byte 0x15 of the serialized `.lnk`.
unsafe fn write_lnk(
    target: &str,
    args: &str,
    workdir: &str,
    description: &str,
    icon: &str,
    dest: &Path,
) -> Result<(), LaunchError> {
    let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)
        .map_err(|e| LaunchError::Os(format!("CoCreateInstance(ShellLink) failed: {e}")))?;

    let target_w = to_wide(target);
    let args_w = to_wide(args);
    let workdir_w = to_wide(workdir);
    let desc_w = to_wide(description);
    let icon_w = to_wide(icon);

    link.SetPath(PCWSTR(target_w.as_ptr()))
        .map_err(|e| LaunchError::Os(format!("SetPath failed: {e}")))?;
    link.SetArguments(PCWSTR(args_w.as_ptr()))
        .map_err(|e| LaunchError::Os(format!("SetArguments failed: {e}")))?;
    link.SetWorkingDirectory(PCWSTR(workdir_w.as_ptr()))
        .map_err(|e| LaunchError::Os(format!("SetWorkingDirectory failed: {e}")))?;
    link.SetDescription(PCWSTR(desc_w.as_ptr()))
        .map_err(|e| LaunchError::Os(format!("SetDescription failed: {e}")))?;
    link.SetIconLocation(PCWSTR(icon_w.as_ptr()), 0)
        .map_err(|e| LaunchError::Os(format!("SetIconLocation failed: {e}")))?;

    // Flag run-as-administrator via IShellLinkDataList.
    let datalist: IShellLinkDataList = link
        .cast()
        .map_err(|e| LaunchError::Os(format!("cast to IShellLinkDataList failed: {e}")))?;
    let flags = datalist
        .GetFlags()
        .map_err(|e| LaunchError::Os(format!("GetFlags failed: {e}")))?;
    datalist
        .SetFlags(flags | SLDF_RUNAS_USER.0 as u32)
        .map_err(|e| LaunchError::Os(format!("SetFlags(SLDF_RUNAS_USER) failed: {e}")))?;

    // Persist to disk.
    let persist: IPersistFile = link
        .cast()
        .map_err(|e| LaunchError::Os(format!("cast to IPersistFile failed: {e}")))?;
    let dest_w = to_wide(&dest.to_string_lossy());
    persist
        .Save(PCWSTR(dest_w.as_ptr()), true)
        .map_err(|e| LaunchError::Os(format!("IPersistFile::Save failed: {e}")))?;

    Ok(())
}

fn default_desktop_path() -> Result<PathBuf, LaunchError> {
    let dirs = directories::UserDirs::new()
        .ok_or_else(|| LaunchError::Os("could not resolve user directories".to_string()))?;
    let desktop = dirs
        .desktop_dir()
        .ok_or_else(|| LaunchError::Os("could not resolve Desktop directory".to_string()))?;
    Ok(desktop.join(DEFAULT_SHORTCUT_NAME))
}

/// Read the CPU affinity mask of the running game — **read-only**.
///
/// Finds the `process_name` PID via `sysinfo`, opens it with
/// `PROCESS_QUERY_LIMITED_INFORMATION` **only** (never a write/set right, so the
/// anti-cheat is not tripped), and calls `GetProcessAffinityMask`. Returns
/// `Ok(None)` when the process is not running.
pub fn read_process_affinity(process_name: &str) -> Result<Option<u64>, LaunchError> {
    use sysinfo::{ProcessesToUpdate, System};

    let mut sys = System::new();
    sys.refresh_processes(ProcessesToUpdate::All, true);

    let pid = sys
        .processes_by_name(std::ffi::OsStr::new(process_name))
        .next()
        .map(|p| p.pid().as_u32());

    let Some(pid) = pid else {
        return Ok(None);
    };

    unsafe {
        let handle: HANDLE = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)
            .map_err(|e| LaunchError::Os(format!("OpenProcess failed: {e}")))?;

        let mut process_mask: usize = 0;
        let mut system_mask: usize = 0;
        let res = GetProcessAffinityMask(handle, &mut process_mask, &mut system_mask);
        let _ = CloseHandle(handle);
        res.map_err(|e| LaunchError::Os(format!("GetProcessAffinityMask failed: {e}")))?;

        Ok(Some(process_mask as u64))
    }
}

/// Best-effort detection of BDO installs.
///
/// Checks common Pearl Abyss / Steam locations and parses
/// `steamapps/libraryfolders.vdf` (simple text scan) for additional Steam
/// libraries. Returns every directory that contains `BlackDesertLauncher.exe`
/// (a `BlackDesert64.exe` presence also qualifies as a signal).
pub fn find_bdo_install() -> Vec<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();

    // Common Pearl Abyss / standalone locations.
    candidates.push(PathBuf::from(r"C:\Program Files (x86)\BlackDesert"));
    candidates.push(PathBuf::from(r"C:\Program Files (x86)\Black Desert"));
    candidates.push(PathBuf::from(r"C:\Program Files\BlackDesert"));

    // Default Steam library.
    let default_steam = PathBuf::from(r"C:\Program Files (x86)\Steam");
    candidates.push(steam_bdo_dir(&default_steam));

    // Additional Steam libraries from libraryfolders.vdf.
    let vdf = default_steam.join(r"steamapps\libraryfolders.vdf");
    for lib in parse_steam_library_paths(&vdf) {
        candidates.push(steam_bdo_dir(&lib));
    }

    let mut found: Vec<PathBuf> = Vec::new();
    for dir in candidates {
        let has_bdo = dir.join(LAUNCHER_EXE).exists() || dir.join(crate::common::GAME_EXE).exists();
        if has_bdo && !found.contains(&dir) {
            found.push(dir);
        }
    }
    found
}

fn steam_bdo_dir(steam_root: &Path) -> PathBuf {
    steam_root.join(r"steamapps\common\Black Desert Online")
}

/// Extract library `"path"` values from a `libraryfolders.vdf` file.
///
/// The VDF format is simple key/value text; a line-based scan is sufficient and
/// avoids pulling in a VDF parser crate. Missing/unreadable files yield an empty
/// list.
fn parse_steam_library_paths(vdf_path: &Path) -> Vec<PathBuf> {
    let Ok(text) = std::fs::read_to_string(vdf_path) else {
        return Vec::new();
    };
    let mut paths = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        // Lines look like:  "path"		"D:\\SteamLibrary"
        if let Some(rest) = line.strip_prefix("\"path\"") {
            if let Some(value) = rest.trim().trim_start_matches('"').split('"').next() {
                if !value.is_empty() {
                    // VDF escapes backslashes as \\ — unescape.
                    let unescaped = value.replace("\\\\", "\\");
                    paths.push(PathBuf::from(unescaped));
                }
            }
        }
    }
    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vdf_parse_extracts_paths() {
        let dir = std::env::temp_dir();
        let vdf = dir.join(format!("bdo_test_libfolders_{}.vdf", std::process::id()));
        let content = r#"
"libraryfolders"
{
    "0"
    {
        "path"		"C:\\Program Files (x86)\\Steam"
    }
    "1"
    {
        "path"		"D:\\SteamLibrary"
    }
}
"#;
        std::fs::write(&vdf, content).unwrap();
        let paths = parse_steam_library_paths(&vdf);
        std::fs::remove_file(&vdf).ok();
        assert_eq!(
            paths,
            vec![
                PathBuf::from(r"C:\Program Files (x86)\Steam"),
                PathBuf::from(r"D:\SteamLibrary"),
            ]
        );
    }

    #[test]
    fn vdf_parse_missing_file_is_empty() {
        assert!(parse_steam_library_paths(Path::new(r"Z:\nope\libraryfolders.vdf")).is_empty());
    }

    #[test]
    fn find_install_does_not_panic() {
        // Just exercise the path; result depends on the machine.
        let _ = find_bdo_install();
    }

    #[test]
    fn read_affinity_absent_process_is_none() {
        let r = read_process_affinity("definitely_not_a_real_process_xyz.exe").unwrap();
        assert_eq!(r, None);
    }

    #[test]
    #[ignore = "creates a real .lnk on disk; run manually with --ignored"]
    fn create_shortcut_writes_lnk() {
        // Use cmd.exe itself as a stand-in launcher so the path exists.
        let launcher = PathBuf::from(
            std::env::var("ComSpec").unwrap_or_else(|_| r"C:\Windows\System32\cmd.exe".into()),
        );
        let dest = std::env::temp_dir().join(format!("bdo_test_{}.lnk", std::process::id()));
        let opts = ShortcutOptions {
            launcher_path: launcher,
            mask_hex: "555".to_string(),
            steam: false,
            destination: Some(dest.clone()),
        };
        let written = create_shortcut(opts).expect("shortcut creation failed");
        assert!(written.exists(), "expected .lnk to exist");
        std::fs::remove_file(&written).ok();
    }
}
