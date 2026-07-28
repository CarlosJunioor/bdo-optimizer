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
//! * Live files and backups are written through a temp sibling plus rename, so
//!   a crash mid-write cannot leave a truncated config or a truncated backup.
//! * Before a file is first modified its original bytes go to
//!   `<name>.bdo-optimizer.bak`; **Restore** copies those back, and can also
//!   recreate a live file the game (or the user) deleted.
//! * Discovered paths are canonicalized and required to stay under the config
//!   root, so a directory junction under `UserCache` cannot redirect a write.
//! * Every file is processed only while `BlackDesert64.exe` is absent — the
//!   caller's guard is re-checked before each file, not once per batch.

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
    let consider = |path: PathBuf, found: &mut Discovery| {
        let live = path.is_file();
        let recoverable = backup_path(&path).is_file();
        if (live || recoverable) && is_within(root, &path) && !found.files.contains(&path) {
            found.files.push(path);
        }
    };

    consider(root.join("GameOption.txt"), &mut found);
    let user_cache = root.join("UserCache");
    consider(user_cache.join("gamevariable.xml"), &mut found);

    match std::fs::read_dir(&user_cache) {
        Ok(entries) => {
            for entry in entries {
                match entry {
                    Ok(entry) if entry.path().is_dir() => {
                        consider(entry.path().join("gamevariable.xml"), &mut found);
                    }
                    Ok(_) => {}
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

/// Back up (first time only) and patch every file.
///
/// `safe` is re-checked before each file so a batch stops as soon as the
/// environment changes (e.g. BDO launching mid-run); remaining files report
/// [`FileChange::Skipped`] rather than being written behind the game's back.
pub fn apply_files(paths: &[PathBuf], safe: &dyn Fn() -> bool) -> Vec<FileOutcome> {
    paths
        .iter()
        .map(|path| FileOutcome {
            path: path.clone(),
            result: if safe() {
                apply_one(path)
            } else {
                Ok(FileChange::Skipped)
            },
        })
        .collect()
}

fn apply_one(path: &Path) -> Result<FileChange, String> {
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
pub fn restore_files(paths: &[PathBuf], safe: &dyn Fn() -> bool) -> Vec<FileOutcome> {
    paths
        .iter()
        .map(|path| FileOutcome {
            path: path.clone(),
            result: if safe() {
                restore_one(path)
            } else {
                Ok(FileChange::Skipped)
            },
        })
        .collect()
}

fn restore_one(path: &Path) -> Result<FileChange, String> {
    let backup = backup_path(path);
    let usable = std::fs::metadata(&backup).is_ok_and(|m| m.is_file() && m.len() > 0);
    if !usable {
        return Ok(FileChange::NoBackup);
    }
    let bytes = std::fs::read(&backup).map_err(|e| format!("backup unreadable: {e}"))?;
    write_atomic_bytes(path, &bytes).map_err(|e| format!("restore failed: {e}"))?;
    Ok(FileChange::Restored)
}

/// Write via a temp sibling + rename so the destination is never observed
/// half-written: readers see either the old bytes or the new ones.
fn write_atomic(path: &Path, contents: &str) -> std::io::Result<()> {
    write_atomic_bytes(path, contents.as_bytes())
}

fn write_atomic_bytes(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write;

    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{}.tmp", std::process::id()));
    let tmp = path.with_file_name(name);

    let mut file = std::fs::File::create(&tmp)?;
    let write = file
        .write_all(contents)
        // Flush to disk before the rename; otherwise a crash can leave the
        // renamed file present but empty.
        .and_then(|()| file.sync_all());
    drop(file);
    if let Err(e) = write.and_then(|()| std::fs::rename(&tmp, path)) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
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
        let out = apply_files(&[file], always_safe());
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

        let applied = apply_files(&paths, always_safe());
        assert_eq!(applied[0].result.as_ref().unwrap(), &FileChange::Patched(2));
        let backup = dir.join(format!("GameOption.txt{BACKUP_SUFFIX}"));
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), GAME_OPTION);

        // A second apply is a no-op and must not clobber the backup.
        let again = apply_files(&paths, always_safe());
        assert_eq!(
            again[0].result.as_ref().unwrap(),
            &FileChange::AlreadyOptimized
        );
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), GAME_OPTION);

        let restored = restore_files(&paths, always_safe());
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
        apply_files(&paths, always_safe());
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

        let restored = restore_files(&discovered.files, always_safe());
        assert_eq!(restored[0].result.as_ref().unwrap(), &FileChange::Restored);
        assert_eq!(std::fs::read_to_string(&root_files).unwrap(), GAME_OPTION);

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn restore_without_backup_reports_no_backup() {
        let dir = temp_dir("nobak");
        let file = dir.join("GameOption.txt");
        std::fs::write(&file, GAME_OPTION).unwrap();
        let out = restore_files(std::slice::from_ref(&file), always_safe());
        assert_eq!(out[0].result.as_ref().unwrap(), &FileChange::NoBackup);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), GAME_OPTION);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unsafe_guard_skips_every_file_without_writing() {
        let dir = temp_dir("guard");
        let file = dir.join("GameOption.txt");
        std::fs::write(&file, GAME_OPTION).unwrap();
        let out = apply_files(std::slice::from_ref(&file), &|| false);
        assert_eq!(out[0].result.as_ref().unwrap(), &FileChange::Skipped);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), GAME_OPTION);
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
        for outcome in apply_files(&found.files, always_safe()) {
            println!("{} -> {:?}", outcome.path.display(), outcome.result);
            outcome.result.expect("patch failed");
        }
        // Idempotence on the real data: a second apply changes nothing.
        for outcome in apply_files(&found.files, always_safe()) {
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
