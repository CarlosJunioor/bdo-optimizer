//! PresentMon executable path resolution.
//!
//! We look for `PresentMon.exe` in two layouts, in priority order:
//!
//! 1. **Installed / release layout** — next to the running executable, i.e. the
//!    bundled copy shipped alongside `bdo-optimizer.exe`.
//! 2. **Dev layout** — `vendor/presentmon/PresentMon.exe` at the workspace root,
//!    found by walking the ancestors of the running executable and the current
//!    working directory (so `cargo run`, which places the binary under
//!    `target/debug/`, still finds the checked-in vendor copy).
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
/// copy first, then `vendor/presentmon/PresentMon.exe` under each ancestor of
/// the exe dir, then under each ancestor of the working dir.
pub fn candidate_paths(exe_dir: Option<&Path>, cwd: Option<&Path>) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();

    if let Some(dir) = exe_dir {
        out.push(dir.join(PRESENTMON_EXE));
    }

    let vendor_rel = Path::new("vendor").join("presentmon").join(PRESENTMON_EXE);

    for root in [exe_dir, cwd].into_iter().flatten() {
        for ancestor in root.ancestors() {
            let candidate = ancestor.join(&vendor_rel);
            if !out.contains(&candidate) {
                out.push(candidate);
            }
        }
    }

    out
}

/// Return the first candidate that satisfies `exists`.
///
/// Split from the filesystem so tests can supply a fake predicate.
pub fn first_existing(candidates: &[PathBuf], exists: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    candidates.iter().find(|p| exists(p)).cloned()
}

/// Resolve the bundled `PresentMon.exe`, or `None` if it cannot be found.
///
/// Uses [`std::env::current_exe`] and [`std::env::current_dir`] to seed the
/// candidate list, then returns the first path that exists on disk.
pub fn resolve() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok();
    let exe_dir = exe.as_deref().and_then(Path::parent);
    let cwd = std::env::current_dir().ok();
    let candidates = candidate_paths(exe_dir, cwd.as_deref());
    first_existing(&candidates, |p| p.exists())
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
    fn dev_layout_found_via_ancestor() {
        // Binary under target/debug; vendor dir at the repo root two levels up.
        let exe_dir = Path::new("/repo/target/debug");
        let cands = candidate_paths(Some(exe_dir), None);
        let expected = PathBuf::from("/repo")
            .join("vendor")
            .join("presentmon")
            .join(PRESENTMON_EXE);
        assert!(
            cands.contains(&expected),
            "candidates {cands:?} should include {expected:?}"
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
