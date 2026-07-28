//! Guide config-file tweaks: `postFilter` and `Tessellation` set to off in
//! `Documents\Black Desert\GameOption.txt` and every `gamevariable.xml` under
//! `UserCache` (the guide's "PostFilter = 0" edit, which disables the forced
//! post-process sharpening, plus the Tessellation FPS gain on High+ presets).
//!
//! Safety model:
//!
//! * Edits are plain text/attribute value swaps that preserve every other byte
//!   of the file, verified by unit tests. Files that would not change are left
//!   completely untouched.
//! * Before a file is first modified, its original bytes are copied to
//!   `<name>.bdo-optimizer.bak` next to it; **Restore** copies the backups
//!   back. Backups are never overwritten by later applies, so the restore
//!   point is always the pre-optimizer state.
//! * The UI refuses to edit while `BlackDesert64.exe` is running — the game
//!   rewrites these files on exit and would clobber (or race) the edit.

use std::path::{Path, PathBuf};

/// Suffix appended to a config file's name for its one-time backup.
pub const BACKUP_SUFFIX: &str = ".bdo-optimizer.bak";

/// `Documents\Black Desert`, where BDO keeps its per-user config.
pub fn config_root() -> Option<PathBuf> {
    directories::UserDirs::new()?
        .document_dir()
        .map(|d| d.join("Black Desert"))
}

/// Every guide-relevant config file that exists under `root`:
/// `GameOption.txt`, plus `UserCache\gamevariable.xml` and one
/// `gamevariable.xml` per account directory below `UserCache`.
pub fn find_config_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let game_option = root.join("GameOption.txt");
    if game_option.is_file() {
        out.push(game_option);
    }
    let user_cache = root.join("UserCache");
    let direct = user_cache.join("gamevariable.xml");
    if direct.is_file() {
        out.push(direct);
    }
    if let Ok(entries) = std::fs::read_dir(&user_cache) {
        for entry in entries.flatten() {
            let candidate = entry.path().join("gamevariable.xml");
            if entry.path().is_dir() && candidate.is_file() {
                out.push(candidate);
            }
        }
    }
    out
}

/// Patch `GameOption.txt` text: `postFilter = 0` and `Tessellation = 0`.
///
/// Only the value after `=` on those two lines changes; every other byte,
/// including CRLF endings and unrelated lines, is preserved. Returns the new
/// text and how many lines actually changed (0 = already optimized).
pub fn patch_game_option(text: &str) -> (String, usize) {
    let mut changed = 0;
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        out.push_str(&patch_option_line(line, &mut changed));
    }
    (out, changed)
}

fn patch_option_line(line: &str, changed: &mut usize) -> String {
    let Some(eq) = line.find('=') else {
        return line.to_string();
    };
    let key = line[..eq].trim();
    if !key.eq_ignore_ascii_case("postFilter") && !key.eq_ignore_ascii_case("Tessellation") {
        return line.to_string();
    }
    let value_part = &line[eq + 1..];
    let ending_len = value_part.len() - value_part.trim_end_matches(['\r', '\n']).len();
    let (value, ending) = value_part.split_at(value_part.len() - ending_len);
    if value.trim() == "0" {
        return line.to_string();
    }
    *changed += 1;
    format!("{}= 0{}", &line[..eq], ending)
}

/// Patch `gamevariable.xml` text: every `<PostFilter Value="…"/>` becomes `"0"`
/// and every `<Tessellation Value="…"/>` becomes `"false"` (the guide says all
/// matching entries). Returns the new text and the number of changed values.
pub fn patch_game_variable(text: &str) -> (String, usize) {
    let mut changed = 0;
    let text = set_attr_values(text, "<PostFilter Value=\"", "0", &mut changed);
    let text = set_attr_values(&text, "<Tessellation Value=\"", "false", &mut changed);
    (text, changed)
}

fn set_attr_values(text: &str, tag_prefix: &str, new_value: &str, changed: &mut usize) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(found) = rest.find(tag_prefix) {
        let value_start = found + tag_prefix.len();
        out.push_str(&rest[..value_start]);
        let Some(quote) = rest[value_start..].find('"') else {
            // Malformed tail (unterminated attribute) — keep it verbatim.
            out.push_str(&rest[value_start..]);
            return out;
        };
        if &rest[value_start..value_start + quote] != new_value {
            *changed += 1;
        }
        out.push_str(new_value);
        rest = &rest[value_start + quote..];
    }
    out.push_str(rest);
    out
}

/// What happened to one file during apply/restore.
pub struct FileOutcome {
    pub path: PathBuf,
    /// `Ok(n)` = n values changed (0 = already optimized); `Err` = why skipped.
    pub result: Result<usize, String>,
}

/// Back up (first time only) and patch every file. Files already carrying the
/// guide values are reported as `Ok(0)` and not rewritten.
pub fn apply_files(paths: &[PathBuf]) -> Vec<FileOutcome> {
    paths
        .iter()
        .map(|path| FileOutcome {
            path: path.clone(),
            result: apply_one(path),
        })
        .collect()
}

fn apply_one(path: &Path) -> Result<usize, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("read failed: {e}"))?;
    let text = String::from_utf8(bytes).map_err(|_| "unexpected non-UTF-8 content".to_string())?;
    let is_xml = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("xml"));
    let (patched, changed) = if is_xml {
        patch_game_variable(&text)
    } else {
        patch_game_option(&text)
    };
    if changed == 0 {
        return Ok(0);
    }
    let backup = backup_path(path);
    if !backup.exists() {
        std::fs::write(&backup, &text).map_err(|e| format!("backup failed: {e}"))?;
    }
    std::fs::write(path, patched).map_err(|e| format!("write failed: {e}"))?;
    Ok(changed)
}

/// Copy each file's backup over the live file. Files without a backup (never
/// modified by us) are reported as `Ok(0)`.
pub fn restore_files(paths: &[PathBuf]) -> Vec<FileOutcome> {
    paths
        .iter()
        .map(|path| {
            let backup = backup_path(path);
            let result = if backup.is_file() {
                std::fs::copy(&backup, path)
                    .map(|_| 1)
                    .map_err(|e| format!("restore failed: {e}"))
            } else {
                Ok(0)
            };
            FileOutcome {
                path: path.clone(),
                result,
            }
        })
        .collect()
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

    #[test]
    fn game_option_patch_changes_only_the_two_keys() {
        let (patched, changed) = patch_game_option(GAME_OPTION);
        assert_eq!(changed, 2);
        assert_eq!(
            patched,
            "version = 1002\r\nantiAliasing = 1\r\nTessellation = 0\r\npostFilter = 0\r\ncameraLUTFilter = None\r\n"
        );
    }

    #[test]
    fn game_option_patch_is_idempotent() {
        let (once, _) = patch_game_option(GAME_OPTION);
        let (twice, changed) = patch_game_option(&once);
        assert_eq!(changed, 0);
        assert_eq!(twice, once);
    }

    #[test]
    fn game_option_patch_keeps_lines_without_equals() {
        let (patched, changed) = patch_game_option("just a line\r\n");
        assert_eq!((patched.as_str(), changed), ("just a line\r\n", 0));
    }

    const GAME_VARIABLE: &str = "<GameOptionGlobal Version=\"2\">\n<PostFilter Value=\"2\"/>\n<Tessellation Value=\"true\"/>\n<Other Value=\"7\"/>\n<PostFilter Value=\"1\"/>\n<Tessellation Value=\"false\"/>\n</GameOptionGlobal>\n";

    #[test]
    fn game_variable_patch_rewrites_every_entry() {
        let (patched, changed) = patch_game_variable(GAME_VARIABLE);
        // Three entries differ (PostFilter 2, PostFilter 1, Tessellation true);
        // the already-false Tessellation does not count.
        assert_eq!(changed, 3);
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
        let (twice, changed) = patch_game_variable(&once);
        assert_eq!(changed, 0);
        assert_eq!(twice, once);
    }

    #[test]
    fn apply_backs_up_then_restore_returns_original() {
        let dir = std::env::temp_dir().join(format!("bdo-gameconfig-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("GameOption.txt");
        std::fs::write(&file, GAME_OPTION).unwrap();

        let paths = vec![file.clone()];
        let applied = apply_files(&paths);
        assert_eq!(applied[0].result.as_ref().copied().unwrap(), 2);
        assert!(std::fs::read_to_string(&file)
            .unwrap()
            .contains("postFilter = 0"));
        let backup = dir.join(format!("GameOption.txt{BACKUP_SUFFIX}"));
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), GAME_OPTION);

        // A second apply is a no-op and must not clobber the backup.
        let again = apply_files(&paths);
        assert_eq!(again[0].result.as_ref().copied().unwrap(), 0);
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), GAME_OPTION);

        let restored = restore_files(&paths);
        assert_eq!(restored[0].result.as_ref().copied().unwrap(), 1);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), GAME_OPTION);

        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Manual check against copies of real BDO files:
    /// set `BDO_CFG_TEST_ROOT` to a folder mirroring `Documents\Black Desert`
    /// and run `cargo test -p bdo-optimizer dry_run_real -- --ignored --nocapture`.
    #[test]
    #[ignore = "needs BDO_CFG_TEST_ROOT pointing at copied config files"]
    fn dry_run_real_copies() {
        let root = PathBuf::from(std::env::var("BDO_CFG_TEST_ROOT").expect("BDO_CFG_TEST_ROOT"));
        let files = find_config_files(&root);
        assert!(
            !files.is_empty(),
            "no config files under {}",
            root.display()
        );
        for outcome in apply_files(&files) {
            println!("{} -> {:?}", outcome.path.display(), outcome.result);
            outcome.result.expect("patch failed");
        }
        // Idempotence on the real data: a second apply changes nothing.
        for outcome in apply_files(&files) {
            assert_eq!(outcome.result.expect("second apply failed"), 0);
        }
    }

    #[test]
    fn find_config_files_walks_user_cache() {
        let root = std::env::temp_dir().join(format!("bdo-gameconfig-find-{}", std::process::id()));
        let account = root.join("UserCache").join("12345");
        std::fs::create_dir_all(&account).unwrap();
        std::fs::write(root.join("GameOption.txt"), "x").unwrap();
        std::fs::write(root.join("UserCache").join("gamevariable.xml"), "x").unwrap();
        std::fs::write(account.join("gamevariable.xml"), "x").unwrap();
        // A per-account dir without the file is skipped.
        std::fs::create_dir_all(root.join("UserCache").join("empty")).unwrap();

        let found = find_config_files(&root);
        assert_eq!(found.len(), 3);

        std::fs::remove_dir_all(root).unwrap();
    }
}
