//! Windows implementation — the real one. BDO runs only on Windows.
//!
//! See the crate-level docs for the anti-cheat-safety rationale. In short:
//! affinity is applied to `BlackDesertLauncher.exe` (which the game inherits),
//! and verification opens the game read-only.

use crate::common::{
    build_cmd_arguments, build_launch_command_line, mask_to_hex, shortcut_description,
    LaunchMethod, ShortcutOptions, DEFAULT_SHORTCUT_NAME, LAUNCHER_EXE,
};
use crate::error::LaunchError;
use std::ffi::c_void;
use std::mem::size_of;
use std::path::{Path, PathBuf};

use windows::core::{Interface, HRESULT, PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, ERROR_CANCELLED, ERROR_ELEVATION_REQUIRED, HANDLE};
use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, IPersistFile, CLSCTX_INPROC_SERVER,
    COINIT_APARTMENTTHREADED,
};
use windows::Win32::System::Threading::{
    CreateProcessW, GetCurrentProcess, GetProcessAffinityMask, OpenProcess, OpenProcessToken,
    ResumeThread, SetProcessAffinityMask, CREATE_SUSPENDED, PROCESS_INFORMATION,
    PROCESS_QUERY_LIMITED_INFORMATION, STARTUPINFOW,
};
use windows::Win32::UI::Shell::{
    IShellLinkDataList, IShellLinkW, ShellExecuteExW, ShellLink, SEE_MASK_NOCLOSEPROCESS,
    SHELLEXECUTEINFOW, SLDF_RUNAS_USER,
};
use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;

/// `RPC_E_CHANGED_MODE` (0x80010106) — returned by `CoInitializeEx` when COM is
/// already initialized on this thread with a different apartment model. Not a
/// real failure for our purposes.
const RPC_E_CHANGED_MODE: i32 = -2_147_417_850;

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Launch `BlackDesertLauncher.exe` with `mask` applied so the game inherits it.
///
/// # Two paths, and why
///
/// `BlackDesertLauncher.exe` ships an embedded manifest marked
/// `requireAdministrator`, so Windows will only start it elevated. `CreateProcess`
/// **cannot** perform UAC elevation — from a non-elevated caller it fails with
/// `ERROR_ELEVATION_REQUIRED` (740) and the game never opens (the user's
/// "nothing happens"). We therefore choose the path by our own elevation state:
///
/// - **Elevated caller** → the superior direct path: `CreateProcessW` with
///   `CREATE_SUSPENDED`, `SetProcessAffinityMask` on the *launcher's* handle,
///   then `ResumeThread`. The mask is in effect before the launcher runs a single
///   instruction. Returns [`LaunchMethod::Direct`] with the launcher PID. If this
///   path still somehow reports `ERROR_ELEVATION_REQUIRED`, we fall back
///   (handled reactively as well as proactively).
/// - **Non-elevated caller** → `ShellExecuteExW` with verb `runas` on `cmd.exe`,
///   running
///   `/c cd /d "<launcher dir>" && start "" /affinity <mask> "BlackDesertLauncher.exe"`
///   (+ `-steam`). The command `cd`s into the launcher folder itself — Windows
///   does not carry the requested working directory across the UAC elevation
///   boundary, so the elevated shell starts in `system32` and a relative
///   launcher name would not resolve. This raises **one** UAC prompt; the
///   elevated shell's `start /affinity` applies the mask and the game inherits
///   it — the same technique the community guide's `.bat` shortcut uses. Returns
///   [`LaunchMethod::ElevatedShell`] (no launcher PID is available here). If the
///   user dismisses the UAC prompt, this returns [`LaunchError::Cancelled`].
///
/// Either way we never open a write handle to the running game — the mask
/// propagates purely by parent→child inheritance, the only anti-cheat-safe path.
pub fn launch_with_affinity(
    launcher_path: &Path,
    mask: u64,
    steam: bool,
) -> Result<LaunchMethod, LaunchError> {
    if mask == 0 {
        return Err(LaunchError::InvalidMask("0".to_string()));
    }
    if !launcher_path.exists() {
        return Err(LaunchError::PathNotFound(launcher_path.to_path_buf()));
    }
    let workdir = launcher_path
        .parent()
        .ok_or_else(|| LaunchError::NoParentDir(launcher_path.to_path_buf()))?;

    // Proactive: only attempt the direct CreateProcess path when we are already
    // elevated (otherwise it is guaranteed to fail with ERROR_ELEVATION_REQUIRED).
    if is_elevated() {
        match launch_direct(launcher_path, workdir, mask, steam)? {
            DirectOutcome::Launched(pid) => return Ok(LaunchMethod::Direct { pid }),
            // Reactive: the manifest still demanded elevation despite our token
            // check — fall through to the elevated-shell path.
            DirectOutcome::NeedsElevation => {}
        }
    }

    launch_via_elevated_shell(launcher_path, workdir, mask, steam)
}

/// Result of the direct `CreateProcessW` path.
enum DirectOutcome {
    /// The launcher started; carries its PID.
    Launched(u32),
    /// `CreateProcessW` returned `ERROR_ELEVATION_REQUIRED`; the caller should
    /// fall back to the elevated-shell path.
    NeedsElevation,
}

/// Direct launch: `CreateProcessW` + `CREATE_SUSPENDED` +
/// `SetProcessAffinityMask` on the launcher handle + `ResumeThread`. Applies the
/// mask before the first instruction. Only valid when already elevated.
fn launch_direct(
    launcher_path: &Path,
    workdir: &Path,
    mask: u64,
    steam: bool,
) -> Result<DirectOutcome, LaunchError> {
    let cmdline = build_launch_command_line(&launcher_path.to_string_lossy(), steam);
    let mut cmdline_w = to_wide(&cmdline);
    let workdir_w = to_wide(&workdir.to_string_lossy());

    let si = STARTUPINFOW {
        cb: size_of::<STARTUPINFOW>() as u32,
        ..Default::default()
    };
    let mut pi = PROCESS_INFORMATION::default();

    unsafe {
        if let Err(e) = CreateProcessW(
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
        ) {
            if e.code() == HRESULT::from_win32(ERROR_ELEVATION_REQUIRED.0) {
                return Ok(DirectOutcome::NeedsElevation);
            }
            return Err(LaunchError::Os(format!("CreateProcessW failed: {e}")));
        }

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
    Ok(DirectOutcome::Launched(pi.dwProcessId))
}

/// Build the `cmd.exe` parameter string for the elevated-shell launch path.
///
/// Pure so it can be unit-tested. Delegates to [`build_cmd_arguments`] with the
/// mask rendered as lowercase hex, yielding e.g.
/// `/c cd /d "<launcher dir>" && start "" /affinity 555 "BlackDesertLauncher.exe"`
/// (+ ` -steam`). The `cd /d` prefix is essential here: this path runs the
/// command through `ShellExecuteExW`/`runas`, and the elevated `cmd.exe` starts
/// in `system32` — a bare launcher file name would not resolve.
fn elevated_shell_params(
    mask: u64,
    steam: bool,
    launcher_dir: &str,
    launcher_filename: &str,
) -> String {
    build_cmd_arguments(&mask_to_hex(mask), steam, launcher_dir, launcher_filename)
}

/// Elevated-shell fallback: `ShellExecuteExW` verb `runas` on `cmd.exe` running
/// `cd /d "<launcher dir>" && start /affinity <mask> "<launcher>"`. Triggers one
/// UAC prompt.
///
/// The command `cd`s into the launcher's own folder first because Windows does
/// not honor `lpDirectory` across the elevation boundary — the elevated shell
/// otherwise starts in `system32` and cannot find the launcher. The `cmd` window
/// is hidden (`SW_HIDE`) since it exits immediately after `start`. A cancelled
/// UAC prompt surfaces as [`LaunchError::Cancelled`].
fn launch_via_elevated_shell(
    launcher_path: &Path,
    workdir: &Path,
    mask: u64,
    steam: bool,
) -> Result<LaunchMethod, LaunchError> {
    let filename = launcher_path
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or(LAUNCHER_EXE);
    let params = elevated_shell_params(mask, steam, &workdir.to_string_lossy(), filename);
    let comspec =
        std::env::var("ComSpec").unwrap_or_else(|_| r"C:\Windows\System32\cmd.exe".into());

    let verb_w = to_wide("runas");
    let file_w = to_wide(&comspec);
    let params_w = to_wide(&params);
    let dir_w = to_wide(&workdir.to_string_lossy());

    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: PCWSTR(verb_w.as_ptr()),
        lpFile: PCWSTR(file_w.as_ptr()),
        lpParameters: PCWSTR(params_w.as_ptr()),
        lpDirectory: PCWSTR(dir_w.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };

    unsafe {
        match ShellExecuteExW(&mut info) {
            Ok(()) => {
                // We requested SEE_MASK_NOCLOSEPROCESS, so close cmd's handle.
                if !info.hProcess.is_invalid() {
                    let _ = CloseHandle(info.hProcess);
                }
                Ok(LaunchMethod::ElevatedShell)
            }
            Err(e) => {
                if e.code() == HRESULT::from_win32(ERROR_CANCELLED.0) {
                    Err(LaunchError::Cancelled)
                } else {
                    Err(LaunchError::Os(format!(
                        "ShellExecuteExW(runas) failed: {e}"
                    )))
                }
            }
        }
    }
}

/// Whether the current process is running elevated (admin).
///
/// Opens the process token (`OpenProcessToken` + `TOKEN_QUERY`) and reads
/// `TokenElevation`. Any failure is treated as "not elevated" so we fall back to
/// the UAC path rather than attempting a direct launch that would fail.
fn is_elevated() -> bool {
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION::default();
        let mut ret_len = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elevation as *mut _ as *mut c_void),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut ret_len,
        )
        .is_ok();
        let _ = CloseHandle(token);
        ok && elevation.TokenIsElevated != 0
    }
}

/// Create an optimized `.lnk` desktop shortcut.
///
/// The shortcut runs
/// `cmd.exe /c cd /d "<launcher dir>" && start "" /affinity <mask> "<launcher>"`,
/// so `start` applies the affinity and the game inherits it. The link is flagged
/// **run-as-administrator** (elevation is needed for the affinity/priority
/// context the guide expects).
///
/// The command `cd`s into the launcher folder itself rather than relying on the
/// shortcut's working directory: because the link is run-as-admin, it crosses a
/// UAC elevation boundary, and Windows does not carry the shortcut's working
/// directory across that boundary — the elevated `cmd.exe` would start in
/// `system32` and fail to find a relative launcher name. The `WorkingDirectory`
/// is still set below (harmless, and correct for any non-elevated context), but
/// the command no longer depends on it.
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
    let args = build_cmd_arguments(
        &opts.mask_hex,
        opts.steam,
        &workdir.to_string_lossy(),
        &filename,
    );
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
/// Sources, in order:
/// 1. Common Pearl Abyss / standalone locations (hardcoded).
/// 2. The default Steam library plus any extra libraries parsed from
///    `steamapps/libraryfolders.vdf` (simple text scan).
/// 3. The Windows uninstall registry — both
///    `HKLM\SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\*`
///    and the native `HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\*`
///    — for any entry whose `DisplayName` mentions "Black Desert", using its
///    `InstallLocation`. This is what catches non-default installs like
///    `C:\BDO EUW\BlackDesert` or `C:\Pearlabyss\BlackDesert`.
///
/// Returns every directory that actually contains `BlackDesertLauncher.exe`
/// (a `BlackDesert64.exe` presence also qualifies as a signal), deduplicated
/// case-insensitively with trailing slashes normalized. Read-only and
/// non-panicking.
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

    // Uninstall-registry installs (both WOW6432Node and native views).
    for (display_name, install_location) in read_uninstall_entries() {
        if let Some(dir) = bdo_install_dir_from_entry(&display_name, &install_location) {
            candidates.push(dir);
        }
    }

    // Keep only dirs that really contain the launcher/game, then dedup.
    let existing: Vec<PathBuf> = candidates
        .into_iter()
        .filter(|dir| dir.join(LAUNCHER_EXE).exists() || dir.join(crate::common::GAME_EXE).exists())
        .collect();
    dedupe_dirs(existing)
}

/// Pure filter for one uninstall-registry entry: if `display_name` names Black
/// Desert (case-insensitive) and `install_location` is non-empty, return it as a
/// `PathBuf`. Separated from the registry I/O so it can be unit-tested.
fn bdo_install_dir_from_entry(display_name: &str, install_location: &str) -> Option<PathBuf> {
    if !display_name.to_ascii_lowercase().contains("black desert") {
        return None;
    }
    let loc = install_location.trim().trim_matches('"');
    if loc.is_empty() {
        return None;
    }
    Some(PathBuf::from(loc))
}

/// Normalize a directory path for case-insensitive, trailing-slash-insensitive
/// comparison (lowercased, trailing `\` / `/` stripped).
fn normalize_dir_key(dir: &Path) -> String {
    dir.to_string_lossy()
        .trim_end_matches(['\\', '/'])
        .to_ascii_lowercase()
}

/// Deduplicate directories, preserving first-seen order, comparing with
/// [`normalize_dir_key`] (case-insensitive, trailing slash ignored).
fn dedupe_dirs(dirs: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen: Vec<String> = Vec::new();
    let mut out: Vec<PathBuf> = Vec::new();
    for dir in dirs {
        let key = normalize_dir_key(&dir);
        if !seen.contains(&key) {
            seen.push(key);
            out.push(dir);
        }
    }
    out
}

/// Enumerate `(DisplayName, InstallLocation)` pairs from both the WOW6432Node
/// (32-bit) and native (64-bit) uninstall registry hives under
/// `HKLM\SOFTWARE\...\Uninstall`. Read-only; failures yield fewer/no entries and
/// never panic.
fn read_uninstall_entries() -> Vec<(String, String)> {
    use windows::Win32::System::Registry::{KEY_READ, KEY_WOW64_32KEY, KEY_WOW64_64KEY};

    const UNINSTALL_PATH: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall";

    let mut out = Vec::new();
    // The 32-bit and 64-bit registry views expose different subkey sets; scan both.
    for view in [KEY_WOW64_32KEY, KEY_WOW64_64KEY] {
        read_uninstall_view(UNINSTALL_PATH, KEY_READ.0 | view.0, &mut out);
    }
    out
}

/// Enumerate one registry view of the uninstall hive, appending
/// `(DisplayName, InstallLocation)` pairs to `out`.
fn read_uninstall_view(subkey: &str, access: u32, out: &mut Vec<(String, String)>) {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegEnumKeyExW, RegOpenKeyExW, HKEY, HKEY_LOCAL_MACHINE, REG_SAM_FLAGS,
    };

    unsafe {
        let subkey_w = to_wide(subkey);
        let mut root = HKEY::default();
        if RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(subkey_w.as_ptr()),
            Some(0),
            REG_SAM_FLAGS(access),
            &mut root,
        ) != ERROR_SUCCESS
        {
            return;
        }

        let mut index = 0u32;
        loop {
            // Max registry key name length is 255; give room for the NUL.
            let mut name_buf = [0u16; 256];
            let mut name_len = name_buf.len() as u32;
            let rc = RegEnumKeyExW(
                root,
                index,
                Some(PWSTR(name_buf.as_mut_ptr())),
                &mut name_len,
                None,
                None,
                None,
                None,
            );
            if rc != ERROR_SUCCESS {
                break;
            }
            index += 1;

            let child_name = String::from_utf16_lossy(&name_buf[..name_len as usize]);
            let child_path = format!("{subkey}\\{child_name}");
            let child_path_w = to_wide(&child_path);
            let mut child = HKEY::default();
            if RegOpenKeyExW(
                HKEY_LOCAL_MACHINE,
                PCWSTR(child_path_w.as_ptr()),
                Some(0),
                REG_SAM_FLAGS(access),
                &mut child,
            ) != ERROR_SUCCESS
            {
                continue;
            }

            let display_name = read_reg_string(child, "DisplayName").unwrap_or_default();
            let install_location = read_reg_string(child, "InstallLocation").unwrap_or_default();
            let _ = RegCloseKey(child);

            if !display_name.is_empty() {
                out.push((display_name, install_location));
            }
        }

        let _ = RegCloseKey(root);
    }
}

/// Read a `REG_SZ` value by name from an open key, returning `None` when the
/// value is missing or not a string.
fn read_reg_string(key: windows::Win32::System::Registry::HKEY, value: &str) -> Option<String> {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{RegQueryValueExW, REG_SZ, REG_VALUE_TYPE};

    unsafe {
        let value_w = to_wide(value);
        let mut ty = REG_VALUE_TYPE::default();
        let mut size: u32 = 0;
        // First call: obtain the required byte size.
        if RegQueryValueExW(
            key,
            PCWSTR(value_w.as_ptr()),
            None,
            Some(&mut ty),
            None,
            Some(&mut size),
        ) != ERROR_SUCCESS
        {
            return None;
        }
        if ty != REG_SZ || size == 0 {
            return None;
        }
        // Size is in bytes; REG_SZ is UTF-16.
        let mut buf = vec![0u8; size as usize];
        let mut got = size;
        if RegQueryValueExW(
            key,
            PCWSTR(value_w.as_ptr()),
            None,
            Some(&mut ty),
            Some(buf.as_mut_ptr()),
            Some(&mut got),
        ) != ERROR_SUCCESS
        {
            return None;
        }
        let wide: Vec<u16> = buf[..got as usize]
            .chunks_exact(2)
            .map(|c| u16::from_ne_bytes([c[0], c[1]]))
            .collect();
        let s = String::from_utf16_lossy(&wide);
        let s = s.trim_end_matches('\0').to_string();
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    }
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
    fn elevated_shell_params_matches_cmd_arguments() {
        assert_eq!(
            elevated_shell_params(0x555, false, r"C:\Games\BDO", "BlackDesertLauncher.exe"),
            "/c cd /d \"C:\\Games\\BDO\" && start \"\" /affinity 555 \"BlackDesertLauncher.exe\""
        );
        assert_eq!(
            elevated_shell_params(0x554, true, r"C:\Games\BDO", "BlackDesertLauncher.exe"),
            "/c cd /d \"C:\\Games\\BDO\" && start \"\" /affinity 554 \"BlackDesertLauncher.exe\" -steam"
        );
    }

    #[test]
    fn elevated_shell_params_cds_into_path_with_spaces() {
        let params = elevated_shell_params(
            0x555,
            false,
            r"C:\BDO EUW\BlackDesert",
            "BlackDesertLauncher.exe",
        );
        assert!(
            params.starts_with("/c cd /d \"C:\\BDO EUW\\BlackDesert\" && "),
            "elevated command must cd into the launcher dir first, got: {params}"
        );
    }

    #[test]
    fn registry_entry_filter_matches_black_desert() {
        assert_eq!(
            bdo_install_dir_from_entry("Black Desert Online", r"C:\BDO EUW\BlackDesert\"),
            Some(PathBuf::from(r"C:\BDO EUW\BlackDesert\"))
        );
        // Case-insensitive on the display name.
        assert_eq!(
            bdo_install_dir_from_entry("BLACK DESERT", r"C:\Pearlabyss\BlackDesert"),
            Some(PathBuf::from(r"C:\Pearlabyss\BlackDesert"))
        );
        // Surrounding quotes/whitespace on the location are trimmed.
        assert_eq!(
            bdo_install_dir_from_entry("Black Desert", "  \"D:\\Games\\BDO\"  "),
            Some(PathBuf::from(r"D:\Games\BDO"))
        );
    }

    #[test]
    fn registry_entry_filter_rejects_others_and_empty() {
        assert_eq!(bdo_install_dir_from_entry("Steam", r"C:\Steam"), None);
        assert_eq!(bdo_install_dir_from_entry("Black Desert", ""), None);
        assert_eq!(bdo_install_dir_from_entry("Black Desert", "   "), None);
    }

    #[test]
    fn dedupe_is_case_and_trailing_slash_insensitive() {
        let dirs = vec![
            PathBuf::from(r"C:\BDO EUW\BlackDesert\"),
            PathBuf::from(r"C:\BDO EUW\BlackDesert"),
            PathBuf::from(r"c:\bdo euw\blackdesert"),
            PathBuf::from(r"C:\Pearlabyss\BlackDesert"),
        ];
        let out = dedupe_dirs(dirs);
        assert_eq!(
            out,
            vec![
                PathBuf::from(r"C:\BDO EUW\BlackDesert\"),
                PathBuf::from(r"C:\Pearlabyss\BlackDesert"),
            ]
        );
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
