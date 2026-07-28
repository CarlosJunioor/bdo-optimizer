//! Guide config-file tweaks: `postFilter` and `Tessellation` set to off in
//! `Documents\Black Desert\GameOption.txt` and every `gamevariable.xml` under
//! `UserCache` (the guide's "PostFilter = 0" edit, which disables the forced
//! post-process sharpening, plus the Tessellation FPS gain on High+ presets).
//!
//! Safety model:
//!
//! * Edits are value swaps that preserve every other byte of the file, verified
//!   by unit tests. A file whose values already match is not rewritten at all.
//! * Anything the parsers do not fully understand is left **verbatim** and not
//!   counted: a malformed line or an unterminated XML attribute never causes a
//!   rewrite, so a half-written file cannot be corrupted further.
//! * Live files and backups are written through an exclusively-created temp
//!   sibling plus an atomic replace that preserves the destination's ACL, so a
//!   crash mid-write cannot leave a truncated config or a truncated backup.
//! * Before a file is first modified its original bytes go to
//!   `<name>.bdo-optimizer.bak`; **Restore** copies those back, and can also
//!   recreate a live file the game (or the user) deleted.
//! * Paths are canonicalized and required to stay under the config root both
//!   at discovery *and* immediately before each write, so a directory junction
//!   swapped in afterwards cannot redirect one.
//! * Every file is processed only while `BlackDesert64.exe` is absent — the
//!   caller's guard is re-checked before each file, and once it trips the rest
//!   of the batch is skipped for good.

use std::path::{Path, PathBuf};

/// Suffix appended to a config file's name for its one-time backup.
pub const BACKUP_SUFFIX: &str = ".bdo-optimizer.bak";

/// `Documents\Black Desert`, where BDO keeps its per-user config.
pub fn config_root() -> Option<PathBuf> {
    directories::UserDirs::new()?
        .document_dir()
        .map(|d| d.join("Black Desert"))
}

/// Result of scanning the config root.
#[derive(Default)]
pub struct Discovery {
    /// Config files to act on (live files, plus files recoverable from backup).
    pub files: Vec<PathBuf>,
    /// Directories that could not be enumerated, and why. Surfaced in the UI so
    /// a partial scan is never mistaken for "there was nothing to do".
    pub unreadable: Vec<String>,
}

/// Find every guide-relevant config file under `root`: `GameOption.txt`, plus
/// `UserCache\gamevariable.xml` and one per account directory below it.
///
/// A file whose live copy is missing but whose `.bdo-optimizer.bak` survives is
/// still returned, so **Restore** can put a deleted config back.
pub fn discover(root: &Path) -> Discovery {
    let mut found = Discovery::default();

    consider(root, root.join("GameOption.txt"), &mut found);
    let user_cache = root.join("UserCache");
    consider(root, user_cache.join("gamevariable.xml"), &mut found);

    match std::fs::read_dir(&user_cache) {
        Ok(entries) => {
            for entry in entries {
                match entry {
                    Ok(entry) => match entry.file_type() {
                        // `file_type` does not follow links, so a linked account
                        // directory needs the following `metadata` call. Letting
                        // it through is safe: `consider` still requires the path
                        // to resolve inside the config root.
                        Ok(kind) if kind.is_dir() || kind.is_symlink() => {
                            let is_dir = kind.is_dir()
                                || std::fs::metadata(entry.path()).is_ok_and(|m| m.is_dir());
                            if is_dir {
                                consider(root, entry.path().join("gamevariable.xml"), &mut found);
                            }
                        }
                        Ok(_) => {}
                        // A directory we cannot even classify must be reported;
                        // treating the error as "not a directory" would hide a
                        // whole account's config behind a clean-looking scan.
                        Err(e) => found
                            .unreadable
                            .push(format!("{}: {e}", entry.path().display())),
                    },
                    Err(e) => found
                        .unreadable
                        .push(format!("{}: {e}", user_cache.display())),
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => found
            .unreadable
            .push(format!("{}: {e}", user_cache.display())),
    }
    found
}

fn consider(root: &Path, path: PathBuf, found: &mut Discovery) {
    let live = is_readable_file(&path, found);
    let recoverable = is_readable_file(&backup_path(&path), found);
    if !(live || recoverable) {
        return;
    }
    // A candidate that exists but resolves outside the root is *skipped on
    // purpose* — say so, rather than letting it vanish from a clean-looking
    // scan the way an ordinary absent file does.
    if !is_within(root, &path) {
        found.unreadable.push(format!(
            "{} resolves outside {} — skipped",
            path.display(),
            root.display()
        ));
        return;
    }
    if !found.files.contains(&path) {
        found.files.push(path);
    }
}

/// Whether `path` is a regular file we can actually read, recording *why* when
/// the answer could not be determined. A permission error must not silently
/// read as "absent" — and attribute access does not imply data access, so the
/// file is opened rather than merely stat'd.
fn is_readable_file(path: &Path, found: &mut Discovery) -> bool {
    match std::fs::metadata(path) {
        Ok(m) if !m.is_file() => false,
        Ok(_) => match std::fs::File::open(path) {
            Ok(_) => true,
            Err(e) => {
                found
                    .unreadable
                    .push(format!("{} could not be opened: {e}", path.display()));
                false
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => {
            found
                .unreadable
                .push(format!("{} could not be inspected: {e}", path.display()));
            false
        }
    }
}

/// True when `path` really lives under `root` after resolving symlinks and
/// Windows junctions. The file itself may not exist yet (restore recreates it),
/// so the *parent* directory is what gets canonicalized.
fn is_within(root: &Path, path: &Path) -> bool {
    let Ok(root) = root.canonicalize() else {
        return false;
    };
    let Some(parent) = path.parent() else {
        return false;
    };
    let Ok(parent) = parent.canonicalize() else {
        return false;
    };
    // A file must also not itself be a link pointing elsewhere.
    let same_name = match path.canonicalize() {
        Ok(real) => real.starts_with(&root),
        // Not existing yet is fine; the parent check below still applies.
        Err(_) => true,
    };
    parent.starts_with(&root) && same_name
}

/// How many settings a parser recognized, and how many it had to change.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PatchStats {
    /// Settings understood by the parser (0 = nothing recognizable in the file).
    pub found: usize,
    /// Settings whose value differed and was rewritten.
    pub changed: usize,
}

/// Split text into lines, keeping each line's own ending, and treating a lone
/// `\r` as a terminator too (BDO writes CRLF, but a CR-only file must not
/// collapse into a single "line" — that would swallow the whole config).
fn lines_inclusive(text: &str) -> Vec<&str> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\n' => {
                out.push(&text[start..=i]);
                i += 1;
                start = i;
            }
            b'\r' => {
                // CRLF counts as one ending.
                let end = if bytes.get(i + 1) == Some(&b'\n') {
                    i + 1
                } else {
                    i
                };
                out.push(&text[start..=end]);
                i = end + 1;
                start = i;
            }
            _ => i += 1,
        }
    }
    if start < text.len() {
        out.push(&text[start..]);
    }
    out
}

/// Patch `GameOption.txt` text: `postFilter = 0` and `Tessellation = 0`.
///
/// Only the value of those two keys changes; every other byte, including line
/// endings and unrelated lines, is preserved.
pub fn patch_game_option(text: &str) -> (String, PatchStats) {
    let mut stats = PatchStats::default();
    let mut out = String::with_capacity(text.len());
    for line in lines_inclusive(text) {
        out.push_str(&patch_option_line(line, &mut stats));
    }
    (out, stats)
}

fn patch_option_line(line: &str, stats: &mut PatchStats) -> String {
    let Some(eq) = line.find('=') else {
        return line.to_string();
    };
    let key = line[..eq].trim();
    if !key.eq_ignore_ascii_case("postFilter") && !key.eq_ignore_ascii_case("Tessellation") {
        return line.to_string();
    }
    let value_part = &line[eq + 1..];
    let trimmed = value_part.trim_end_matches(['\r', '\n']);
    let (value, ending) = value_part.split_at(trimmed.len());
    stats.found += 1;
    if value.trim() == "0" {
        return line.to_string();
    }
    stats.changed += 1;
    format!("{}= 0{}", &line[..eq], ending)
}

/// Patch `gamevariable.xml` text: every `<PostFilter Value="…"/>` becomes `"0"`
/// and every `<Tessellation Value="…"/>` becomes `"false"` (the guide says all
/// matching entries).
pub fn patch_game_variable(text: &str) -> (String, PatchStats) {
    let mut stats = PatchStats::default();
    let text = set_attr_values(text, "<PostFilter Value=\"", "0", &mut stats);
    let text = set_attr_values(&text, "<Tessellation Value=\"", "false", &mut stats);
    (text, stats)
}

fn set_attr_values(
    text: &str,
    tag_prefix: &str,
    new_value: &str,
    stats: &mut PatchStats,
) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(found) = rest.find(tag_prefix) {
        let value_start = found + tag_prefix.len();
        out.push_str(&rest[..value_start]);
        rest = &rest[value_start..];

        // The closing quote must come before the tag ends. Searching the whole
        // remaining document instead would let a later tag's quote "close" this
        // attribute and delete everything between them.
        let terminator = rest.find(['"', '<', '>']);
        match terminator {
            Some(end) if rest.as_bytes()[end] == b'"' => {
                if &rest[..end] != new_value {
                    stats.changed += 1;
                }
                stats.found += 1;
                out.push_str(new_value);
                rest = &rest[end..];
            }
            // Unterminated attribute (truncated / malformed file): copy verbatim
            // and do not count it, so nothing is rewritten on its account.
            _ => continue,
        }
    }
    out.push_str(rest);
    out
}

/// What happened to one file during apply/restore.
pub struct FileOutcome {
    pub path: PathBuf,
    pub result: Result<FileChange, String>,
}

/// The meaningful end states of acting on one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileChange {
    /// `n` values were rewritten to the guide values.
    Patched(usize),
    /// Every recognized setting already had the guide value.
    AlreadyOptimized,
    /// The parser recognized none of the settings — nothing was touched. This is
    /// reported distinctly so it can never read as "already optimized".
    NothingRecognized,
    /// The original file was put back from its backup.
    Restored,
    /// There was no backup to restore from.
    NoBackup,
    /// Skipped because the caller's guard said it was unsafe to continue.
    Skipped,
}

/// Run `act` over every path, stopping for good the first time `safe` says the
/// environment changed.
///
/// The stop is **latched**: once unsafe, every remaining file reports
/// [`FileChange::Skipped`] even if the condition flickers back. Re-checking
/// independently would let a run the UI describes as "stopped early" keep
/// writing files afterwards.
fn run_over(
    root: &Path,
    paths: &[PathBuf],
    safe: &dyn Fn() -> bool,
    act: impl Fn(&Path, &Path) -> Result<FileChange, String>,
) -> Vec<FileOutcome> {
    let mut stopped = false;
    paths
        .iter()
        .map(|path| {
            if !stopped && !safe() {
                stopped = true;
            }
            FileOutcome {
                path: path.clone(),
                result: if stopped {
                    Ok(FileChange::Skipped)
                } else {
                    act(root, path)
                },
            }
        })
        .collect()
}

/// Back up (first time only) and patch every file.
pub fn apply_files(root: &Path, paths: &[PathBuf], safe: &dyn Fn() -> bool) -> Vec<FileOutcome> {
    run_over(root, paths, safe, apply_one)
}

fn apply_one(root: &Path, path: &Path) -> Result<FileChange, String> {
    // Held for the whole operation, not just re-checked once: see `pin_and_verify`.
    let _pinned = pin_and_verify(root, path)?;
    let bytes = std::fs::read(path).map_err(|e| format!("read failed: {e}"))?;
    let text = String::from_utf8(bytes).map_err(|_| "unexpected non-UTF-8 content".to_string())?;
    let is_xml = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("xml"));
    let (patched, stats) = if is_xml {
        patch_game_variable(&text)
    } else {
        patch_game_option(&text)
    };
    if stats.found == 0 {
        return Ok(FileChange::NothingRecognized);
    }
    if stats.changed == 0 {
        return Ok(FileChange::AlreadyOptimized);
    }
    let backup = backup_path(path);
    // A zero-length backup is treated as absent: it would be a useless restore
    // point, and trusting mere existence would also skip making a real one.
    let has_backup = std::fs::metadata(&backup).is_ok_and(|m| m.is_file() && m.len() > 0);
    if !has_backup {
        write_atomic(&backup, &text).map_err(|e| format!("backup failed: {e}"))?;
    }
    write_atomic(path, &patched).map_err(|e| format!("write failed: {e}"))?;
    Ok(FileChange::Patched(stats.changed))
}

/// Copy each file's backup over the live file, recreating it if it is gone.
pub fn restore_files(root: &Path, paths: &[PathBuf], safe: &dyn Fn() -> bool) -> Vec<FileOutcome> {
    run_over(root, paths, safe, restore_one)
}

fn restore_one(root: &Path, path: &Path) -> Result<FileChange, String> {
    let _pinned = pin_and_verify(root, path)?;
    let backup = backup_path(path);
    let usable = std::fs::metadata(&backup).is_ok_and(|m| m.is_file() && m.len() > 0);
    if !usable {
        return Ok(FileChange::NoBackup);
    }
    let bytes = std::fs::read(&backup).map_err(|e| format!("backup unreadable: {e}"))?;
    write_atomic_bytes(path, &bytes).map_err(|e| format!("restore failed: {e}"))?;
    Ok(FileChange::Restored)
}

/// Pin the containing directory, then verify containment — in that order.
///
/// Checking a pathname and then acting on it is inherently racy: the directory
/// can be swapped for a junction in between, and every later pathname operation
/// (read, backup, replace) would follow the new target. Holding an open handle
/// on the directory with a share mode that omits `FILE_SHARE_DELETE` means it
/// cannot be renamed or deleted while we work, so the identity we validated is
/// the identity we act on. The returned guard must be kept alive for the whole
/// operation.
#[must_use = "dropping the guard releases the directory pin"]
struct DirGuard(#[allow(dead_code)] Option<std::fs::File>);

fn pin_and_verify(root: &Path, path: &Path) -> Result<DirGuard, String> {
    let parent = path
        .parent()
        .ok_or_else(|| "path has no parent directory".to_string())?;

    #[cfg(windows)]
    let handle = {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_READ_ATTRIBUTES: u32 = 0x0080;
        const FILE_SHARE_READ: u32 = 0x0000_0001;
        const FILE_SHARE_WRITE: u32 = 0x0000_0002;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        Some(
            std::fs::OpenOptions::new()
                .access_mode(FILE_READ_ATTRIBUTES)
                // Deliberately omits FILE_SHARE_DELETE: that is what blocks the
                // rename/delete a junction swap would need.
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
                .open(parent)
                .map_err(|e| format!("could not pin {}: {e}", parent.display()))?,
        )
    };
    #[cfg(not(windows))]
    let handle = None;

    if !is_within(root, path) {
        return Err("resolves outside the BDO config folder — refusing to touch it".to_string());
    }
    Ok(DirGuard(handle))
}

/// Write via a temp sibling + atomic replace so the destination is never
/// observed half-written: readers see either the old bytes or the new ones.
fn write_atomic(path: &Path, contents: &str) -> std::io::Result<()> {
    write_atomic_bytes(path, contents.as_bytes())
}

fn write_atomic_bytes(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write;

    let (mut file, tmp) = create_temp_sibling(path)?;
    let write = file
        .write_all(contents)
        // Flush to disk before the replace; otherwise a crash can leave the
        // renamed file present but empty.
        .and_then(|()| file.sync_all());
    drop(file);

    if let Err(e) = write {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    match replace_file(&tmp, path) {
        Ok(()) => Ok(()),
        Err(Replace { error, consumed }) => {
            // Only clean up when the replacement is known to be untouched. If
            // the API may already have moved it into place, deleting it here
            // could destroy the only copy of the destination's contents.
            if !consumed {
                let _ = std::fs::remove_file(&tmp);
            }
            Err(error)
        }
    }
}

/// A failed replace, and whether the temp file may already have been consumed.
struct Replace {
    error: std::io::Error,
    consumed: bool,
}

/// Create an exclusively-owned temp file next to `path`.
///
/// `create_new` is the important part: it fails when the name already exists,
/// so a pre-positioned file or reparse point cannot be followed and truncated.
/// The name also varies per attempt, so a squatter cannot camp one guess.
fn create_temp_sibling(path: &Path) -> std::io::Result<(std::fs::File, PathBuf)> {
    let pid = std::process::id();
    for attempt in 0..64u32 {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(attempt);
        let mut name = path.file_name().unwrap_or_default().to_os_string();
        name.push(format!(".{pid}-{nanos}-{attempt}.tmp"));
        let candidate = path.with_file_name(name);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => return Ok((file, candidate)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::other(
        "could not create a temporary file next to the config",
    ))
}

/// Atomically put `tmp` in place of `dest`.
///
/// On Windows this uses `ReplaceFileW` with no flags, which keeps the
/// *destination's* security descriptor and attributes. A plain rename would
/// hand the config the temp file's inherited ACL instead, discarding
/// permissions the user set on it — so rename is used only where it cannot
/// cause that loss:
///
/// * the destination does not exist yet (restore recreating a deleted file),
///   where there is no ACL to preserve; or
/// * the filesystem does not implement `ReplaceFileW` at all, where preserving
///   the ACL is not achievable by any path.
///
/// Every other failure is reported instead of being papered over with a rename.
fn replace_file(tmp: &Path, dest: &Path) -> Result<(), Replace> {
    #[cfg(windows)]
    return replace_file_windows(tmp, dest);
    #[cfg(not(windows))]
    std::fs::rename(tmp, dest).map_err(|error| Replace {
        error,
        consumed: false,
    })
}

/// The Win32 error code carried by a `FACILITY_WIN32` HRESULT, if it is one.
#[cfg(windows)]
fn win32_code(e: &windows::core::Error) -> Option<u32> {
    let hr = e.code().0 as u32;
    // HRESULT_FROM_WIN32(x) == 0x8007_0000 | (x & 0xFFFF)
    (hr & 0xFFFF_0000 == 0x8007_0000).then_some(hr & 0xFFFF)
}

#[cfg(windows)]
fn replace_file_windows(tmp: &Path, dest: &Path) -> Result<(), Replace> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{
        ERROR_FILE_NOT_FOUND, ERROR_INVALID_FUNCTION, ERROR_NOT_SUPPORTED, ERROR_PATH_NOT_FOUND,
    };
    use windows::Win32::Storage::FileSystem::{ReplaceFileW, REPLACE_FILE_FLAGS};

    let wide = |p: &Path| -> Vec<u16> {
        p.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    };
    let dest_w = wide(dest);
    let tmp_w = wide(tmp);
    // SAFETY: both buffers are NUL-terminated and outlive the call; the
    // optional backup/reserved parameters are null.
    let replaced = unsafe {
        ReplaceFileW(
            PCWSTR(dest_w.as_ptr()),
            PCWSTR(tmp_w.as_ptr()),
            PCWSTR::null(),
            REPLACE_FILE_FLAGS(0),
            None,
            None,
        )
    };
    match replaced {
        Ok(()) => Ok(()),
        Err(e) => {
            // Only a FACILITY_WIN32 HRESULT (0x8007xxxx) carries a Win32 error
            // code in its low word. Masking unconditionally would let an
            // unrelated HRESULT whose low bits happen to equal 1/2/3/50 be
            // misread as "destination missing" and silently fall back to a
            // rename that discards the destination's ACL.
            let code = win32_code(&e);
            let missing_dest =
                code == Some(ERROR_FILE_NOT_FOUND.0) || code == Some(ERROR_PATH_NOT_FOUND.0);
            let unsupported =
                code == Some(ERROR_NOT_SUPPORTED.0) || code == Some(ERROR_INVALID_FUNCTION.0);
            if missing_dest || unsupported {
                return std::fs::rename(tmp, dest).map_err(|error| Replace {
                    error,
                    consumed: false,
                });
            }
            // Windows documents partial-failure states in which the replacement
            // has already been moved, so the caller must not assume `tmp` is
            // still a disposable scratch file.
            Err(Replace {
                error: std::io::Error::other(format!(
                    "could not replace {}: {e} (a replacement may remain at {})",
                    dest.display(),
                    tmp.display()
                )),
                consumed: true,
            })
        }
    }
}

fn backup_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(BACKUP_SUFFIX);
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    const GAME_OPTION: &str = "version = 1002\r\nantiAliasing = 1\r\nTessellation = 1\r\npostFilter = 1\r\ncameraLUTFilter = None\r\n";

    fn always_safe() -> &'static dyn Fn() -> bool {
        &|| true
    }

    #[test]
    fn game_option_patch_changes_only_the_two_keys() {
        let (patched, stats) = patch_game_option(GAME_OPTION);
        assert_eq!(
            stats,
            PatchStats {
                found: 2,
                changed: 2
            }
        );
        assert_eq!(
            patched,
            "version = 1002\r\nantiAliasing = 1\r\nTessellation = 0\r\npostFilter = 0\r\ncameraLUTFilter = None\r\n"
        );
    }

    #[test]
    fn game_option_patch_is_idempotent() {
        let (once, _) = patch_game_option(GAME_OPTION);
        let (twice, stats) = patch_game_option(&once);
        assert_eq!(
            stats,
            PatchStats {
                found: 2,
                changed: 0
            }
        );
        assert_eq!(twice, once);
    }

    #[test]
    fn cr_only_file_keeps_every_other_setting() {
        // Regression: splitting on '\n' alone made this one giant "line", so the
        // whole file after the first '=' was replaced by the new value.
        let text = "postFilter = 1\rTessellation = 1\rfoo = keepme\r";
        let (patched, stats) = patch_game_option(text);
        assert_eq!(
            stats,
            PatchStats {
                found: 2,
                changed: 2
            }
        );
        assert_eq!(patched, "postFilter = 0\rTessellation = 0\rfoo = keepme\r");
    }

    #[test]
    fn mixed_and_missing_final_newline_survive() {
        let text = "Tessellation = 1\nfoo = 1\r\npostFilter = 2";
        let (patched, stats) = patch_game_option(text);
        assert_eq!(stats.changed, 2);
        assert_eq!(patched, "Tessellation = 0\nfoo = 1\r\npostFilter = 0");
    }

    #[test]
    fn game_option_patch_keeps_lines_without_equals() {
        let (patched, stats) = patch_game_option("just a line\r\n");
        assert_eq!(patched, "just a line\r\n");
        assert_eq!(stats, PatchStats::default());
    }

    const GAME_VARIABLE: &str = "<GameOptionGlobal Version=\"2\">\n<PostFilter Value=\"2\"/>\n<Tessellation Value=\"true\"/>\n<Other Value=\"7\"/>\n<PostFilter Value=\"1\"/>\n<Tessellation Value=\"false\"/>\n</GameOptionGlobal>\n";

    #[test]
    fn game_variable_patch_rewrites_every_entry() {
        let (patched, stats) = patch_game_variable(GAME_VARIABLE);
        assert_eq!(
            stats,
            PatchStats {
                found: 4,
                changed: 3
            }
        );
        assert_eq!(patched.matches("<PostFilter Value=\"0\"/>").count(), 2);
        assert_eq!(
            patched.matches("<Tessellation Value=\"false\"/>").count(),
            2
        );
        assert!(patched.contains("<Other Value=\"7\"/>"));
    }

    #[test]
    fn game_variable_patch_is_idempotent() {
        let (once, _) = patch_game_variable(GAME_VARIABLE);
        let (twice, stats) = patch_game_variable(&once);
        assert_eq!(stats.changed, 0);
        assert_eq!(twice, once);
    }

    #[test]
    fn unterminated_attribute_is_left_verbatim() {
        // Regression: the closing-quote search ran past the tag, so the quote
        // before `7` closed the first attribute and the XML between was deleted.
        let text = "<PostFilter Value=\"2/>\n<Other Value=\"7\"/>\n";
        let (patched, stats) = patch_game_variable(text);
        assert_eq!(patched, text, "malformed input must not be rewritten");
        assert_eq!(stats, PatchStats::default());
    }

    #[test]
    fn apply_reports_nothing_recognized_separately() {
        let dir = temp_dir("norecognize");
        let file = dir.join("GameOption.txt");
        std::fs::write(&file, "version = 1002\r\n").unwrap();
        let out = apply_files(&dir, &[file], always_safe());
        assert_eq!(
            out[0].result.as_ref().unwrap(),
            &FileChange::NothingRecognized
        );
        assert!(!dir.join(format!("GameOption.txt{BACKUP_SUFFIX}")).exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn apply_backs_up_then_restore_returns_original() {
        let dir = temp_dir("roundtrip");
        let file = dir.join("GameOption.txt");
        std::fs::write(&file, GAME_OPTION).unwrap();
        let paths = vec![file.clone()];

        let applied = apply_files(&dir, &paths, always_safe());
        assert_eq!(applied[0].result.as_ref().unwrap(), &FileChange::Patched(2));
        let backup = dir.join(format!("GameOption.txt{BACKUP_SUFFIX}"));
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), GAME_OPTION);

        // A second apply is a no-op and must not clobber the backup.
        let again = apply_files(&dir, &paths, always_safe());
        assert_eq!(
            again[0].result.as_ref().unwrap(),
            &FileChange::AlreadyOptimized
        );
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), GAME_OPTION);

        let restored = restore_files(&dir, &paths, always_safe());
        assert_eq!(restored[0].result.as_ref().unwrap(), &FileChange::Restored);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), GAME_OPTION);

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn empty_backup_is_not_trusted_and_gets_rewritten() {
        let dir = temp_dir("emptybak");
        let file = dir.join("GameOption.txt");
        std::fs::write(&file, GAME_OPTION).unwrap();
        let backup = dir.join(format!("GameOption.txt{BACKUP_SUFFIX}"));
        // Simulate a crashed/disk-full backup attempt.
        std::fs::write(&backup, "").unwrap();

        let paths = vec![file.clone()];
        apply_files(&dir, &paths, always_safe());
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), GAME_OPTION);

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn restore_recreates_a_deleted_live_file() {
        let dir = temp_dir("recreate");
        let root_files = dir.join("GameOption.txt");
        std::fs::write(
            dir.join(format!("GameOption.txt{BACKUP_SUFFIX}")),
            GAME_OPTION,
        )
        .unwrap();
        assert!(!root_files.exists());

        // Discovery must offer it even though the live file is gone.
        let discovered = discover(&dir);
        assert!(discovered.files.contains(&root_files));

        let restored = restore_files(&dir, &discovered.files, always_safe());
        assert_eq!(restored[0].result.as_ref().unwrap(), &FileChange::Restored);
        assert_eq!(std::fs::read_to_string(&root_files).unwrap(), GAME_OPTION);

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn restore_without_backup_reports_no_backup() {
        let dir = temp_dir("nobak");
        let file = dir.join("GameOption.txt");
        std::fs::write(&file, GAME_OPTION).unwrap();
        let out = restore_files(&dir, std::slice::from_ref(&file), always_safe());
        assert_eq!(out[0].result.as_ref().unwrap(), &FileChange::NoBackup);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), GAME_OPTION);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unsafe_guard_skips_every_file_without_writing() {
        let dir = temp_dir("guard");
        let file = dir.join("GameOption.txt");
        std::fs::write(&file, GAME_OPTION).unwrap();
        let out = apply_files(&dir, std::slice::from_ref(&file), &|| false);
        assert_eq!(out[0].result.as_ref().unwrap(), &FileChange::Skipped);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), GAME_OPTION);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn stop_is_latched_once_the_guard_trips() {
        // A flickering guard must not let later files be written after the UI
        // has already reported that the run stopped early.
        let dir = temp_dir("latch");
        let mut paths = Vec::new();
        for i in 0..3 {
            let file = dir.join(format!("GameOption{i}.txt"));
            std::fs::write(&file, GAME_OPTION).unwrap();
            paths.push(file);
        }
        let calls = std::cell::Cell::new(0);
        let flickering = || {
            calls.set(calls.get() + 1);
            calls.get() != 2 // safe, unsafe, safe…
        };
        let out = apply_files(&dir, &paths, &flickering);
        assert_eq!(out[0].result.as_ref().unwrap(), &FileChange::Patched(2));
        assert_eq!(out[1].result.as_ref().unwrap(), &FileChange::Skipped);
        assert_eq!(out[2].result.as_ref().unwrap(), &FileChange::Skipped);
        // The third file must still hold its original bytes.
        assert_eq!(std::fs::read_to_string(&paths[2]).unwrap(), GAME_OPTION);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn writes_outside_the_root_are_refused() {
        let root = temp_dir("rootguard");
        let outside = temp_dir("outside");
        let stray = outside.join("GameOption.txt");
        std::fs::write(&stray, GAME_OPTION).unwrap();

        let out = apply_files(&root, std::slice::from_ref(&stray), always_safe());
        assert!(out[0].result.is_err(), "{:?}", out[0].result);
        assert_eq!(std::fs::read_to_string(&stray).unwrap(), GAME_OPTION);

        let out = restore_files(&root, std::slice::from_ref(&stray), always_safe());
        assert!(out[0].result.is_err());

        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(outside).unwrap();
    }

    #[test]
    fn out_of_root_candidates_are_reported_not_silently_dropped() {
        let root = temp_dir("reportskip");
        let outside = temp_dir("reportskip-out");
        let stray = outside.join("GameOption.txt");
        std::fs::write(&stray, GAME_OPTION).unwrap();

        let mut found = Discovery::default();
        consider(&root, stray.clone(), &mut found);
        assert!(found.files.is_empty());
        assert_eq!(found.unreadable.len(), 1, "{:?}", found.unreadable);
        assert!(found.unreadable[0].contains("resolves outside"));

        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(outside).unwrap();
    }

    #[test]
    fn apply_preserves_content_when_replace_succeeds() {
        // Guards the ReplaceFileW path end to end: the destination keeps its
        // identity and receives exactly the patched bytes.
        let dir = temp_dir("replace");
        let file = dir.join("GameOption.txt");
        std::fs::write(&file, GAME_OPTION).unwrap();
        let (expected, _) = patch_game_option(GAME_OPTION);

        let out = apply_files(&dir, std::slice::from_ref(&file), always_safe());
        assert_eq!(out[0].result.as_ref().unwrap(), &FileChange::Patched(2));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), expected);

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn atomic_write_leaves_no_temp_files_behind() {
        let dir = temp_dir("notemp");
        let file = dir.join("GameOption.txt");
        std::fs::write(&file, GAME_OPTION).unwrap();
        apply_files(&dir, std::slice::from_ref(&file), always_safe());

        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temp files left: {leftovers:?}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn discover_walks_user_cache_and_reports_nothing_missing() {
        let root = temp_dir("discover");
        let account = root.join("UserCache").join("12345");
        std::fs::create_dir_all(&account).unwrap();
        std::fs::write(root.join("GameOption.txt"), "x").unwrap();
        std::fs::write(root.join("UserCache").join("gamevariable.xml"), "x").unwrap();
        std::fs::write(account.join("gamevariable.xml"), "x").unwrap();
        // A per-account dir without the file is skipped.
        std::fs::create_dir_all(root.join("UserCache").join("empty")).unwrap();

        let found = discover(&root);
        assert_eq!(found.files.len(), 3);
        assert!(found.unreadable.is_empty());

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn discover_reports_a_missing_user_cache_without_error_noise() {
        let root = temp_dir("nousercache");
        std::fs::write(root.join("GameOption.txt"), "x").unwrap();
        let found = discover(&root);
        assert_eq!(found.files.len(), 1);
        assert!(found.unreadable.is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "bdo-gameconfig-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Manual check against copies of real BDO files:
    /// set `BDO_CFG_TEST_ROOT` to a folder mirroring `Documents\Black Desert`
    /// and run `cargo test -p bdo-optimizer dry_run_real -- --ignored --nocapture`.
    #[test]
    #[ignore = "needs BDO_CFG_TEST_ROOT pointing at copied config files"]
    fn dry_run_real_copies() {
        let root = PathBuf::from(std::env::var("BDO_CFG_TEST_ROOT").expect("BDO_CFG_TEST_ROOT"));
        let found = discover(&root);
        assert!(
            !found.files.is_empty(),
            "no config files under {}",
            root.display()
        );
        for outcome in apply_files(&root, &found.files, always_safe()) {
            println!("{} -> {:?}", outcome.path.display(), outcome.result);
            outcome.result.expect("patch failed");
        }
        // Idempotence on the real data: a second apply changes nothing.
        for outcome in apply_files(&root, &found.files, always_safe()) {
            let change = outcome.result.expect("second apply failed");
            assert!(
                matches!(
                    change,
                    FileChange::AlreadyOptimized | FileChange::NothingRecognized
                ),
                "{} changed twice: {change:?}",
                outcome.path.display()
            );
        }
    }
}
