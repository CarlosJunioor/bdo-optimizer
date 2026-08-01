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

/// One setting the guide asks for, in the spellings each store uses.
///
/// The same setting is written differently depending on the file: booleans are
/// `0`/`1` in `GameOption.txt` but `false`/`true` in the XML, and the key is
/// lower-camel in the text file and upper-camel in the XML. Both spellings are
/// carried here so neither store gets the other's format.
pub struct Tweak {
    /// Key in `GameOption.txt` (matched case-insensitively).
    pub txt_key: &'static str,
    /// Element or `Type=` name in `gamevariable.xml`.
    pub xml_key: &'static str,
    pub txt_value: &'static str,
    pub xml_value: &'static str,
}

/// The settings applied by the config-file tweak.
///
/// Deliberately limited to values the guide states plainly and that are plain
/// booleans or a documented 0/1. Numeric settings whose scale is not certain
/// (the effect-optimization slider, the anti-aliasing index) are **not** here:
/// writing a guessed number into a live config is how this feature would make
/// someone's game worse rather than better.
pub const TWEAKS: &[Tweak] = &[
    // "next to PostFilter where there's a 1 it should be a 0"
    Tweak {
        txt_key: "postFilter",
        xml_key: "PostFilter",
        txt_value: "0",
        xml_value: "0",
    },
    // "You can also disable Tessellation on High settings and above"
    Tweak {
        txt_key: "Tessellation",
        xml_key: "Tessellation",
        txt_value: "0",
        xml_value: "false",
    },
    // "I don't use Auto Frame Optimization... It will cancel out the settings below"
    Tweak {
        txt_key: "autoOptimization",
        xml_key: "AutoOptimization",
        txt_value: "0",
        xml_value: "false",
    },
    // "Disable low power mode. This one's a big deal."
    Tweak {
        txt_key: "sleepModeEnable",
        xml_key: "SleepModeEnable",
        txt_value: "0",
        xml_value: "false",
    },
    // "Combine this with Remove Faraway Effects to clean up unnecessary particles"
    Tweak {
        txt_key: "useNearestEffectOnly",
        xml_key: "UseNearestEffectOnly",
        txt_value: "1",
        xml_value: "true",
    },
    // "I don't use the Character Optimization setting."
    Tweak {
        txt_key: "useFarPlayerOptimization",
        xml_key: "useFarPlayerOptimization",
        txt_value: "0",
        xml_value: "false",
    },
];

/// Patch `GameOption.txt` text: every [`TWEAKS`] key gets its `txt_value`.
///
/// Only the value of a matched key changes; every other byte, including line
/// endings and unrelated lines, is preserved. A key absent from this file is
/// simply not applicable — it is not an error and is not counted.
pub fn patch_game_option(text: &str) -> (String, PatchStats) {
    patch_game_option_to(text, &|t, _| Some(t.txt_value.to_string()))
}

/// [`patch_game_option`] with the target value chosen per setting.
///
/// Restore uses this to write back the values a file *originally* had rather
/// than the guide's, which is what lets it undo six settings without touching
/// anything else in the file.
pub fn patch_game_option_to(
    text: &str,
    value_for: &dyn Fn(&Tweak, usize) -> Option<String>,
) -> (String, PatchStats) {
    let mut stats = PatchStats::default();
    let mut seen: Vec<(&str, usize)> = Vec::new();
    let mut out = String::with_capacity(text.len());
    for line in lines_inclusive(text) {
        out.push_str(&patch_option_line(line, value_for, &mut seen, &mut stats));
    }
    (out, stats)
}

fn patch_option_line(
    line: &str,
    value_for: &dyn Fn(&Tweak, usize) -> Option<String>,
    seen: &mut Vec<(&'static str, usize)>,
    stats: &mut PatchStats,
) -> String {
    let Some(eq) = line.find('=') else {
        return line.to_string();
    };
    let key = line[..eq].trim();
    let Some(tweak) = TWEAKS.iter().find(|t| key.eq_ignore_ascii_case(t.txt_key)) else {
        return line.to_string();
    };
    let value_part = &line[eq + 1..];
    let trimmed = value_part.trim_end_matches(['\r', '\n']);
    let (value, ending) = value_part.split_at(trimmed.len());
    let nth = match seen.iter_mut().find(|(key, _)| *key == tweak.txt_key) {
        Some((_, count)) => {
            *count += 1;
            *count
        }
        None => {
            seen.push((tweak.txt_key, 0));
            0
        }
    };
    // `None` means "leave this occurrence as it is" — restore uses it for an
    // occurrence the backup never had.
    let Some(target) = value_for(tweak, nth) else {
        stats.found += 1;
        return line.to_string();
    };
    stats.found += 1;
    if value.trim() == target {
        return line.to_string();
    }
    stats.changed += 1;
    format!("{}= {}{}", &line[..eq], target, ending)
}

/// Patch `gamevariable.xml` text: every [`TWEAKS`] key gets its `xml_value`.
///
/// These files store the same setting in two syntactically different shapes,
/// and which one a given setting uses is not predictable — several appear in
/// both. Every key is therefore tried against both:
///
/// * `<Key Value="…"/>`
/// * `<GameOption Type="Key" DataValue="…"/>`
///
/// Each shape occurs multiple times per file (the live values plus one copy per
/// in-game settings preset); the guide says to set them all.
pub fn patch_game_variable(text: &str) -> (String, PatchStats) {
    patch_game_variable_to(text, &|t, _| Some(t.xml_value.to_string()))
}

/// [`patch_game_variable`] with the target value chosen per setting.
pub fn patch_game_variable_to(
    text: &str,
    value_for: &dyn Fn(&Tweak, usize) -> Option<String>,
) -> (String, PatchStats) {
    let mut stats = PatchStats::default();
    let mut text = text.to_string();
    for tweak in TWEAKS {
        let element = format!("<{} Value=\"", tweak.xml_key);
        // One counter per key across both shapes, matching the order
        // `managed_values` reads them in — these files carry one copy per
        // in-game settings preset, each with its own value.
        let mut nth = 0;
        text = set_attr_values(
            &text,
            &element,
            &mut nth,
            &|i| value_for(tweak, i),
            &mut stats,
        );
        text = set_game_option_values(
            &text,
            tweak.xml_key,
            &mut nth,
            &|i| value_for(tweak, i),
            &mut stats,
        );
    }
    (text, stats)
}

/// The value each managed setting currently holds in `text`, if the parser can
/// see it. Used to read a file's original values out of its backup.
fn managed_values(text: &str, is_xml: bool) -> Vec<(&'static str, Vec<String>)> {
    let mut found = Vec::new();
    for tweak in TWEAKS {
        let mut values = Vec::new();
        if is_xml {
            // Both shapes, in the order `patch_game_variable_to` rewrites them,
            // so the nth value read here is the nth value written back.
            collect_attr_values(text, &format!("<{} Value=\"", tweak.xml_key), &mut values);
            collect_game_option_values(text, tweak.xml_key, &mut values);
        } else {
            for line in lines_inclusive(text) {
                let Some(eq) = line.find('=') else { continue };
                if line[..eq].trim().eq_ignore_ascii_case(tweak.txt_key) {
                    values.push(line[eq + 1..].trim().to_string());
                }
            }
        }
        if !values.is_empty() {
            found.push((tweak.txt_key, values));
        }
    }
    found
}

/// Every `<Key Value="…"/>` value in document order.
fn collect_attr_values(text: &str, tag_prefix: &str, out: &mut Vec<String>) {
    let mut rest = text;
    while let Some(found) = rest.find(tag_prefix) {
        rest = &rest[found + tag_prefix.len()..];
        match rest.find(['"', '<', '>']) {
            Some(end) if rest.as_bytes()[end] == b'"' => {
                out.push(rest[..end].to_string());
                rest = &rest[end..];
            }
            // Unterminated attribute: `set_attr_values` skips it too, so the
            // counts stay aligned.
            _ => continue,
        }
    }
}

/// Every `DataValue` inside a `<GameOption Type="key" …>` tag, in order.
fn collect_game_option_values(text: &str, key: &str, out: &mut Vec<String>) {
    let opener = format!("<GameOption Type=\"{key}\"");
    let mut rest = text;
    while let Some(found) = rest.find(&opener) {
        rest = &rest[found + opener.len()..];
        let tag_end = rest.find('>').unwrap_or(rest.len());
        let (tag, after) = rest.split_at(tag_end);
        const ATTR: &str = "DataValue=\"";
        if let Some(attr_at) = tag.find(ATTR) {
            let value_start = attr_at + ATTR.len();
            if let Some(close) = tag[value_start..].find('"') {
                out.push(tag[value_start..value_start + close].to_string());
            }
        }
        rest = after;
    }
}

/// Rewrite `DataValue` inside every `<GameOption Type="key" … />` tag.
///
/// The `DataValue` must be found *within the same tag* as the matched `Type`,
/// which is why the search is bounded by the tag's closing `>`. Some tags carry
/// a second discriminating attribute (`RenderPlayerColor` repeats with
/// different `ColorType`s), so the tag — not the `Type` — is the unit of work.
fn set_game_option_values(
    text: &str,
    key: &str,
    nth: &mut usize,
    value_for: &dyn Fn(usize) -> Option<String>,
    stats: &mut PatchStats,
) -> String {
    let opener = format!("<GameOption Type=\"{key}\"");
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(found) = rest.find(&opener) {
        let tag_start = found + opener.len();
        out.push_str(&rest[..tag_start]);
        rest = &rest[tag_start..];

        // Bound everything below to this tag.
        let tag_end = rest.find('>').unwrap_or(rest.len());
        let (tag, after) = rest.split_at(tag_end);

        const ATTR: &str = "DataValue=\"";
        let Some(attr_at) = tag.find(ATTR) else {
            // No DataValue in this tag — copy it untouched and keep scanning.
            out.push_str(tag);
            rest = after;
            continue;
        };
        let value_start = attr_at + ATTR.len();
        let Some(close) = tag[value_start..].find('"') else {
            out.push_str(tag);
            rest = after;
            continue;
        };
        let current = &tag[value_start..value_start + close];
        let Some(new_value) = value_for(*nth) else {
            *nth += 1;
            stats.found += 1;
            out.push_str(tag);
            rest = after;
            continue;
        };
        *nth += 1;
        if current != new_value {
            stats.changed += 1;
        }
        stats.found += 1;
        out.push_str(&tag[..value_start]);
        out.push_str(&new_value);
        out.push_str(&tag[value_start + close..]);
        rest = after;
    }
    out.push_str(rest);
    out
}

fn set_attr_values(
    text: &str,
    tag_prefix: &str,
    nth: &mut usize,
    value_for: &dyn Fn(usize) -> Option<String>,
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
                let Some(new_value) = value_for(*nth) else {
                    *nth += 1;
                    stats.found += 1;
                    out.push_str(&rest[..end]);
                    rest = &rest[end..];
                    continue;
                };
                *nth += 1;
                if rest[..end] != new_value {
                    stats.changed += 1;
                }
                stats.found += 1;
                out.push_str(&new_value);
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
    // Every operation below uses `path` — the *canonical* path returned here —
    // while the guard keeps its whole directory chain pinned. See `DirGuard`.
    let (_pinned, path) = pin_and_verify(root, path)?;
    let path = path.as_path();
    let bytes = std::fs::read(path).map_err(|e| format!("read failed: {e}"))?;
    let text = decode_latin1(&bytes);
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
    match std::fs::metadata(&backup) {
        // A usable backup already exists: never touch it.
        Ok(m) if m.is_file() && m.len() > 0 => {}
        // A zero-length leftover from a crashed write is worthless as a restore
        // point, so it gets replaced outright.
        Ok(_) => write_atomic_bytes(&backup, &bytes).map_err(|e| format!("backup failed: {e}"))?,
        // Nothing there: create, never replace. A backup that appears in the
        // gap is an *older* copy of the file and therefore the better restore
        // point, so losing that race must not overwrite it.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            write_new_only(&backup, &bytes).map_err(|e| format!("backup failed: {e}"))?
        }
        // A permission or transient error is *not* "missing". Treating it as
        // missing would fall through to a create that accepts a collision as
        // success, and the live file would then be modified with no verified
        // way back.
        Err(e) => return Err(format!("could not inspect the backup: {e}")),
    }
    // Whatever route got us here, refuse to touch the live file unless the
    // restore point can actually be *read back*. Metadata alone is not enough:
    // an ACL-restricted file, or an offline OneDrive placeholder whose contents
    // are not local, reports a healthy size and then fails at restore time —
    // exactly when it is needed and too late to matter.
    if !backup_is_readable(&backup) {
        return Err(format!(
            "the backup at {} could not be read back — refusing to modify the config",
            backup.display()
        ));
    }
    write_atomic_bytes(path, &encode_latin1(&patched)).map_err(|e| format!("write failed: {e}"))?;
    Ok(FileChange::Patched(stats.changed))
}

/// Copy each file's backup over the live file, recreating it if it is gone.
pub fn restore_files(root: &Path, paths: &[PathBuf], safe: &dyn Fn() -> bool) -> Vec<FileOutcome> {
    run_over(root, paths, safe, restore_one)
}

/// Undo this app's edits to one file.
///
/// # Why the whole backup is not simply copied back
///
/// The backup is a snapshot of the file the first time it was ever changed, and
/// these files hold far more than the six settings this app touches — graphics,
/// controls, UI layout, keybinds. Copying it over the live file would silently
/// throw away everything the user changed in the game since, which is not an
/// undo of *this app's* work but of theirs. Apply-play-undo is an ordinary
/// sequence, not an edge case.
///
/// So the backup is read for what the six managed settings *were*, and only
/// those values are written into the file as it stands today. Same principle as
/// the registry undo. The whole-file copy survives for the one case where it is
/// the right answer: the live file is gone and there is nothing to merge into.
fn restore_one(root: &Path, path: &Path) -> Result<FileChange, String> {
    let (_pinned, path) = pin_and_verify(root, path)?;
    let path = path.as_path();
    let backup = backup_path(path);
    let usable = std::fs::metadata(&backup).is_ok_and(|m| m.is_file() && m.len() > 0);
    if !usable {
        return Ok(FileChange::NoBackup);
    }
    let backup_bytes = std::fs::read(&backup).map_err(|e| format!("backup unreadable: {e}"))?;

    // The live file was deleted (by the game, or by hand): there is nothing to
    // merge into, so the snapshot *is* the best available restore. Only a
    // genuine `NotFound` counts — a permission error, a sharing violation or an
    // unhydrated cloud file would otherwise be read as "deleted" and answered
    // by overwriting a file that is still there, discarding everything changed
    // since the backup.
    let live_bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            write_atomic_bytes(path, &backup_bytes).map_err(|e| format!("restore failed: {e}"))?;
            return Ok(FileChange::Restored);
        }
        Err(e) => return Err(format!("could not read the current file: {e}")),
    };

    let is_xml = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("xml"));
    let original = decode_latin1(&backup_bytes);
    let originals = managed_values(&original, is_xml);
    // A setting the backup does not mention was not in the file before either,
    // so leaving the current value alone is the faithful undo.
    // Restore the nth occurrence to the nth original. These files hold one copy
    // of each setting per in-game settings preset, and those copies can differ,
    // so collapsing them to "the" value per key would flatten every preset onto
    // whichever one happened to come first.
    //
    // If the file has grown an occurrence the backup never had — a preset added
    // since — there is no original for it, and the value it currently holds is
    // left alone rather than guessed at from a different preset.
    let value_for = |tweak: &Tweak, nth: usize| {
        originals
            .iter()
            .find(|(key, _)| *key == tweak.txt_key)
            .and_then(|(_, values)| values.get(nth).cloned())
    };

    let live = decode_latin1(&live_bytes);
    let (restored, stats) = if is_xml {
        patch_game_variable_to(&live, &value_for)
    } else {
        patch_game_option_to(&live, &value_for)
    };
    if stats.found == 0 {
        return Ok(FileChange::NothingRecognized);
    }
    if stats.changed == 0 {
        return Ok(FileChange::Restored);
    }
    write_atomic_bytes(path, &encode_latin1(&restored))
        .map_err(|e| format!("restore failed: {e}"))?;
    Ok(FileChange::Restored)
}

/// Handles held open for the duration of one file operation.
///
/// Validating a pathname and then acting on that pathname is inherently racy:
/// any directory along it can be swapped for a junction in between, and every
/// later operation would follow the new target. Two things together close that:
///
/// 1. **Act on the canonical path.** [`pin_and_verify`] returns the resolved
///    path, so the caller never re-traverses the caller-supplied name.
/// 2. **Pin every directory on it.** Each component from the config root down
///    to the file's parent is opened and held with a share mode of
///    `FILE_SHARE_READ` alone — no write, no delete — so while the operation
///    runs none of them can be renamed, deleted, or have a reparse point set on
///    it. Each is also opened *without* following reparse points and rejected
///    if it is one, so the chain contains no link that could be retargeted.
#[must_use = "dropping the guard releases the directory pins"]
struct DirGuard(#[allow(dead_code)] Vec<std::fs::File>);

/// Pin the directory chain and return the canonical path to operate on.
fn pin_and_verify(root: &Path, path: &Path) -> Result<(DirGuard, PathBuf), String> {
    let root = root
        .canonicalize()
        .map_err(|e| format!("config folder unavailable: {e}"))?;
    let parent = path
        .parent()
        .ok_or_else(|| "path has no parent directory".to_string())?
        .canonicalize()
        .map_err(|e| format!("containing folder unavailable: {e}"))?;
    if !parent.starts_with(&root) {
        return Err("resolves outside the BDO config folder — refusing to touch it".to_string());
    }
    let name = path
        .file_name()
        .ok_or_else(|| "path has no file name".to_string())?;
    // The file itself may not exist yet (restore recreating it), so the target
    // is built from the canonical parent rather than canonicalized directly.
    let target = parent.join(name);
    // The parent chain being clean is not enough: the leaf can be a link of its
    // own, and reads/writes on the pathname would follow it straight out of the
    // root. The backup is checked too, since restore reads through it.
    reject_leaf_link(&target)?;
    reject_leaf_link(&backup_path(&target))?;

    let mut handles = Vec::new();
    let mut current = root.clone();
    handles.push(pin_dir(&current)?);
    for component in parent
        .strip_prefix(&root)
        .map_err(|_| "containing folder is not under the config folder".to_string())?
        .components()
    {
        current = current.join(component);
        handles.push(pin_dir(&current)?);
    }
    Ok((DirGuard(handles), target))
}

/// Whether an open handle refers to a *name-surrogate* reparse point — a
/// symlink or junction, i.e. one that redirects a path.
///
/// Classifying from the handle (rather than re-inspecting the path) is what
/// makes the check atomic with the open. The name-surrogate bit is what keeps
/// OneDrive Files On-Demand folders working: those are reparse points too, but
/// not surrogates, and rejecting them would lock out everyone whose Documents
/// folder lives in OneDrive.
#[cfg(windows)]
fn is_name_surrogate(handle: &std::fs::File) -> std::io::Result<bool> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        FileAttributeTagInfo, GetFileInformationByHandleEx, FILE_ATTRIBUTE_TAG_INFO,
    };

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    /// `IsReparseTagNameSurrogate`: the tag names another entity.
    const REPARSE_TAG_NAME_SURROGATE: u32 = 0x2000_0000;

    let mut info = FILE_ATTRIBUTE_TAG_INFO::default();
    // SAFETY: `info` is a valid, correctly-sized FILE_ATTRIBUTE_TAG_INFO and the
    // handle is live for the call.
    unsafe {
        GetFileInformationByHandleEx(
            HANDLE(handle.as_raw_handle() as _),
            FileAttributeTagInfo,
            std::ptr::from_mut(&mut info).cast(),
            std::mem::size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32,
        )
    }
    .map_err(std::io::Error::other)?;

    Ok(info.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
        && info.ReparseTag & REPARSE_TAG_NAME_SURROGATE != 0)
}

/// Refuse a leaf that is itself a symlink, so no read or write follows it out
/// of the pinned directory. A file that does not exist is fine.
///
/// ponytail: this is a check, not a pin — the leaf is not held open, because
/// holding it would block the very replace that publishes the new contents. The
/// residual window is narrow: the containing directory *is* pinned for the whole
/// operation, and creating a *file* symlink inside it requires either
/// administrator rights or Developer Mode, so an ordinary same-user process
/// cannot swap one in. Pin the leaf through a handle-relative open if that ever
/// stops being true.
fn reject_leaf_link(path: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Err(format!(
            "{} is a link — refusing to read or write through it",
            path.display()
        )),
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("could not inspect {}: {e}", path.display())),
    }
}

/// Open one directory so it cannot be renamed, deleted, or relinked, rejecting
/// it outright if it is a link.
fn pin_dir(dir: &Path) -> Result<std::fs::File, String> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_READ_ATTRIBUTES: u32 = 0x0080;
        const FILE_SHARE_READ: u32 = 0x0000_0001;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

        // Open first, classify from the handle second. Inspecting the path and
        // then opening it would be two separate lookups, and a link swapped in
        // between the two would be followed — pinning the wrong directory.
        // `FILE_FLAG_OPEN_REPARSE_POINT` means this open never follows one.
        let handle = std::fs::OpenOptions::new()
            .access_mode(FILE_READ_ATTRIBUTES)
            // Read sharing only: denying write and delete sharing is what stops
            // a rename, a delete, or a reparse point being set while we work.
            .share_mode(FILE_SHARE_READ)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(dir)
            .map_err(|e| format!("could not pin {}: {e}", dir.display()))?;

        let surrogate = match is_name_surrogate(&handle) {
            Ok(answer) => answer,
            // A filesystem that does not implement the query (some remote or
            // third-party ones) still needs an answer. Falling back to the
            // path-based check is weaker — it is a second lookup — but a hard
            // failure would make the feature unusable there for no safety gain.
            Err(_) => std::fs::symlink_metadata(dir)
                .map_err(|e| format!("could not inspect {}: {e}", dir.display()))?
                .file_type()
                .is_symlink(),
        };
        if surrogate {
            return Err(format!(
                "{} is a link — refusing to write through it",
                dir.display()
            ));
        }
        Ok(handle)
    }
    #[cfg(not(windows))]
    std::fs::File::open(dir).map_err(|e| format!("could not open {}: {e}", dir.display()))
}

/// Write via a temp sibling + atomic replace so the destination is never
/// observed half-written: readers see either the old bytes or the new ones.
/// Read a config file as Latin-1 rather than UTF-8.
///
/// BDO writes anything the player typed — skill memos above all — in the OS
/// legacy codepage, so a config file is frequently *not* valid UTF-8: a
/// Portuguese "Espaço" lands as a lone `0xE7`. Rejecting those files meant
/// refusing to optimize on any non-English install.
///
/// Latin-1 is the one encoding that maps every possible byte to a distinct char
/// and back, so this is a total, lossless view of arbitrary bytes — and since
/// everything this module matches on is pure ASCII, guessing the real codepage
/// wrong cannot change which values get patched. [`encode_latin1`] reverses it,
/// so untouched text is written back byte-for-byte whatever it originally was.
fn decode_latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| b as char).collect()
}

/// Reverse [`decode_latin1`].
///
/// ponytail: no UTF-16 support and no encoding detection. Every BDO config seen
/// so far is single-byte; a UTF-16 one would simply not match the ASCII patterns
/// and be reported as unrecognized rather than corrupted.
fn encode_latin1(text: &str) -> Vec<u8> {
    // Every char here came from `decode_latin1` or from this module's own ASCII
    // replacements, so all of them fit in a single byte.
    text.chars()
        .map(|c| u8::try_from(c as u32).unwrap_or(b'?'))
        .collect()
}

pub(crate) fn write_atomic_bytes(path: &Path, contents: &[u8]) -> std::io::Result<()> {
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

/// Whether the backup is genuinely usable: present, non-empty, and readable.
///
/// Reads a byte rather than trusting `metadata`, so a file whose bytes are not
/// actually retrievable cannot pass as a restore point.
fn backup_is_readable(backup: &Path) -> bool {
    use std::io::Read;

    let Ok(meta) = std::fs::metadata(backup) else {
        return false;
    };
    if !meta.is_file() || meta.len() == 0 {
        return false;
    }
    let Ok(mut file) = std::fs::File::open(backup) else {
        return false;
    };
    let mut byte = [0u8; 1];
    matches!(file.read(&mut byte), Ok(1))
}

/// Write `contents` to `path` only if nothing is there, leaving any existing
/// file untouched. Used for the one-time backup, where an existing copy is
/// always the more valuable one.
pub(crate) fn write_new_only(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write;

    let (mut file, tmp) = create_temp_sibling(path)?;
    let write = file.write_all(contents).and_then(|()| file.sync_all());
    drop(file);
    if let Err(e) = write {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    #[cfg(windows)]
    let moved = move_without_replacing(&tmp, path).map_err(|r| r.error);
    #[cfg(not(windows))]
    let moved = std::fs::hard_link(&tmp, path).and_then(|()| std::fs::remove_file(&tmp));
    match moved {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            // Losing the race is success: a backup already exists.
            if e.kind() == std::io::ErrorKind::Interrupted
                || e.kind() == std::io::ErrorKind::AlreadyExists
            {
                return Ok(());
            }
            Err(e)
        }
    }
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
/// the destination does not exist yet (restore recreating a deleted file),
/// where there is no ACL to preserve and nothing can be lost.
///
/// Every other failure is reported instead of being papered over with a rename.
fn replace_file(tmp: &Path, dest: &Path) -> Result<(), Replace> {
    #[cfg(windows)]
    {
        // The two Windows strategies hand off to each other when the
        // destination appears or disappears between calls. A pathological
        // create/delete race could ping-pong forever, so the handoff is a
        // bounded loop rather than mutual recursion.
        const MAX_ATTEMPTS: u32 = 8;
        let mut replace_existing = dest.exists();
        for _ in 0..MAX_ATTEMPTS {
            let outcome = if replace_existing {
                replace_file_windows(tmp, dest)
            } else {
                move_without_replacing(tmp, dest)
            };
            match outcome {
                Err(Replace {
                    consumed: false,
                    error,
                }) if error.kind() == std::io::ErrorKind::Interrupted => {
                    // `Interrupted` is the private signal meaning "the other
                    // strategy is the right one now"; flip and retry.
                    replace_existing = !replace_existing;
                }
                other => return other,
            }
        }
        Err(Replace {
            error: std::io::Error::other(format!(
                "could not settle a replacement for {} — the file kept appearing and \
                 disappearing",
                dest.display()
            )),
            consumed: false,
        })
    }
    #[allow(unreachable_code)]
    #[cfg(not(windows))]
    std::fs::rename(tmp, dest).map_err(|error| Replace {
        error,
        consumed: false,
    })
}

/// Move `tmp` to `dest` **without** overwriting anything that appeared there.
///
/// `std::fs::rename` always replaces the destination, so using it after a
/// "destination missing" result would silently clobber — and strip the ACL
/// from — a file created in the gap between the two calls. `MoveFileExW`
/// without `MOVEFILE_REPLACE_EXISTING` fails instead, and that case loops back
/// to the replace path where the ACL is preserved.
#[cfg(windows)]
fn move_without_replacing(tmp: &Path, dest: &Path) -> Result<(), Replace> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, ERROR_FILE_EXISTS};
    use windows::Win32::Storage::FileSystem::{MoveFileExW, MOVE_FILE_FLAGS};

    let wide = |p: &Path| -> Vec<u16> {
        p.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    };
    let tmp_w = wide(tmp);
    let dest_w = wide(dest);
    // SAFETY: both buffers are NUL-terminated and outlive the call.
    let moved = unsafe {
        MoveFileExW(
            PCWSTR(tmp_w.as_ptr()),
            PCWSTR(dest_w.as_ptr()),
            MOVE_FILE_FLAGS(0),
        )
    };
    match moved {
        Ok(()) => Ok(()),
        Err(e) => {
            let code = win32_code(&e);
            if code == Some(ERROR_ALREADY_EXISTS.0) || code == Some(ERROR_FILE_EXISTS.0) {
                // Someone created the destination in the gap; ask the caller to
                // retry through the ACL-preserving path rather than clobbering
                // it. See `replace_file` for why this is a signal, not a call.
                return Err(Replace {
                    error: std::io::Error::from(std::io::ErrorKind::Interrupted),
                    consumed: false,
                });
            }
            Err(Replace {
                error: std::io::Error::other(format!(
                    "could not move into {}: {e}",
                    dest.display()
                )),
                consumed: false,
            })
        }
    }
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
    use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND};
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
            // Only a missing destination falls back to rename: there is no ACL
            // to preserve, so nothing is lost. A filesystem that cannot do
            // ReplaceFileW at all used to fall back too, but that quietly did
            // the very ACL substitution this function exists to avoid — it is
            // now reported instead.
            let missing_dest =
                code == Some(ERROR_FILE_NOT_FOUND.0) || code == Some(ERROR_PATH_NOT_FOUND.0);
            if missing_dest {
                // Destination vanished; retry as a plain non-replacing move.
                return Err(Replace {
                    error: std::io::Error::from(std::io::ErrorKind::Interrupted),
                    consumed: false,
                });
            }
            // Only these three leave the two names in a half-moved state, so
            // only these make the temp file something other than disposable
            // scratch. Marking *every* failure as consumed would leak a .tmp
            // sibling after ordinary failures like a sharing violation, which
            // leave both names untouched.
            // 1175 (UNABLE_TO_REMOVE_REPLACED) is deliberately *not* here:
            // Windows guarantees both original names still exist in that case,
            // so the temp file is still disposable and must be cleaned up.
            const ERROR_UNABLE_TO_MOVE_REPLACEMENT: u32 = 1176;
            const ERROR_UNABLE_TO_MOVE_REPLACEMENT_2: u32 = 1177;
            let consumed = matches!(
                code,
                Some(ERROR_UNABLE_TO_MOVE_REPLACEMENT) | Some(ERROR_UNABLE_TO_MOVE_REPLACEMENT_2)
            );
            let detail = if consumed {
                format!(" (a replacement may remain at {})", tmp.display())
            } else {
                String::new()
            };
            Err(Replace {
                error: std::io::Error::other(format!(
                    "could not replace {}: {e}{detail}",
                    dest.display()
                )),
                consumed,
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
    fn game_option_type_shape_is_patched_within_its_own_tag() {
        // Shape A: several settings live only here, never as <Key Value="…"/>.
        let text = "<GameOption Type=\"AutoOptimization\" DataValue=\"true\"/>\n\
                    <GameOption Type=\"useFarPlayerOptimization\" DataValue=\"true\"/>\n";
        let (patched, stats) = patch_game_variable(text);
        assert_eq!(stats.changed, 2);
        assert!(patched.contains("<GameOption Type=\"AutoOptimization\" DataValue=\"false\"/>"));
        assert!(
            patched.contains("<GameOption Type=\"useFarPlayerOptimization\" DataValue=\"false\"/>")
        );
    }

    #[test]
    fn a_tag_without_datavalue_is_left_alone() {
        // The DataValue search must not run past this tag into the next one.
        let text = "<GameOption Type=\"AutoOptimization\"/>\n<Other DataValue=\"keep\"/>\n";
        let (patched, stats) = patch_game_variable(text);
        assert_eq!(patched, text);
        assert_eq!(stats, PatchStats::default());
    }

    #[test]
    fn a_repeated_type_with_a_discriminator_is_not_confused() {
        // RenderPlayerColor repeats with different ColorType values; each tag is
        // its own unit of work, so a matcher must not spill across them.
        let text = "<GameOption Type=\"SleepModeEnable\" DataValue=\"true\"/>\n\
                    <GameOption Type=\"RenderPlayerColor\" ColorType=\"1\" DataValue=\"7\"/>\n";
        let (patched, stats) = patch_game_variable(text);
        assert_eq!(stats.changed, 1);
        assert!(patched.contains("<GameOption Type=\"SleepModeEnable\" DataValue=\"false\"/>"));
        assert!(
            patched.contains("ColorType=\"1\" DataValue=\"7\""),
            "{patched}"
        );
    }

    #[test]
    fn every_tweak_is_idempotent_across_both_shapes() {
        let mut doc = String::new();
        for t in TWEAKS {
            doc.push_str(&format!("<{} Value=\"zzz\"/>\n", t.xml_key));
            doc.push_str(&format!(
                "<GameOption Type=\"{}\" DataValue=\"zzz\"/>\n",
                t.xml_key
            ));
        }
        let (once, first) = patch_game_variable(&doc);
        assert_eq!(first.changed, TWEAKS.len() * 2);
        let (twice, second) = patch_game_variable(&once);
        assert_eq!(second.changed, 0);
        assert_eq!(twice, once);
    }

    #[test]
    fn text_store_uses_numeric_booleans_not_xml_ones() {
        // Regression: the .txt store must never receive true/false.
        let text = "sleepModeEnable = 1\r\nuseNearestEffectOnly = 0\r\n";
        let (patched, stats) = patch_game_option(text);
        assert_eq!(stats.changed, 2);
        assert_eq!(
            patched,
            "sleepModeEnable = 0\r\nuseNearestEffectOnly = 1\r\n"
        );
        assert!(!patched.contains("false") && !patched.contains("true"));
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
    fn write_new_only_never_overwrites_an_existing_backup() {
        let dir = temp_dir("newonly");
        let target = dir.join("GameOption.txt.bak");
        std::fs::write(&target, "the older, better copy").unwrap();

        write_new_only(&target, b"should not land").unwrap();
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "the older, better copy"
        );
        // And it must not leave scratch files behind when it declines.
        let leftovers = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .count();
        assert_eq!(leftovers, 0);

        // With nothing there, it writes.
        let fresh = dir.join("fresh.bak");
        write_new_only(&fresh, b"written").unwrap();
        assert_eq!(std::fs::read_to_string(&fresh).unwrap(), "written");

        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Undo must reverse *this app's* six settings, not everything the user
    /// did since. The backup is a whole-file snapshot from the first apply, so
    /// copying it back wholesale would silently discard graphics, keybind and
    /// UI changes made after — apply, play, undo is an ordinary sequence.
    #[test]
    fn restore_puts_back_our_settings_and_keeps_later_edits() {
        let dir = temp_dir("merge-restore");
        let file = dir.join("GameOption.txt");
        std::fs::write(
            &file,
            "Resolution = 1920x1080\npostFilter = 1\nTessellation = 1\nMasterVolume = 50\n",
        )
        .unwrap();

        let out = apply_files(&dir, std::slice::from_ref(&file), always_safe());
        assert!(out[0].result.is_ok(), "{:?}", out[0].result);

        // The user then changes unrelated settings in-game.
        let patched = std::fs::read_to_string(&file).unwrap();
        let edited = patched
            .replace("Resolution = 1920x1080", "Resolution = 2560x1440")
            .replace("MasterVolume = 50", "MasterVolume = 80");
        std::fs::write(&file, &edited).unwrap();

        restore_files(&dir, std::slice::from_ref(&file), always_safe());
        let restored = std::fs::read_to_string(&file).unwrap();

        assert!(
            restored.contains("postFilter = 1") && restored.contains("Tessellation = 1"),
            "our settings must be reverted to their originals: {restored}"
        );
        assert!(
            restored.contains("Resolution = 2560x1440") && restored.contains("MasterVolume = 80"),
            "changes made after the apply must survive the undo: {restored}"
        );

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn legacy_codepage_bytes_are_patched_and_preserved() {
        // A real gamevariable.xml from a Portuguese install: the skill memo
        // "Espaço" is CP1252, so the file is not valid UTF-8. It must still be
        // patched, and every byte outside the patch must survive untouched.
        let dir = temp_dir("cp1252");
        let file = dir.join("gamevariable.xml");
        let mut original = Vec::new();
        original.extend_from_slice(b"<SkillCoolTimeMemo Memo=\"Espa");
        original.push(0xE7); // 'ç' in CP1252 — a lone continuation byte in UTF-8
        original.extend_from_slice(b"o\"/>\r\n");
        original.extend_from_slice(GAME_VARIABLE.as_bytes());
        std::fs::write(&file, &original).unwrap();

        let out = apply_files(&dir, std::slice::from_ref(&file), always_safe());
        let change = out[0]
            .result
            .as_ref()
            .expect("non-UTF-8 must not be refused");
        assert!(
            matches!(change, FileChange::Patched(n) if *n > 0),
            "{change:?}"
        );

        // The backup is the original bytes, exactly.
        let backup = dir.join(format!("gamevariable.xml{BACKUP_SUFFIX}"));
        assert_eq!(std::fs::read(&backup).unwrap(), original);

        // The memo survives the patch byte-for-byte; only the guide values moved.
        let patched = std::fs::read(&file).unwrap();
        assert!(
            patched.starts_with(b"<SkillCoolTimeMemo Memo=\"Espa\xE7o\"/>\r\n"),
            "the legacy-codepage memo was rewritten"
        );

        // And restore puts the original back, byte for byte.
        restore_files(&dir, std::slice::from_ref(&file), always_safe());
        assert_eq!(std::fs::read(&file).unwrap(), original);

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_directory_where_the_backup_belongs_blocks_the_patch() {
        // Stands in for any case where the backup cannot be established: the
        // live file must be left alone rather than modified with no way back.
        let dir = temp_dir("nobackuppossible");
        let file = dir.join("GameOption.txt");
        std::fs::write(&file, GAME_OPTION).unwrap();
        // A directory at the backup's exact path cannot be written as a file.
        std::fs::create_dir(dir.join(format!("GameOption.txt{BACKUP_SUFFIX}"))).unwrap();

        let out = apply_files(&dir, std::slice::from_ref(&file), always_safe());
        assert!(out[0].result.is_err(), "{:?}", out[0].result);
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            GAME_OPTION,
            "the live file must be untouched when no backup could be made"
        );

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_unreadable_backup_does_not_pass_as_a_restore_point() {
        let dir = temp_dir("unreadablebak");
        let backup = dir.join("x.bak");
        // Present and non-empty by metadata, but a directory cannot be read as
        // a file — standing in for any backup whose bytes are not retrievable.
        std::fs::write(&backup, "content").unwrap();
        assert!(backup_is_readable(&backup));

        std::fs::remove_file(&backup).unwrap();
        std::fs::create_dir(&backup).unwrap();
        assert!(!backup_is_readable(&backup));

        let empty = dir.join("empty.bak");
        std::fs::write(&empty, "").unwrap();
        assert!(!backup_is_readable(&empty));

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
    fn pin_returns_the_canonical_target_not_the_supplied_name() {
        let dir = temp_dir("canon");
        let nested = dir.join("UserCache");
        std::fs::create_dir_all(&nested).unwrap();
        let file = nested.join("gamevariable.xml");
        std::fs::write(&file, "<x/>").unwrap();

        // A path with a redundant traversal must come back fully resolved, so
        // later operations never re-walk the caller's spelling of it.
        let noisy = nested.join("..").join("UserCache").join("gamevariable.xml");
        let Ok((_guard, target)) = pin_and_verify(&dir, &noisy) else {
            panic!("pin_and_verify rejected a legitimate path");
        };
        assert_eq!(target, file.canonicalize().unwrap());

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn junctions_resolve_inside_the_root_and_are_refused_outside_it() {
        // Creating a junction needs no privileges, unlike a file symlink.
        fn junction(link: &Path, target: &Path) -> bool {
            std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(link)
                .arg(target)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        }

        let root = temp_dir("junction");
        let real = root.join("real");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(real.join("gamevariable.xml"), "<x/>").unwrap();
        let outside = temp_dir("junction-out");
        std::fs::write(outside.join("gamevariable.xml"), "<x/>").unwrap();

        let inside_link = root.join("inside");
        let escape_link = root.join("escape");
        if !junction(&inside_link, &real) || !junction(&escape_link, &outside) {
            let _ = std::fs::remove_dir_all(&root);
            let _ = std::fs::remove_dir_all(&outside);
            return; // junctions unavailable here; nothing to assert
        }

        // Pointing back inside the root: allowed, but resolved to the real
        // directory so nothing is written through the link itself.
        match pin_and_verify(&root, &inside_link.join("gamevariable.xml")) {
            Ok((_g, target)) => assert_eq!(
                target,
                real.join("gamevariable.xml").canonicalize().unwrap()
            ),
            Err(e) => panic!("a junction inside the root was refused: {e}"),
        }

        // Pointing out of the root: refused.
        match pin_and_verify(&root, &escape_link.join("gamevariable.xml")) {
            Err(e) => assert!(e.contains("outside"), "unexpected error: {e}"),
            Ok(_) => panic!("a junction escaping the root was accepted"),
        }

        for link in [&inside_link, &escape_link] {
            let _ = std::process::Command::new("cmd")
                .args(["/C", "rmdir"])
                .arg(link)
                .output();
        }
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(outside).unwrap();
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
