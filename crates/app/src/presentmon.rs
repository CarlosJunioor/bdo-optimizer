//! PresentMon executable path resolution.
//!
//! We look for `PresentMon.exe` in two layouts, in priority order:
//!
//! 1. **Installed / release layout** — the hash-pinned bundled copy next to the
//!    running `bdo-optimizer.exe`.
//! 2. **Dev layout** — `vendor/presentmon/PresentMon.exe` at the workspace root,
//!    resolved from the compile-time manifest directory in debug builds.
//!
//! If neither exists, resolution returns `None` and the UI reports the two
//! expected locations.

use std::path::{Path, PathBuf};

/// The bundled PresentMon executable file name.
pub const PRESENTMON_EXE: &str = "PresentMon.exe";

/// Build the ordered list of candidate `PresentMon.exe` paths for the given
/// executable directory and current working directory.
///
/// Pure: performs no filesystem access, so it can be unit-tested with synthetic
/// directories. Candidate order matches the documented priority: the exe-dir
/// copy first, then the compile-time workspace path in debug builds.
pub fn candidate_paths(exe_dir: Option<&Path>, _cwd: Option<&Path>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(dir) = exe_dir {
        out.push(dir.join(PRESENTMON_EXE));
    }
    #[cfg(debug_assertions)]
    out.push(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../vendor/presentmon")
            .join(PRESENTMON_EXE),
    );
    out
}
/// Return the first candidate that satisfies `exists`.
///
/// Split from the filesystem so tests can supply a fake predicate.
pub fn first_existing(candidates: &[PathBuf], exists: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    candidates.iter().find(|p| exists(p)).cloned()
}

fn is_trusted_presentmon(path: &Path) -> bool {
    bdo_bench::is_supported_presentmon(path)
}

/// Resolve the bundled `PresentMon.exe`, or `None` if it cannot be found.
///
/// Uses [`std::env::current_exe`] and [`std::env::current_dir`] to seed the
/// candidate list, then returns the first path that exists on disk.
pub fn resolve() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok();
    let exe_dir = exe.as_deref().and_then(Path::parent);
    let candidates = candidate_paths(exe_dir, None);
    first_existing(&candidates, is_trusted_presentmon).and_then(|path| path.canonicalize().ok())
}

/// The two human-readable locations shown to the user when resolution fails.
pub fn expected_locations() -> [String; 2] {
    [
        format!("next to the app executable ({PRESENTMON_EXE})"),
        format!("vendor/presentmon/{PRESENTMON_EXE} under the project root"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exe_dir_copy_is_first_candidate() {
        let exe_dir = Path::new("/opt/bdo");
        let cands = candidate_paths(Some(exe_dir), None);
        assert_eq!(cands[0], PathBuf::from("/opt/bdo").join(PRESENTMON_EXE));
    }

    #[test]
    fn untrusted_ancestor_is_not_a_candidate() {
        let cands = candidate_paths(
            Some(Path::new("/untrusted/target/debug")),
            Some(Path::new("/untrusted")),
        );
        assert!(
            !cands.contains(&PathBuf::from("/untrusted/vendor/presentmon").join(PRESENTMON_EXE))
        );
    }
    #[test]
    fn first_existing_prefers_earlier_candidate() {
        let a = PathBuf::from("/a/PresentMon.exe");
        let b = PathBuf::from("/repo/vendor/presentmon/PresentMon.exe");
        let cands = vec![a.clone(), b.clone()];
        // Both "exist": the earlier one (exe-dir copy) must win.
        let got = first_existing(&cands, |_| true).unwrap();
        assert_eq!(got, a);
    }

    #[test]
    fn first_existing_skips_missing() {
        let a = PathBuf::from("/a/PresentMon.exe");
        let b = PathBuf::from("/repo/vendor/presentmon/PresentMon.exe");
        let cands = vec![a.clone(), b.clone()];
        // Only the vendor copy exists.
        let got = first_existing(&cands, |p| p == b).unwrap();
        assert_eq!(got, b);
    }

    #[test]
    fn none_when_nothing_exists() {
        let cands = candidate_paths(Some(Path::new("/x/bin")), None);
        assert!(first_existing(&cands, |_| false).is_none());
    }

    #[test]
    fn bundled_presentmon_hash_rejects_modified_copy() {
        let bundled = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../vendor/presentmon")
            .join(PRESENTMON_EXE);
        assert!(is_trusted_presentmon(&bundled));

        let copy = std::env::temp_dir().join(format!(
            "bdo-presentmon-hash-test-{}.exe",
            std::process::id()
        ));
        let mut bytes = std::fs::read(&bundled).unwrap();
        bytes[0] ^= 1;
        std::fs::write(&copy, bytes).unwrap();
        assert!(!is_trusted_presentmon(&copy));
        std::fs::remove_file(copy).unwrap();
    }

    #[test]
    fn no_duplicate_candidates() {
        // When exe_dir and cwd share ancestors, candidates must not repeat.
        let cands = candidate_paths(
            Some(Path::new("/repo/target/debug")),
            Some(Path::new("/repo")),
        );
        let mut sorted = cands.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            cands.len(),
            "duplicate candidate paths: {cands:?}"
        );
    }
}
