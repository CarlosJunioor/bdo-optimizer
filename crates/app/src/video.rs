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

/// The `.config` the inspector ships with, reproduced verbatim.
///
/// Emitted from here rather than copied out of the app folder when the tool is
/// staged into its private run directory: it is a fixed five lines that only
/// pin the .NET runtime version, and reading it from a user-writable location
/// would hand a same-user process a way to add assembly-binding redirects to a
/// process that runs elevated.
pub const INSPECTOR_CONFIG: &str = concat!(
    "<?xml version=\"1.0\" encoding=\"utf-8\"?>\r\n",
    "<configuration>\r\n",
    "  <startup>\r\n",
    "    <supportedRuntime version=\"v4.0\" sku=\".NETFramework,Version=v4.8\" />\r\n",
    "  </startup>\r\n",
    "</configuration>",
);

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

/// The defaults for exactly the settings a previous apply actually wrote.
///
/// Restoring every setting in [`default_settings`] undoes work this app never
/// did: with Ultra Low Latency left unticked — the default — apply never touches
/// the three ULL entries, so a blanket restore wipes a Low Latency setup the
/// user configured themselves in the NVIDIA control panel.
///
/// `applied` is the id list recorded when the profile was written. An empty
/// list is **not** "assume the usual three": it means this app has no evidence
/// it ever changed the profile — a fresh install, or a deleted record — and the
/// only honest scope for a restore then is nothing at all. Guessing there would
/// let Restore overwrite a Black Desert profile the user built themselves,
/// which is exactly what the button promises not to do. Callers surface the
/// empty result rather than running an import that would change nothing.
pub fn restore_settings(applied: &[u32]) -> Vec<(u32, u32)> {
    default_settings()
        .into_iter()
        .filter(|(id, _)| applied.contains(id))
        .collect()
}

/// A machine-wide lock over the driver profile and its applied-settings record.
///
/// The record is a read-modify-write on a shared file, and the in-process job
/// lock says nothing about a second copy of the app. Two elevated instances
/// could each read the same prior ids and overwrite the other's union — after
/// which the profile carries settings the surviving record does not list, and
/// Undo cannot reverse them. Held across reading, importing and rewriting.
#[cfg(windows)]
pub struct DriverLock(windows::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl DriverLock {
    /// Take the lock, waiting briefly for another instance to finish.
    pub fn acquire() -> Result<Self, String> {
        use windows::Win32::Foundation::WAIT_TIMEOUT;
        use windows::Win32::System::Threading::WaitForSingleObject;

        // The driver profile is machine state, so the lock crosses logon
        // sessions too — see `privdir::cross_session_mutex`.
        let handle = crate::privdir::cross_session_mutex("bdo-optimizer-driver-profile")
            .map_err(|e| format!("could not create the driver lock: {e}"))?;
        // SAFETY: a live mutex handle from the call above.
        if unsafe { WaitForSingleObject(handle, 30_000) } == WAIT_TIMEOUT {
            // SAFETY: closing a handle we own and do not use again.
            unsafe {
                let _ = windows::Win32::Foundation::CloseHandle(handle);
            }
            return Err(
                "another copy of BDO Optimizer is changing the driver profile — wait for it \
                 to finish"
                    .to_string(),
            );
        }
        Ok(Self(handle))
    }
}

#[cfg(windows)]
impl Drop for DriverLock {
    fn drop(&mut self) {
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::Threading::ReleaseMutex;
        // SAFETY: owned by this guard and released exactly once.
        unsafe {
            let _ = ReleaseMutex(self.0);
            let _ = CloseHandle(self.0);
        }
    }
}

/// Where the list of applied setting ids is kept, beside the other undo record.
fn applied_record_path() -> Option<PathBuf> {
    bdo_bench::SessionStore::default_store()
        .ok()
        .map(|s| s.dir().join("nvidia-applied-settings.txt"))
}

/// Record which setting ids an apply wrote, so undo can reverse exactly those.
///
/// The record accumulates: it is the union of every id written since the last
/// verified restore, not just the latest apply. Applying once with Ultra Low
/// Latency on and again with it off would otherwise shrink the record to three
/// ids — and because the import is a *merge*, the earlier ULL overrides are
/// still sitting in the driver profile, so undo would walk away leaving
/// settings this app wrote behind.
///
/// Returns an error the caller can report: silently failing to record makes a
/// change the UI calls reversible into one that is not.
pub fn record_applied(settings: &[(u32, u32)]) -> Result<(), String> {
    let path = applied_record_path()
        .ok_or_else(|| "the app data folder could not be located".to_string())?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
    }
    let mut ids: Vec<u32> = recorded_applied();
    for (id, _) in settings {
        if !ids.contains(id) {
            ids.push(*id);
        }
    }
    let text: Vec<String> = ids.iter().map(u32::to_string).collect();
    write_no_follow(&path, &text.join("\n"))
        .map_err(|e| format!("could not save {}: {e}", path.display()))
}

/// Read back the ids recorded by [`record_applied`]; empty when none survived.
pub fn recorded_applied() -> Vec<u32> {
    let Some(path) = applied_record_path() else {
        return Vec::new();
    };
    std::fs::read_to_string(path)
        .map(|text| text.lines().filter_map(|l| l.trim().parse().ok()).collect())
        .unwrap_or_default()
}

/// Write a file without following a reparse point planted at its path.
///
/// These records sit at predictable names under the per-user data directory,
/// and the NVIDIA step runs elevated. `std::fs::write` follows a link, so one
/// planted here would be followed with the administrator token and truncate
/// whatever it points at.
pub fn write_no_follow(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    #[cfg(windows)]
    use std::os::windows::fs::OpenOptionsExt;

    // Write a sibling and replace, rather than truncating in place. This file
    // is the *only* record of which driver settings this app changed: a
    // truncate that then fails to finish — disk full, a killed process, power
    // loss — would leave the overrides applied with an empty or half-written
    // list, and Undo could no longer reverse them. The replace is the last
    // step, so the old contents survive every failure before it.
    let tmp = path.with_extension("tmp");
    {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(windows)]
        {
            const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
            options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        }
        let mut file = options.open(&tmp)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
    }
    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// Forget the applied-settings record once the profile has been restored.
pub fn clear_applied_record() {
    if let Some(path) = applied_record_path() {
        let _ = std::fs::remove_file(path);
    }
}

/// Put the record back to `previous`, used when an apply is recorded and then
/// never reaches the driver.
///
/// The record has to be written *before* the job runs — a crash mid-import must
/// not leave settings changed with no note of which ones. But a job that fails
/// before the import (staging, spawning, an inspector that never starts) leaves
/// ids attributed to this app that it never wrote, and a later Restore would
/// then reset the user's own values for them. Rewinding on a failure that is
/// known to precede the import keeps the record honest in both directions.
pub fn restore_applied_record(previous: &[u32]) {
    if previous.is_empty() {
        clear_applied_record();
        return;
    }
    let Some(path) = applied_record_path() else {
        return;
    };
    let text: Vec<String> = previous.iter().map(u32::to_string).collect();
    let _ = write_no_follow(&path, &text.join("\n"));
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
        pub rx: Receiver<DriverOutcome>,
        /// "apply" or "restore" — for the in-progress label.
        pub label: &'static str,
        /// Set to ask an in-flight job to stop at its next check, and to kill
        /// the inspector if one is already running. See [`DriverWorker::stop`].
        cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    /// What a finished driver job reports.
    pub struct DriverOutcome {
        pub result: Result<String, String>,
        /// Whether the merge-import actually ran.
        ///
        /// A failure is *not* proof the profile is untouched: the import
        /// completes before the verification export, so an export timeout or a
        /// mismatch fails a job whose settings did land. Callers use this to
        /// decide whether the applied-settings record may be rewound — doing
        /// that after a real write would leave changed values with no undo.
        pub imported: bool,
    }

    impl DriverWorker {
        /// Stop an in-flight job and wait briefly for it to unwind.
        ///
        /// Called when the app is closing. The job can be parked for up to
        /// `INSPECTOR_TIMEOUT` waiting on a child process; letting the process
        /// exit underneath it would leave Profile Inspector running elevated,
        /// still writing the driver profile, with nothing left to verify the
        /// result or clean the staging directory.
        pub fn stop(&mut self) {
            self.cancel.store(true, std::sync::atomic::Ordering::SeqCst);
            let Some(thread) = self.thread.take() else {
                return;
            };
            let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let flag = done.clone();
            std::thread::spawn(move || {
                let _ = thread.join();
                flag.store(true, std::sync::atomic::Ordering::SeqCst);
            });
            let deadline = Instant::now() + Duration::from_secs(10);
            while !done.load(std::sync::atomic::Ordering::SeqCst) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(25));
            }
        }
    }

    /// Start applying (or restoring) `settings` via the inspector at `exe`.
    pub fn start(exe: PathBuf, settings: Vec<(u32, u32)>, label: &'static str) -> DriverWorker {
        let (tx, rx) = channel();
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = cancel.clone();
        let thread = std::thread::spawn(move || {
            let _ = tx.send(run(&exe, &settings, &flag));
        });
        DriverWorker {
            rx,
            label,
            cancel,
            thread: Some(thread),
        }
    }

    fn run(
        exe: &Path,
        settings: &[(u32, u32)],
        cancel: &std::sync::atomic::AtomicBool,
    ) -> DriverOutcome {
        let mut imported = false;
        let result = run_job(exe, settings, cancel, &mut imported);
        DriverOutcome { result, imported }
    }

    fn run_job(
        exe: &Path,
        settings: &[(u32, u32)],
        cancel: &std::sync::atomic::AtomicBool,
        imported: &mut bool,
    ) -> Result<String, String> {
        // In-process serialisation only. The *machine-wide* `DriverLock` is
        // held by the caller for the whole transaction — from before the
        // applied-settings record is read until the completion bookkeeping is
        // done — so taking it again here would block against our own UI thread
        // (a Windows mutex is owned by a thread, not a process).
        let _serialized = job_lock();
        if cancelled(cancel) {
            return Err("cancelled".to_string());
        }

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
        // Run the inspector from a directory that holds *only* files we
        // wrote. Pinning the executable proves the EXE is ours, but Windows
        // searches the executable's own folder before System32 for ordinary
        // (non-KnownDLL) imports, and this one resolves `nvapi64.dll` by name.
        // Left in the portable app folder, a same-user process could drop an
        // `nvapi64.dll` beside the pinned EXE and have it loaded into a child
        // that inherits our administrator token — no tampering with the EXE
        // required. A fresh, exclusively-created directory has nothing to find.
        //
        // Exports land here too, which is why this file no longer needs to
        // stash and restore the user's own `CustomProfiles_*.nip` files: the
        // directory starts empty and belongs to this run alone.
        let run_dir = crate::privdir::create("bdo-optimizer-nvidia-run")?;
        // `staged_pin` is held for the whole run, not just the verification:
        // dropping it would leave the copy replaceable in the moment before
        // each spawn, which is the same check-then-use gap the pin exists to
        // close. The integrity label on `run_dir` blocks a lower-integrity
        // writer, the pin blocks an equal-integrity one.
        let (exe, staged_pin) = match stage_inspector(&mut locked, &run_dir) {
            Ok(staged) => staged,
            Err(e) => {
                let _ = std::fs::remove_dir_all(&run_dir);
                return Err(e);
            }
        };
        let outcome = run_in(&exe, &run_dir, settings, cancel, imported);
        // Release the pin *before* clearing up. It is opened `FILE_SHARE_READ`
        // only, which denies delete — so leaving it open made `remove_dir_all`
        // fail silently and every apply or restore left a copy of the inspector,
        // its config and an exported profile behind in the temp folder.
        drop(staged_pin);
        let _ = std::fs::remove_dir_all(&run_dir);
        outcome
    }

    /// Copy the verified inspector out of `locked` into `dir`, alongside the
    /// `.config` the .NET launcher needs, and hand back the new path.
    ///
    /// The copy is written from the *pinned handle*, so it is byte-for-byte the
    /// build that was hashed. The `.config` is emitted from a constant rather
    /// than copied out of the app folder: it is five fixed lines that only pin
    /// the runtime version, and reading it from a user-writable location would
    /// reintroduce exactly the tampering this staging removes.
    fn stage_inspector(
        locked: &mut std::fs::File,
        dir: &Path,
    ) -> Result<(PathBuf, std::fs::File), String> {
        use std::io::{Read, Seek, SeekFrom, Write};
        use std::os::windows::fs::OpenOptionsExt;

        let dest = dir.join(super::INSPECTOR_EXE);
        locked
            .seek(SeekFrom::Start(0))
            .map_err(|e| format!("could not rewind the inspector: {e}"))?;
        let mut bytes = Vec::new();
        locked
            .read_to_end(&mut bytes)
            .map_err(|e| format!("could not read the inspector: {e}"))?;
        let mut out = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .share_mode(0)
            .open(&dest)
            .map_err(|e| format!("could not create {}: {e}", dest.display()))?;
        out.write_all(&bytes)
            .and_then(|()| out.sync_all())
            .map_err(|e| format!("could not write {}: {e}", dest.display()))?;
        drop(out);

        let config = dir.join(format!("{}.config", super::INSPECTOR_EXE));
        super::write_no_follow(&config, super::INSPECTOR_CONFIG)
            .map_err(|e| format!("could not write {}: {e}", config.display()))?;

        // The staged copy is what actually runs, so it is what must be pinned
        // and re-verified — trusting the source handle alone would leave the
        // copy itself unchecked.
        let mut staged = super::open_locked(&dest)
            .map_err(|e| format!("could not lock {}: {e}", dest.display()))?;
        if !super::handle_is_trusted(&mut staged) {
            return Err("the staged Profile Inspector copy did not verify".to_string());
        }
        Ok((dest, staged))
    }

    /// The import-and-verify sequence, with `exe` already staged in `exe_dir`.
    fn run_in(
        exe: &Path,
        exe_dir: &Path,
        settings: &[(u32, u32)],
        cancel: &std::sync::atomic::AtomicBool,
        imported: &mut bool,
    ) -> Result<String, String> {
        // 1. Write the .nip into a private directory and merge-import it.
        //    `create_dir` fails if the name already exists, so winning that call
        //    proves exclusive ownership: no other process can have pre-created
        //    the path and no shared, predictable file is handed to the elevated
        //    child for it to re-open.
        let job_dir = crate::privdir::create("bdo-optimizer-nvidia-job")?;
        let nip = job_dir.join("profile.nip");
        let import = write_payload(&nip, settings).and_then(|payload| {
            // Hold the payload open with FILE_SHARE_READ for the whole import.
            // Writing then closing it would leave a window in which another
            // process could swap the file the elevated child re-opens by path;
            // this handle lets the child read it and nobody rewrite it.
            let nip_arg = nip.to_string_lossy().into_owned();
            let result = run_inspector_cancellable(
                exe,
                &["-mergeImport", "-silentImport", &nip_arg],
                cancel,
            );
            drop(payload);
            result
        });
        let _ = std::fs::remove_dir_all(&job_dir);
        import?;
        // Past this point the driver profile has been written. A later failure
        // is a *verification* failure, not proof that nothing changed.
        *imported = true;
        if cancelled(cancel) {
            return Err("cancelled after the profile was written".to_string());
        }

        // 2. The silent import reports nothing, so verify through an export.
        //
        // The export lands beside the executable under a timestamped name we do
        // not choose. That used to mean guessing which file was ours among the
        // user's own exports in the app folder — and an elaborate stash-and-
        // restore dance to avoid consuming one of theirs. Running from a
        // private directory removes the problem instead of managing it: this
        // folder was created empty moments ago and nothing else can see it, so
        // whatever is here now came from the call above.
        run_inspector_cancellable(exe, &["-exportCustomized"], cancel)?;
        let created = export_files(exe_dir);
        let export = match created.as_slice() {
            [only] => only,
            [] => return Err("verification export produced no file".to_string()),
            many => {
                return Err(format!(
                    "Profile Inspector wrote {} exports into its own private folder, so this \
                     run's export could not be identified.",
                    many.len()
                ))
            }
        };
        let bytes = std::fs::read(export)
            .map_err(|e| format!("could not read {}: {e}", export.display()))?;

        if super::export_confirms(&super::decode_export(&bytes), settings) {
            Ok(format!(
                "Verified: the driver's \"{}\" profile now carries all {} settings.",
                super::PROFILE_NAME,
                settings.len()
            ))
        } else {
            // Keep the evidence: without it a verification failure cannot be
            // debugged, because the export is deleted above. Only claim it was
            // kept if writing it actually worked.
            let kept = std::env::temp_dir().join("bdo-optimizer-verify-failed.nip");
            let where_kept = match std::fs::write(&kept, &bytes) {
                Ok(()) => format!(" (Export kept at {})", kept.display()),
                Err(e) => format!(" (The export could not be saved for inspection: {e})"),
            };
            Err(format!(
                "Import ran but the driver database does not show the expected \
                 values on the \"{}\" profile. Open NVIDIA Profile Inspector \
                 manually to check for a renamed profile.{where_kept}",
                super::PROFILE_NAME
            ))
        }
    }

    /// Create the import payload and return a **read** handle to hold while the
    /// child runs.
    ///
    /// The handle must be read-only. Windows grants a second open only if each
    /// side's access is permitted by the other's share mode, so retaining a
    /// *write* handle — even one sharing reads — makes the inspector's ordinary
    /// read open fail with a sharing violation. A read handle shared for reading
    /// lets the child read while still blocking any writer.
    ///
    /// Writing happens first through a `create_new` handle that shares nothing,
    /// so a pre-positioned file at this name is never followed. The reopened
    /// bytes are compared against what was written, which closes the brief
    /// close-then-reopen window: a swap in between fails the comparison instead
    /// of being imported.
    fn write_payload(nip: &Path, settings: &[(u32, u32)]) -> Result<std::fs::File, String> {
        use std::io::{Read, Write};
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 0x0000_0001;

        let expected = super::utf16le_bytes(&super::nip_xml(settings));
        {
            let mut writer = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .share_mode(0) // exclusive while the bytes are being written
                .open(nip)
                .map_err(|e| format!("could not create {}: {e}", nip.display()))?;
            writer
                .write_all(&expected)
                .and_then(|()| writer.sync_all())
                .map_err(|e| format!("could not write {}: {e}", nip.display()))?;
        }

        let mut reader = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .open(nip)
            .map_err(|e| format!("could not reopen {}: {e}", nip.display()))?;
        let mut actual = Vec::with_capacity(expected.len());
        reader
            .read_to_end(&mut actual)
            .map_err(|e| format!("could not verify {}: {e}", nip.display()))?;
        if actual != expected {
            return Err(format!(
                "{} changed after it was written — refusing to import it",
                nip.display()
            ));
        }
        Ok(reader)
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

    fn cancelled(flag: &std::sync::atomic::AtomicBool) -> bool {
        flag.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Spawn the inspector without a console window and wait for it to exit,
    /// killing it if `cancel` is raised.
    fn run_inspector_cancellable(
        exe: &Path,
        args: &[&str],
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<(), String> {
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
                Ok(None) if cancelled(cancel) => {
                    // The app is closing. Killing the child here is the whole
                    // point: leaving it running would let an elevated Profile
                    // Inspector keep writing the driver profile after the UI
                    // is gone, with nothing left to verify or clean up.
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("cancelled while Profile Inspector was running".to_string());
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
    #[cfg(test)]
    mod tests {
        use super::*;

        /// The payload handle is held across the child run, so a reader must
        /// still be able to open the file. A retained *write* handle makes the
        /// inspector's ordinary read open fail with a sharing violation and
        /// breaks the whole feature — this reproduces that without needing the
        /// driver, elevation, or a UAC prompt.
        #[test]
        fn a_reader_can_open_the_payload_while_it_is_held() {
            use std::io::Read;
            use std::os::windows::fs::OpenOptionsExt;
            const FILE_SHARE_READ: u32 = 0x0000_0001;

            let dir = crate::privdir::create("bdo-nvidia-test").expect("private dir");
            let nip = dir.join("profile.nip");
            let settings = super::super::guide_settings(8, true);
            let held = write_payload(&nip, &settings).expect("payload");

            // Mirrors how a .NET reader opens a file: read access, shared for
            // reading only.
            let mut reader = std::fs::OpenOptions::new()
                .read(true)
                .share_mode(FILE_SHARE_READ)
                .open(&nip)
                .expect("a reader must be able to open the held payload");
            let mut bytes = Vec::new();
            reader.read_to_end(&mut bytes).expect("read payload");
            assert_eq!(
                bytes,
                super::super::utf16le_bytes(&super::super::nip_xml(&settings))
            );

            // And a writer must still be locked out while we hold it.
            let writer = std::fs::OpenOptions::new().write(true).open(&nip);
            assert!(writer.is_err(), "payload must not be writable while held");

            drop(held);
            let _ = std::fs::remove_dir_all(dir);
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

    /// Undo must not reset settings the app never wrote. With ULL left off —
    /// the default — a blanket restore wiped a Low Latency configuration the
    /// user had set up themselves.
    #[test]
    fn restore_covers_only_what_was_applied() {
        let applied: Vec<u32> = guide_settings(8, false).iter().map(|(id, _)| *id).collect();
        let restore = restore_settings(&applied);
        assert_eq!(restore.len(), 3, "{restore:?}");
        for id in [ULL_ENABLED, ULL_CPL_STATE, MAX_PRERENDERED_FRAMES] {
            assert!(
                !restore.iter().any(|(r, _)| *r == id),
                "an untouched ULL setting must not be reset: {id}"
            );
        }

        // With ULL on, all six were written, so all six come back.
        let with_ull: Vec<u32> = guide_settings(8, true).iter().map(|(id, _)| *id).collect();
        assert_eq!(restore_settings(&with_ull).len(), 6);
    }

    /// No record means no evidence this app ever wrote the profile. Assuming
    /// the usual three would let Restore reset a Black Desert profile the user
    /// configured entirely themselves.
    #[test]
    fn restore_without_a_record_touches_nothing() {
        assert!(restore_settings(&[]).is_empty());
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
            assert!(result.result.is_ok(), "{label}: {:?}", result.result);
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
