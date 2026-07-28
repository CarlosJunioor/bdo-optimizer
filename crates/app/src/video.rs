//! NVIDIA driver-profile tweaks from the performance guide, applied through the
//! bundled NVIDIA Profile Inspector CLI (`-mergeImport -silentImport`).
//!
//! Scope and safety:
//!
//! * Only the game's predefined **"Black Desert"** driver profile is written.
//!   Merge-import never resets the profile and never touches settings we do not
//!   list (a user's own G-Sync entries survive), and nothing global or
//!   driver-wide is changed.
//! * The inspector binary is hash-pinned exactly like PresentMon and runs fully
//!   outside the game process — the anti-cheat story is unchanged.
//! * A silent import reports nothing (no exit code, no output), so every apply
//!   is verified afterwards with `-exportCustomized`: the exported .nip must
//!   contain the expected id/value pairs inside the Black Desert profile.
//!
//! Setting ids and values were verified against Profile Inspector v3.0.2.1's
//! `CustomSettingNames.xml` and cross-checked against a real machine configured
//! per the guide.

use std::path::{Path, PathBuf};

/// The bundled NVIDIA Profile Inspector executable file name.
pub const INSPECTOR_EXE: &str = "nvidiaProfileInspector.exe";

/// Exact bundled Profile Inspector v3.0.2.1 binary identity.
const INSPECTOR_SIZE: u64 = 1_043_456;
const INSPECTOR_SHA256: [u8; 32] = [
    0x1e, 0xbd, 0x81, 0x29, 0xb3, 0xc5, 0x64, 0xbf, 0x22, 0x62, 0x91, 0xfb, 0x33, 0x44, 0x81, 0x9f,
    0xd5, 0x96, 0x68, 0x06, 0x6f, 0x0c, 0x5e, 0x03, 0x33, 0x4a, 0x69, 0xa0, 0x4a, 0x62, 0x85, 0x9e,
];

/// NVIDIA's predefined driver profile for BDO (contains `blackdesert64.exe`).
pub const PROFILE_NAME: &str = "Black Desert";

// DRS setting ids (decimal, as .nip files store them).
const THREADED_OPTIMIZATION: u32 = 549_528_094; // 0x20C1221E: 0 auto, 1 on, 2 off
const ANSEL_ENABLE: u32 = 276_158_834; // 0x1075D972: 0 off, 1 on
const POWER_MANAGEMENT_MODE: u32 = 274_197_361; // 0x1057EB71: 0 adaptive, 5 optimal
const ULL_ENABLED: u32 = 277_041_152; // 0x10835000: 0 off, 1 on
const ULL_CPL_STATE: u32 = 390_467; // 0x0005F543: control-panel state, 2 = ultra
const MAX_PRERENDERED_FRAMES: u32 = 8_102_046; // 0x007BA09E: 0 app-controlled

/// The guide's profile values as `(setting id, value)` pairs.
///
/// Threaded optimization follows the guide's core-count rule: On for 6+ physical
/// cores, Off for older quad-cores. Ultra Low Latency is opt-in ("enable if your
/// CPU can handle the overhead") and expands to the trio NVCP itself writes.
pub fn guide_settings(physical_cores: usize, ull: bool) -> Vec<(u32, u32)> {
    let threaded = if physical_cores >= 6 { 1 } else { 2 };
    let mut settings = vec![
        (THREADED_OPTIMIZATION, threaded),
        (ANSEL_ENABLE, 0),
        (POWER_MANAGEMENT_MODE, 0),
    ];
    if ull {
        settings.extend([
            (ULL_ENABLED, 1),
            (ULL_CPL_STATE, 2),
            (MAX_PRERENDERED_FRAMES, 1),
        ]);
    }
    settings
}

/// Driver-default values for every setting [`guide_settings`] can touch.
///
/// True "remove the override" is not reachable through a .nip import, so restore
/// writes the documented driver defaults explicitly — behaviorally identical.
pub fn default_settings() -> Vec<(u32, u32)> {
    vec![
        (THREADED_OPTIMIZATION, 0),
        (ANSEL_ENABLE, 1),
        (POWER_MANAGEMENT_MODE, 5),
        (ULL_ENABLED, 0),
        (ULL_CPL_STATE, 0),
        (MAX_PRERENDERED_FRAMES, 0),
    ]
}

/// Render the `.nip` XML for the Black Desert profile with the given settings.
///
/// Mirrors the element order Profile Inspector's own exporter produces, since
/// its importer deserializes with the same fixed-order XML serializer.
pub fn nip_xml(settings: &[(u32, u32)]) -> String {
    let mut xml = format!(
        "<?xml version=\"1.0\" encoding=\"utf-16\"?>\r\n\
         <ArrayOfProfile>\r\n\
         \x20 <Profile>\r\n\
         \x20   <ProfileName>{PROFILE_NAME}</ProfileName>\r\n\
         \x20   <Executeables>\r\n\
         \x20     <string>blackdesert64.exe</string>\r\n\
         \x20   </Executeables>\r\n\
         \x20   <Settings>\r\n"
    );
    for (id, value) in settings {
        xml.push_str(&format!(
            "      <ProfileSetting>\r\n\
             \x20       <SettingNameInfo />\r\n\
             \x20       <SettingID>{id}</SettingID>\r\n\
             \x20       <SettingValue>{value}</SettingValue>\r\n\
             \x20       <ValueType>Dword</ValueType>\r\n\
             \x20     </ProfileSetting>\r\n"
        ));
    }
    xml.push_str("    </Settings>\r\n  </Profile>\r\n</ArrayOfProfile>\r\n");
    xml
}

/// Encode a string as UTF-16 LE with BOM — the encoding Profile Inspector's
/// .NET XML serializer reads and writes.
pub fn utf16le_bytes(text: &str) -> Vec<u8> {
    let mut out = vec![0xFF, 0xFE];
    for unit in text.encode_utf16() {
        out.extend_from_slice(&unit.to_le_bytes());
    }
    out
}

/// Decode a Profile Inspector export by sniffing the BOM.
///
/// The exporter's XML declaration always says `utf-16`, but the bytes it
/// actually writes are UTF-8 (a .NET writer quirk) — so trust the BOM/content,
/// not the declaration.
pub fn decode_export(bytes: &[u8]) -> String {
    match bytes {
        [0xFF, 0xFE, rest @ ..] => {
            let units: Vec<u16> = rest
                .chunks_exact(2)
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                .collect();
            String::from_utf16_lossy(&units)
        }
        [0xEF, 0xBB, 0xBF, rest @ ..] => String::from_utf8_lossy(rest).into_owned(),
        _ => String::from_utf8_lossy(bytes).into_owned(),
    }
}

/// True when the exported profiles XML shows the Black Desert profile carrying
/// every expected `(id, value)` pair. Whitespace-insensitive so the exporter's
/// exact indentation cannot break verification.
pub fn export_confirms(exported_xml: &str, settings: &[(u32, u32)]) -> bool {
    fn strip(s: &str) -> String {
        s.chars().filter(|c| !c.is_whitespace()).collect()
    }
    let doc = strip(exported_xml);
    let name_tag = strip(&format!("<ProfileName>{PROFILE_NAME}</ProfileName>"));
    let Some(start) = doc.find(&name_tag) else {
        return false;
    };
    let block = &doc[start..];
    let block = &block[..block.find("</Profile>").unwrap_or(block.len())];
    settings.iter().all(|(id, value)| {
        block.contains(&format!(
            "<SettingID>{id}</SettingID><SettingValue>{value}</SettingValue>"
        ))
    })
}

/// Build the ordered candidate paths for the bundled inspector executable.
/// Same layout contract as PresentMon: exe-dir copy first, then the workspace
/// vendor copy in debug builds. Pure, for unit tests.
pub fn candidate_paths(exe_dir: Option<&Path>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(dir) = exe_dir {
        out.push(dir.join(INSPECTOR_EXE));
    }
    #[cfg(debug_assertions)]
    out.push(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../vendor/nvidiaProfileInspector")
            .join(INSPECTOR_EXE),
    );
    out
}

/// Open the inspector for hashing in a way that blocks tampering while the
/// handle lives.
///
/// On Windows the share mode is `FILE_SHARE_READ` only: other processes may
/// read the file but cannot write to, rename, or delete it. Holding this handle
/// across both the hash check *and* the spawn is what makes the pin meaningful
/// — otherwise a same-user process could swap the executable in between and get
/// its replacement launched with the administrator token we run under.
fn open_locked(path: &Path) -> std::io::Result<std::fs::File> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 0x0000_0001;
        std::fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .open(path)
    }
    #[cfg(not(windows))]
    std::fs::File::open(path)
}

/// Hash an already-open handle against the pinned build identity.
fn handle_is_trusted(file: &mut std::fs::File) -> bool {
    use sha2::Digest;
    let Ok(metadata) = file.metadata() else {
        return false;
    };
    if !metadata.is_file() || metadata.len() != INSPECTOR_SIZE {
        return false;
    }
    let mut hash = sha2::Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        match std::io::Read::read(file, &mut buffer) {
            Ok(0) => break,
            Ok(read) => hash.update(&buffer[..read]),
            Err(_) => return false,
        }
    }
    <[u8; 32]>::from(hash.finalize()) == INSPECTOR_SHA256
}

/// Verify that `path` is the exact bundled Profile Inspector build.
///
/// Suitable for *discovery* only. Anything that goes on to execute the file
/// must instead hold the handle from [`open_locked`] across the spawn; see
/// [`worker::run`].
pub fn is_trusted_inspector(path: &Path) -> bool {
    match open_locked(path) {
        Ok(mut file) => handle_is_trusted(&mut file),
        Err(_) => false,
    }
}

/// Resolve the bundled inspector executable, or `None` if it cannot be found.
pub fn resolve() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok();
    let exe_dir = exe.as_deref().and_then(Path::parent);
    candidate_paths(exe_dir)
        .iter()
        .find(|p| is_trusted_inspector(p))
        .and_then(|path| path.canonicalize().ok())
}

/// The human-readable locations shown when resolution fails.
pub fn expected_locations() -> [String; 2] {
    [
        format!("next to the app executable ({INSPECTOR_EXE})"),
        format!("vendor/nvidiaProfileInspector/{INSPECTOR_EXE} under the project root"),
    ]
}

/// Windows-only spawn/verify pipeline. Non-Windows builds keep only the pure
/// helpers above (unit-testable everywhere).
#[cfg(windows)]
pub mod worker {
    use std::collections::HashSet;
    use std::path::{Path, PathBuf};
    use std::sync::mpsc::{channel, Receiver};
    use std::sync::{Mutex, MutexGuard, OnceLock};
    use std::time::{Duration, Instant};

    /// How long one inspector invocation may take before it is killed.
    const INSPECTOR_TIMEOUT: Duration = Duration::from_secs(90);

    /// Serializes driver jobs so two of our own jobs cannot mistake each other's
    /// `CustomProfiles_*.nip` export for their own.
    ///
    /// ponytail: in-process lock only. A second *copy* of the app (or a manual
    /// Profile Inspector export) still races; that is why the export step below
    /// also picks the newest created file and deletes only the file it claimed.
    /// A named Windows mutex would close the cross-process case if it ever bites.
    fn job_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        let mutex = LOCK.get_or_init(|| Mutex::new(()));
        mutex
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// A one-shot background driver-profile job. Poll `rx` from the UI loop.
    pub struct DriverWorker {
        pub rx: Receiver<Result<String, String>>,
        /// "apply" or "restore" — for the in-progress label.
        pub label: &'static str,
    }

    /// Start applying (or restoring) `settings` via the inspector at `exe`.
    pub fn start(exe: PathBuf, settings: Vec<(u32, u32)>, label: &'static str) -> DriverWorker {
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let _ = tx.send(run(&exe, &settings));
        });
        DriverWorker { rx, label }
    }

    fn run(exe: &Path, settings: &[(u32, u32)]) -> Result<String, String> {
        let _serialized = job_lock();

        // Hold this handle for the whole job. It denies other processes write /
        // rename / delete access to the executable, so the binary we hashed is
        // provably the binary every spawn below runs — no swap window.
        let mut locked = super::open_locked(exe)
            .map_err(|e| format!("could not open {}: {e}", exe.display()))?;
        if !super::handle_is_trusted(&mut locked) {
            return Err(format!(
                "{} does not match the bundled Profile Inspector build",
                exe.display()
            ));
        }
        let exe_dir = exe
            .parent()
            .ok_or_else(|| "inspector path has no parent directory".to_string())?
            .to_path_buf();

        // 1. Write the .nip into a private directory and merge-import it.
        //    `create_dir` fails if the name already exists, so winning that call
        //    proves exclusive ownership: no other process can have pre-created
        //    the path and no shared, predictable file is handed to the elevated
        //    child for it to re-open.
        let job_dir = create_private_dir()?;
        let nip = job_dir.join("profile.nip");
        let import = write_payload(&nip, settings).and_then(|payload| {
            // Hold the payload open with FILE_SHARE_READ for the whole import.
            // Writing then closing it would leave a window in which another
            // process could swap the file the elevated child re-opens by path;
            // this handle lets the child read it and nobody rewrite it.
            let nip_arg = nip.to_string_lossy().into_owned();
            let result = run_inspector(exe, &["-mergeImport", "-silentImport", &nip_arg]);
            drop(payload);
            result
        });
        let _ = std::fs::remove_dir_all(&job_dir);
        import?;

        // 2. The silent import reports nothing, so verify through an export.
        let before: HashSet<PathBuf> = export_files(&exe_dir).into_iter().collect();
        run_inspector(exe, &["-exportCustomized"])?;
        let created: Vec<PathBuf> = export_files(&exe_dir)
            .into_iter()
            .filter(|p| !before.contains(p))
            .collect();
        // The export lands next to the executable under a timestamped name we
        // do not choose, so the only sound attribution is "exactly one new file
        // appeared". Picking the newest of several would happily verify — and
        // delete — an export another process made; refusing is the honest
        // answer, and in practice there is always exactly one.
        let export = match created.as_slice() {
            [only] => only,
            [] => return Err("verification export produced no file".to_string()),
            many => {
                return Err(format!(
                    "{} new Profile Inspector exports appeared at once, so this run's own \
                     export could not be identified. Close any other Profile Inspector \
                     window and try again.",
                    many.len()
                ))
            }
        };
        let bytes = std::fs::read(export)
            .map_err(|e| format!("could not read {}: {e}", export.display()))?;
        let _ = std::fs::remove_file(export);

        if super::export_confirms(&super::decode_export(&bytes), settings) {
            Ok(format!(
                "Verified: the driver's \"{}\" profile now carries all {} settings.",
                super::PROFILE_NAME,
                settings.len()
            ))
        } else {
            // Keep the evidence: without it a verification failure cannot be
            // debugged, because the export is deleted above.
            let kept = std::env::temp_dir().join("bdo-optimizer-verify-failed.nip");
            let _ = std::fs::write(&kept, &bytes);
            Err(format!(
                "Import ran but the driver database does not show the expected \
                 values on the \"{}\" profile. Open NVIDIA Profile Inspector \
                 manually to check for a renamed profile. (Export kept at {})",
                super::PROFILE_NAME,
                kept.display()
            ))
        }
    }

    /// Write the import payload and return a handle that keeps it unmodifiable
    /// (read-shared only) for as long as it is held.
    fn write_payload(nip: &Path, settings: &[(u32, u32)]) -> Result<std::fs::File, String> {
        std::fs::write(nip, super::utf16le_bytes(&super::nip_xml(settings)))
            .map_err(|e| format!("could not write {}: {e}", nip.display()))?;
        super::open_locked(nip).map_err(|e| format!("could not lock {}: {e}", nip.display()))
    }

    /// Create a fresh, exclusively-owned directory under the temp dir.
    ///
    /// `create_dir` is atomic and fails when the path exists, so the first name
    /// that succeeds is one nobody else holds.
    fn create_private_dir() -> Result<PathBuf, String> {
        let base = std::env::temp_dir();
        let pid = std::process::id();
        for attempt in 0..64u32 {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(attempt);
            let dir = base.join(format!("bdo-optimizer-nvidia-{pid}-{nanos}-{attempt}"));
            match std::fs::create_dir(&dir) {
                Ok(()) => return Ok(dir),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(format!("could not create {}: {e}", dir.display())),
            }
        }
        Err("could not create a private temp directory".to_string())
    }

    /// Every `CustomProfiles_*.nip` currently next to the inspector executable.
    fn export_files(dir: &Path) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("CustomProfiles_") && n.ends_with(".nip"))
            })
            .collect()
    }

    /// Spawn the inspector without a console window and wait for it to exit.
    fn run_inspector(exe: &Path, args: &[&str]) -> Result<(), String> {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;

        let mut child = std::process::Command::new(exe)
            .args(args)
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map_err(|e| format!("could not start {}: {e}", exe.display()))?;

        let started = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(status)) if status.success() => return Ok(()),
                // A non-zero exit means the import/export did not do what we
                // asked. Reporting it as success would let a failed run wear a
                // green check whenever the profile already matched.
                Ok(Some(status)) => {
                    return Err(format!(
                        "Profile Inspector exited with {status} (it may have been closed or \
                         refused by the driver)"
                    ))
                }
                Ok(None) if started.elapsed() > INSPECTOR_TIMEOUT => {
                    // Kill *and reap*: without the wait the child stays a zombie
                    // handle and can still be mid-write while we report failure.
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!(
                        "Profile Inspector did not finish within {}s",
                        INSPECTOR_TIMEOUT.as_secs()
                    ));
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(100)),
                Err(e) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("failed waiting for Profile Inspector: {e}"));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guide_settings_follow_core_count_rule() {
        let six = guide_settings(6, false);
        assert!(six.contains(&(THREADED_OPTIMIZATION, 1)));
        let four = guide_settings(4, false);
        assert!(four.contains(&(THREADED_OPTIMIZATION, 2)));
        assert!(six.contains(&(ANSEL_ENABLE, 0)));
        assert!(six.contains(&(POWER_MANAGEMENT_MODE, 0)));
        assert_eq!(six.len(), 3);
    }

    #[test]
    fn ull_expands_to_the_nvcp_trio() {
        let with_ull = guide_settings(8, true);
        assert!(with_ull.contains(&(ULL_ENABLED, 1)));
        assert!(with_ull.contains(&(ULL_CPL_STATE, 2)));
        assert!(with_ull.contains(&(MAX_PRERENDERED_FRAMES, 1)));
        assert_eq!(with_ull.len(), 6);
    }

    #[test]
    fn restore_covers_every_touchable_setting() {
        let touchable: Vec<u32> = guide_settings(8, true).iter().map(|s| s.0).collect();
        let defaults = default_settings();
        for id in touchable {
            assert!(
                defaults.iter().any(|d| d.0 == id),
                "missing default for {id}"
            );
        }
    }

    #[test]
    fn nip_xml_lists_profile_exe_and_settings() {
        let settings = guide_settings(8, true);
        let xml = nip_xml(&settings);
        assert!(xml.contains("<ProfileName>Black Desert</ProfileName>"));
        assert!(xml.contains("<string>blackdesert64.exe</string>"));
        for (id, value) in &settings {
            assert!(xml.contains(&format!("<SettingID>{id}</SettingID>")));
            assert!(xml.contains(&format!("<SettingValue>{value}</SettingValue>")));
        }
    }

    #[test]
    fn utf16_round_trip_keeps_content_and_bom() {
        let xml = nip_xml(&guide_settings(8, false));
        let bytes = utf16le_bytes(&xml);
        assert_eq!(&bytes[..2], &[0xFF, 0xFE]);
        assert_eq!(decode_export(&bytes), xml);
    }

    #[test]
    fn decode_export_handles_utf8_despite_utf16_declaration() {
        // The real exporter writes UTF-8 bytes under a `utf-16` declaration.
        let xml = nip_xml(&guide_settings(8, false));
        assert_eq!(decode_export(xml.as_bytes()), xml);
        // And BOM-prefixed UTF-8 too.
        let mut bom = vec![0xEF, 0xBB, 0xBF];
        bom.extend_from_slice(xml.as_bytes());
        assert_eq!(decode_export(&bom), xml);
    }

    #[test]
    fn export_confirms_accepts_matching_and_rejects_wrong_values() {
        let settings = guide_settings(8, true);
        // Our own .nip has the same XML shape an export does.
        let export = nip_xml(&settings);
        assert!(export_confirms(&export, &settings));

        // A single changed value must fail verification.
        let mut wrong = settings.clone();
        wrong[0].1 = 99;
        assert!(!export_confirms(&export, &wrong));

        // A different profile entirely must fail too.
        assert!(!export_confirms(
            "<ArrayOfProfile></ArrayOfProfile>",
            &settings
        ));
    }

    #[test]
    fn exe_dir_copy_is_first_candidate() {
        let cands = candidate_paths(Some(Path::new("/opt/bdo")));
        assert_eq!(cands[0], PathBuf::from("/opt/bdo").join(INSPECTOR_EXE));
    }

    /// Full pipeline against the real driver database: restore defaults, verify,
    /// then re-apply the guide profile (with ULL) and verify again. Both legs
    /// really change driver values, so this proves imports write — and it ends
    /// on the guide configuration. Requires an NVIDIA GPU and elevation:
    /// `cargo test -p bdo-optimizer e2e_roundtrip -- --ignored --nocapture`
    #[cfg(windows)]
    #[test]
    #[ignore = "writes the real NVIDIA driver profile"]
    fn e2e_roundtrip_restore_then_apply() {
        let exe = resolve().expect("bundled inspector not found");
        for (settings, label) in [
            (default_settings(), "restore"),
            (guide_settings(8, true), "apply"),
        ] {
            let job = worker::start(exe.clone(), settings, label);
            let result = job
                .rx
                .recv_timeout(std::time::Duration::from_secs(240))
                .expect("worker did not finish");
            assert!(result.is_ok(), "{label}: {result:?}");
        }
    }

    #[cfg(debug_assertions)]
    #[test]
    fn bundled_inspector_hash_matches_pin() {
        let bundled = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../vendor/nvidiaProfileInspector")
            .join(INSPECTOR_EXE);
        assert!(is_trusted_inspector(&bundled));
    }
}
