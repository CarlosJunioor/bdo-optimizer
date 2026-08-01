//! Two Windows settings the guide asks for, both applied for the current user
//! only: "Optimizations for windowed games", and PCI Express link-state power
//! management.
//!
//! Both are deliberately narrow:
//!
//! * Neither needs administrator rights — one is `HKEY_CURRENT_USER`, the other
//!   is the user's own power scheme. Nothing machine-wide is written, which
//!   keeps the app's promise that it changes nothing global.
//! * Neither needs a reboot.
//! * Both record their previous state so the change can be undone, and both
//!   distinguish **"the value was absent"** from **"the value was zero"**.
//!   Those are different states in Windows, and restoring the wrong one leaves
//!   a machine in a configuration it never had.

/// What a setting looked like before we touched it, so it can be put back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Previous {
    /// The value did not exist. Undo means deleting it, not writing a zero.
    Absent,
    /// The value existed, with these contents.
    Was(String),
}

impl Previous {
    /// Serialise for the on-disk undo record.
    ///
    /// Written as a file rather than kept in memory because the undo has to
    /// survive closing the app — a setting you cannot reverse tomorrow is not
    /// really reversible. The two states are encoded distinctly: `absent` is
    /// not the same as an empty `was:`.
    pub fn encode(&self) -> String {
        match self {
            Previous::Absent => "absent".to_string(),
            Previous::Was(text) => format!("was:{text}"),
        }
    }

    /// Parse a record written by [`Previous::encode`].
    pub fn decode(text: &str) -> Option<Previous> {
        let text = text.trim_end_matches(['\r', '\n']);
        match text {
            "absent" => Some(Previous::Absent),
            _ => text
                .strip_prefix("was:")
                .map(|v| Previous::Was(v.to_string())),
        }
    }
}

/// The registry value holding the per-user DirectX toggles.
pub const DX_KEY: &str = r"Software\Microsoft\DirectX\UserGpuPreferences";
pub const DX_VALUE: &str = "DirectXUserGlobalSettings";
/// "Optimizations for windowed games" — the guide's fix for uncapped framerates
/// in windowed mode.
pub const SWAP_EFFECT_UPGRADE: &str = "SwapEffectUpgradeEnable";

/// Remove `key` from a `DirectXUserGlobalSettings` string, preserving every
/// other entry.
pub fn drop_dx_setting(existing: &str, key: &str) -> String {
    let mut out = String::with_capacity(existing.len());
    for entry in existing.split(';').filter(|e| !e.trim().is_empty()) {
        let name = entry.split('=').next().unwrap_or("").trim();
        if !name.eq_ignore_ascii_case(key) {
            out.push_str(entry.trim());
            out.push(';');
        }
    }
    out
}

/// Read `key` out of a `DirectXUserGlobalSettings` string, if it is present.
pub fn dx_setting_value(existing: &str, key: &str) -> Option<String> {
    existing
        .split(';')
        .filter_map(|e| e.split_once('='))
        .find(|(name, _)| name.trim().eq_ignore_ascii_case(key))
        .map(|(_, value)| value.trim().to_string())
}

/// Undo our edit to a *current* `DirectXUserGlobalSettings` value, using the
/// recorded original only to decide what `SwapEffectUpgradeEnable` should go
/// back to.
///
/// Writing the recorded snapshot back wholesale is what a naive undo does, and
/// it is wrong: this value is shared. Windows and other apps add their own keys
/// to it, so restoring a snapshot taken before the optimization silently deletes
/// every unrelated change made since — including ones the user made deliberately
/// in the Graphics settings page. Only the key this app owns is put back.
pub fn undo_windowed_optimization(current: &str, previous: &Previous) -> String {
    let original = match previous {
        Previous::Absent => None,
        Previous::Was(text) => dx_setting_value(text, SWAP_EFFECT_UPGRADE),
    };
    match original {
        // The key existed before us: restore exactly the value it had.
        Some(value) => merge_dx_setting(current, SWAP_EFFECT_UPGRADE, &value),
        // It did not exist before us, so undo means removing it again — not
        // writing a zero, which is a different state to Windows.
        None => drop_dx_setting(current, SWAP_EFFECT_UPGRADE),
    }
}

/// Merge `key=value;` into a `DirectXUserGlobalSettings` string, preserving any
/// other entries already present.
///
/// The value is a flat `k=v;k=v;` list that other Windows features also write
/// to, so it must be parsed and merged rather than overwritten — replacing it
/// wholesale would silently drop settings we know nothing about.
pub fn merge_dx_setting(existing: &str, key: &str, value: &str) -> String {
    let mut out = String::with_capacity(existing.len() + key.len() + value.len() + 2);
    let mut replaced = false;
    for entry in existing.split(';').filter(|e| !e.trim().is_empty()) {
        let name = entry.split('=').next().unwrap_or("").trim();
        if name.eq_ignore_ascii_case(key) {
            if replaced {
                continue; // drop duplicates of the key we own
            }
            out.push_str(&format!("{key}={value};"));
            replaced = true;
        } else {
            out.push_str(entry.trim());
            out.push(';');
        }
    }
    if !replaced {
        out.push_str(&format!("{key}={value};"));
    }
    out
}

/// Whether a `DirectXUserGlobalSettings` string already enables `key`.
pub fn dx_setting_is_on(existing: &str, key: &str) -> bool {
    existing
        .split(';')
        .filter_map(|e| e.split_once('='))
        .any(|(name, value)| name.trim().eq_ignore_ascii_case(key) && value.trim() == "1")
}

#[cfg(windows)]
pub use imp::*;

#[cfg(windows)]
mod imp {
    use super::{Previous, DX_KEY, DX_VALUE, SWAP_EFFECT_UPGRADE};

    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
    use windows::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegQueryValueExW, RegSetValueExW, HKEY,
        HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ,
    };

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// Open (creating if needed) the per-user DirectX preferences key.
    fn open_dx_key() -> Result<HKEY, String> {
        let path = wide(DX_KEY);
        let mut key = HKEY::default();
        // SAFETY: `path` is NUL-terminated and outlives the call; `key` is a
        // valid out-parameter.
        let status = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR(path.as_ptr()),
                None,
                None,
                REG_OPTION_NON_VOLATILE,
                KEY_READ | KEY_WRITE,
                None,
                &mut key,
                None,
            )
        };
        if status != ERROR_SUCCESS {
            return Err(format!("could not open HKCU\\{DX_KEY}: {status:?}"));
        }
        Ok(key)
    }

    /// Read `DirectXUserGlobalSettings`, distinguishing absent from empty.
    pub fn read_dx_settings() -> Result<Previous, String> {
        let key = open_dx_key()?;
        let name = wide(DX_VALUE);
        let mut size = 0u32;
        // SAFETY: querying the size first with a null buffer is the documented
        // two-call pattern; all pointers are valid for the call.
        let status = unsafe {
            RegQueryValueExW(
                key,
                PCWSTR(name.as_ptr()),
                None,
                None,
                None,
                Some(&mut size),
            )
        };
        if status == ERROR_FILE_NOT_FOUND {
            // SAFETY: `key` came from RegCreateKeyExW and is not used again.
            unsafe {
                let _ = RegCloseKey(key);
            };
            return Ok(Previous::Absent);
        }
        if status != ERROR_SUCCESS {
            unsafe {
                let _ = RegCloseKey(key);
            };
            return Err(format!("could not read {DX_VALUE}: {status:?}"));
        }
        let mut buf = vec![0u8; size as usize];
        // SAFETY: `buf` is sized from the query above.
        let status = unsafe {
            RegQueryValueExW(
                key,
                PCWSTR(name.as_ptr()),
                None,
                None,
                Some(buf.as_mut_ptr()),
                Some(&mut size),
            )
        };
        unsafe {
            let _ = RegCloseKey(key);
        };
        if status != ERROR_SUCCESS {
            return Err(format!("could not read {DX_VALUE}: {status:?}"));
        }
        let units: Vec<u16> = buf
            .chunks_exact(2)
            .map(|p| u16::from_le_bytes([p[0], p[1]]))
            .take_while(|&u| u != 0)
            .collect();
        Ok(Previous::Was(String::from_utf16_lossy(&units)))
    }

    /// Write `DirectXUserGlobalSettings`, or delete it to restore "absent".
    pub fn write_dx_settings(value: &Previous) -> Result<(), String> {
        let key = open_dx_key()?;
        let name = wide(DX_VALUE);
        let status = match value {
            Previous::Absent => {
                // SAFETY: NUL-terminated name, valid key.
                unsafe { RegDeleteValueW(key, PCWSTR(name.as_ptr())) }
            }
            Previous::Was(text) => {
                let data = wide(text);
                // SAFETY: reinterpreting the UTF-16 buffer as the byte slice
                // REG_SZ expects. `data` outlives the borrow and the call, and
                // u16 has no padding or invalid bit patterns as bytes.
                let bytes = unsafe {
                    std::slice::from_raw_parts(
                        data.as_ptr().cast::<u8>(),
                        std::mem::size_of_val(&data[..]),
                    )
                };
                // SAFETY: NUL-terminated name and a buffer valid for the call.
                unsafe { RegSetValueExW(key, PCWSTR(name.as_ptr()), None, REG_SZ, Some(bytes)) }
            }
        };
        unsafe {
            let _ = RegCloseKey(key);
        };
        // Deleting a value that is already gone is success for our purposes.
        if status == ERROR_SUCCESS || status == ERROR_FILE_NOT_FOUND {
            Ok(())
        } else {
            Err(format!("could not write {DX_VALUE}: {status:?}"))
        }
    }

    /// The name Windows gives this setting, for user-facing messages.
    pub const WINDOWED_OPT_LABEL: &str = "Optimizations for windowed games";

    /// Where the undo record lives, beside the saved benchmark sessions.
    fn undo_record_path() -> Option<std::path::PathBuf> {
        bdo_bench::SessionStore::default_store()
            .ok()
            .map(|s| s.dir().join("windowed-optimizations-undo.txt"))
    }

    /// Write a file without following a reparse point planted at its path.
    ///
    /// This record sits at a predictable name under the per-user data
    /// directory, and the app may be elevated. `std::fs::write` follows a link,
    /// so one planted here would be followed with the administrator token.
    fn write_no_follow(path: &std::path::Path, text: &str) -> std::io::Result<()> {
        use std::io::Write;
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()
    }

    /// Serialise apply and restore of this setting across processes: both are
    /// read-modify-write over one registry value plus one record file, and two
    /// instances interleaving can leave the value changed with no provenance.
    fn windowed_lock() -> Result<crate::privdir::MutexGuard, String> {
        crate::privdir::lock("bdo-optimizer-windowed-optimizations")
    }

    /// Enable the setting, writing an undo record first.
    ///
    /// Returns `Ok(true)` if it changed something, `Ok(false)` if it was already
    /// on. The undo record is written *before* the registry, and only when one
    /// does not already exist — the oldest record is the one that describes the
    /// user's real original state.
    pub fn enable_windowed_optimizations_recording_undo() -> Result<bool, String> {
        let _serialised = windowed_lock()?;
        let previous = read_dx_settings()?;
        let current = match &previous {
            Previous::Absent => String::new(),
            Previous::Was(text) => text.clone(),
        };
        if super::dx_setting_is_on(&current, SWAP_EFFECT_UPGRADE) {
            return Ok(false);
        }
        // No record, no change. This setting is advertised as reversible, and
        // the record is the only thing that makes it so — writing the registry
        // first and discovering afterwards that the original state was never
        // saved leaves the user with a change they cannot undo.
        let Some(path) = undo_record_path() else {
            return Err(
                "the app data folder could not be located, so the original value could not be \
                 saved for undo — leaving this setting alone"
                    .to_string(),
            );
        };
        if !path.exists() {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            write_no_follow(&path, &previous.encode())
                .map_err(|e| format!("could not save the undo record: {e}"))?;
        }
        let merged = super::merge_dx_setting(&current, SWAP_EFFECT_UPGRADE, "1");
        write_dx_settings(&Previous::Was(merged))?;
        Ok(true)
    }

    /// Put our setting back as it was, leaving every other entry in the shared
    /// value exactly as it stands today.
    pub fn restore_windowed_optimizations() -> Result<bool, String> {
        let _serialised = windowed_lock()?;
        let Some(path) = undo_record_path() else {
            return Err("no place to look for an undo record".to_string());
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            // Only a missing record means "we never changed anything". A
            // sharing violation or an I/O error is not that, and reporting it
            // as "nothing to undo" would leave the setting on with the user
            // told it was never touched.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(e) => {
                return Err(format!(
                    "the undo record at {} could not be read ({e}) — leaving the setting alone",
                    path.display()
                ))
            }
        };
        let Some(previous) = Previous::decode(&text) else {
            return Err(format!(
                "the undo record at {} is unreadable — leaving the setting alone",
                path.display()
            ));
        };
        // Undo against the value as it is *now*, not against the snapshot: see
        // [`super::undo_windowed_optimization`].
        let current = match read_dx_settings()? {
            Previous::Absent => String::new(),
            Previous::Was(text) => text,
        };
        let merged = super::undo_windowed_optimization(&current, &previous);
        // An empty result means nothing is left in the value at all; that is the
        // "absent" state, not an empty string.
        let restored = if merged.trim().is_empty() {
            Previous::Absent
        } else {
            Previous::Was(merged)
        };
        write_dx_settings(&restored)?;
        let _ = std::fs::remove_file(&path);
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merging_preserves_other_entries() {
        let existing = "VRROptimizeEnable=1;SomethingElse=0;";
        let merged = merge_dx_setting(existing, SWAP_EFFECT_UPGRADE, "1");
        assert!(merged.contains("VRROptimizeEnable=1;"));
        assert!(merged.contains("SomethingElse=0;"));
        assert!(merged.contains("SwapEffectUpgradeEnable=1;"));
    }

    #[test]
    fn merging_replaces_our_key_without_duplicating_it() {
        let existing = "SwapEffectUpgradeEnable=0;VRROptimizeEnable=1;";
        let merged = merge_dx_setting(existing, SWAP_EFFECT_UPGRADE, "1");
        assert_eq!(merged.matches("SwapEffectUpgradeEnable").count(), 1);
        assert!(merged.contains("SwapEffectUpgradeEnable=1;"));
        assert!(merged.contains("VRROptimizeEnable=1;"));
    }

    #[test]
    fn merging_into_an_empty_value_just_adds_the_key() {
        assert_eq!(
            merge_dx_setting("", SWAP_EFFECT_UPGRADE, "1"),
            "SwapEffectUpgradeEnable=1;"
        );
    }

    #[test]
    fn already_on_is_detected() {
        assert!(dx_setting_is_on(
            "SwapEffectUpgradeEnable=1;",
            SWAP_EFFECT_UPGRADE
        ));
        assert!(!dx_setting_is_on(
            "SwapEffectUpgradeEnable=0;",
            SWAP_EFFECT_UPGRADE
        ));
        assert!(!dx_setting_is_on(
            "VRROptimizeEnable=1;",
            SWAP_EFFECT_UPGRADE
        ));
    }

    #[test]
    fn absent_and_empty_are_different_states() {
        // The whole point of Previous: restoring "absent" must not write "".
        assert_ne!(Previous::Absent, Previous::Was(String::new()));
        // …and that distinction has to survive the round trip to disk.
        assert_ne!(
            Previous::Absent.encode(),
            Previous::Was(String::new()).encode()
        );
        assert_eq!(
            Previous::decode(&Previous::Absent.encode()),
            Some(Previous::Absent)
        );
        assert_eq!(
            Previous::decode(&Previous::Was(String::new()).encode()),
            Some(Previous::Was(String::new()))
        );
    }

    #[test]
    fn undo_record_round_trips_a_real_value() {
        let original = Previous::Was("VRROptimizeEnable=1;SwapEffectUpgradeEnable=0;".to_string());
        let encoded = original.encode();
        assert_eq!(Previous::decode(&encoded), Some(original));
        // Trailing newlines from a text file must not corrupt the value.
        assert_eq!(
            Previous::decode(&format!("{encoded}\r\n")),
            Previous::decode(&encoded)
        );
    }

    #[test]
    fn a_corrupt_undo_record_is_rejected_not_guessed() {
        assert_eq!(Previous::decode("garbage"), None);
        assert_eq!(Previous::decode(""), None);
    }

    /// The value is shared with Windows and other apps. Undo must put back only
    /// our key and leave everything that changed since the snapshot alone —
    /// writing the whole recorded snapshot back would delete it.
    #[test]
    fn undo_touches_only_our_key() {
        // Recorded before we touched anything: our key absent, one other entry.
        let previous = Previous::Was("VRROptimizeEnable=1;".to_string());
        // Since then: we set our key, and something else added a new entry and
        // changed the existing one.
        let current = "VRROptimizeEnable=0;SwapEffectUpgradeEnable=1;AutoHDREnable=1;";

        let undone = undo_windowed_optimization(current, &previous);

        assert!(
            !undone.contains("SwapEffectUpgradeEnable"),
            "our key was absent before, so undo must remove it: {undone}"
        );
        assert!(
            undone.contains("VRROptimizeEnable=0;"),
            "a later change to another entry must survive: {undone}"
        );
        assert!(
            undone.contains("AutoHDREnable=1;"),
            "an entry added after the snapshot must survive: {undone}"
        );
    }

    /// When the key did exist beforehand, undo restores that exact value rather
    /// than deleting it.
    #[test]
    fn undo_restores_a_preexisting_value() {
        let previous = Previous::Was("SwapEffectUpgradeEnable=0;VRROptimizeEnable=1;".to_string());
        let current = "SwapEffectUpgradeEnable=1;VRROptimizeEnable=1;AutoHDREnable=1;";

        let undone = undo_windowed_optimization(current, &previous);

        assert!(undone.contains("SwapEffectUpgradeEnable=0;"), "{undone}");
        assert!(undone.contains("AutoHDREnable=1;"), "{undone}");
        assert_eq!(undone.matches("SwapEffectUpgradeEnable").count(), 1);
    }

    /// A record of "the whole value was absent" still means only our key is
    /// removed — anything written since belongs to someone else.
    #[test]
    fn undo_from_an_absent_record_removes_only_our_key() {
        let undone = undo_windowed_optimization(
            "SwapEffectUpgradeEnable=1;AutoHDREnable=1;",
            &Previous::Absent,
        );
        assert_eq!(undone, "AutoHDREnable=1;");
    }
}

#[cfg(all(test, windows))]
mod live_tests {
    use super::*;

    /// Round-trips the real HKCU value on this machine. Restores whatever was
    /// there — including restoring "absent" by deleting, which is the case that
    /// a naive implementation gets wrong.
    /// `cargo test -p bdo-optimizer live_registry -- --ignored --nocapture`
    #[test]
    #[ignore = "writes the real HKCU DirectX value (and puts it back)"]
    fn live_registry_round_trip() {
        let before = read_dx_settings().expect("read");
        println!("before: {before:?}");

        let merged = merge_dx_setting(
            match &before {
                Previous::Absent => "",
                Previous::Was(t) => t,
            },
            SWAP_EFFECT_UPGRADE,
            "1",
        );
        write_dx_settings(&Previous::Was(merged)).expect("write");

        let after = read_dx_settings().expect("read back");
        println!("after:  {after:?}");
        match &after {
            Previous::Was(t) => assert!(dx_setting_is_on(t, SWAP_EFFECT_UPGRADE)),
            Previous::Absent => panic!("value vanished after writing it"),
        }

        write_dx_settings(&before).expect("restore");
        let restored = read_dx_settings().expect("read restored");
        println!("restored: {restored:?}");
        assert_eq!(
            restored, before,
            "the original state must come back exactly"
        );
    }
}
