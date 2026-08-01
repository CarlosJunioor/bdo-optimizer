//! Exclusively-owned temporary directories that other processes cannot write
//! into.
//!
//! Two features stage files that are later executed with this app's token: the
//! updater unpacks a release beside the running executable, and the NVIDIA step
//! runs Profile Inspector out of a scratch folder. Both used to build that
//! folder with `create_dir` and call it private. Winning `create_dir` only
//! proves nobody got there *first* — the temp folder belongs to the user, so a
//! medium-integrity process running as that same user can still write into the
//! directory afterwards: replace a staged executable, or plant a DLL the child
//! will resolve from its own directory before System32.
//!
//! A DACL cannot express the difference (same user, same SID). A **mandatory
//! integrity label** can: integrity is checked before the DACL, and a
//! medium-integrity process cannot write up to a high-integrity object. So the
//! directory is created exclusively *and* labelled.
//!
//! Labelling is only possible, and only necessary, when elevated. Raising a
//! label above your own integrity level needs `SeRelabelPrivilege`, so an
//! unelevated attempt fails with `ERROR_PRIVILEGE_NOT_HELD` — and unelevated
//! there is nothing to escalate to, because whatever is staged runs with
//! exactly the privileges the attacker already has. Elevated therefore fails
//! closed; unelevated skips the label.

use std::path::{Path, PathBuf};

/// Create a fresh directory under the temp folder that this process owns and,
/// when elevated, that lower-integrity processes cannot write into.
///
/// `prefix` distinguishes the caller in the directory name; it has no security
/// role (the name is not a secret, exclusivity comes from `create_dir`).
pub fn create(prefix: &str) -> Result<PathBuf, String> {
    let base = std::env::temp_dir();
    let pid = std::process::id();
    for attempt in 0..64u32 {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(attempt);
        let dir = base.join(format!("{prefix}-{pid}-{nanos}-{attempt}"));
        // `create_dir` is atomic and fails when the path exists, so the first
        // name that succeeds is one nobody else holds.
        match std::fs::create_dir(&dir) {
            Ok(()) => {
                if let Err(e) = protect(&dir) {
                    let _ = std::fs::remove_dir_all(&dir);
                    return Err(e);
                }
                return Ok(dir);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("could not create {}: {e}", dir.display())),
        }
    }
    Err(format!("could not create a private {prefix} directory"))
}

/// Label `dir` high-integrity when elevated. See the module docs for why this
/// is mandatory there and skipped otherwise.
fn protect(dir: &Path) -> Result<(), String> {
    if !bdo_launch::is_elevated() {
        return Ok(());
    }
    set_high_integrity(dir).map_err(|e| {
        format!(
            "a folder protected from other programs on this machine could not be created \
             ({e}), so this step was not run"
        )
    })
}

/// Apply a `High` mandatory label with no-write-up to `dir`.
fn set_high_integrity(dir: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{HLOCAL, LocalFree, ERROR_SUCCESS};
    use windows::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SetNamedSecurityInfoW,
        SDDL_REVISION_1, SE_FILE_OBJECT,
    };
    use windows::Win32::Security::{
        GetSecurityDescriptorSacl, ACL, LABEL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
    };

    // SDDL for a system-access-control-list holding one mandatory label:
    // no-write-up (NW) at the High integrity level (HI).
    let sddl: Vec<u16> = "S:(ML;;NW;;;HI)"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let mut descriptor = PSECURITY_DESCRIPTOR::default();

    // SAFETY: `sddl` is a NUL-terminated wide string alive across the call, and
    // `descriptor` is a valid out-parameter the callee allocates.
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(sddl.as_ptr()),
            SDDL_REVISION_1,
            &mut descriptor,
            None,
        )
        .map_err(|e| format!("could not build the integrity label: {e}"))?;
    }

    let mut sacl: *mut ACL = std::ptr::null_mut();
    let mut present = false.into();
    let mut defaulted = false.into();
    // SAFETY: `descriptor` came from the call above and is still owned here.
    let read =
        unsafe { GetSecurityDescriptorSacl(descriptor, &mut present, &mut sacl, &mut defaulted) };
    let result = match read {
        Ok(()) => {
            let path: Vec<u16> = dir
                .as_os_str()
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();
            // SAFETY: NUL-terminated path and a SACL borrowed from the
            // descriptor, both alive for the duration of the call.
            let status = unsafe {
                SetNamedSecurityInfoW(
                    PCWSTR(path.as_ptr()),
                    SE_FILE_OBJECT,
                    LABEL_SECURITY_INFORMATION,
                    None,
                    None,
                    None,
                    Some(sacl),
                )
            };
            if status == ERROR_SUCCESS {
                Ok(())
            } else {
                Err(format!("could not label the folder: {status:?}"))
            }
        }
        Err(e) => Err(format!("could not read the built label: {e}")),
    };

    // SAFETY: allocated by the conversion above, not used after this point.
    unsafe {
        let _ = LocalFree(Some(HLOCAL(descriptor.0)));
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_call_owns_a_distinct_directory() {
        let a = create("bdo-privdir-test").expect("a");
        let b = create("bdo-privdir-test").expect("b");
        assert_ne!(a, b);
        assert!(a.is_dir() && b.is_dir());
        // Exclusive creation: the same path must not be creatable again.
        assert!(std::fs::create_dir(&a).is_err());
        let _ = std::fs::remove_dir_all(a);
        let _ = std::fs::remove_dir_all(b);
    }
}
