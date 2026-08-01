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
        // `CreateDirectoryW` is atomic and fails when the path exists, so the
        // first name that succeeds is one nobody else holds — *and* the label
        // is part of the same call, so the directory is never briefly writable
        // under a name someone else can see.
        match create_labelled(&dir) {
            Ok(()) => return Ok(dir),
            Err(Create::Exists) => continue,
            Err(Create::Failed(e)) => return Err(e),
        }
    }
    Err(format!("could not create a private {prefix} directory"))
}

/// A named mutex that spans Windows sessions where possible.
///
/// The state these locks protect — a portable install folder, the driver
/// profile — is machine-wide, but a `Local\` mutex is per logon session, so
/// under Fast User Switching or RDP two copies would each hold "the" lock and
/// race anyway. `Global\` fixes that, at the cost of needing
/// `SeCreateGlobalPrivilege`, which standard users do not hold. Falling back
/// keeps an unelevated app working, while the elevated case — the one that can
/// corrupt a shared install — always gets the cross-session object.
pub fn cross_session_mutex(name: &str) -> Result<windows::Win32::Foundation::HANDLE, String> {
    use windows::core::PCWSTR;
    use windows::Win32::System::Threading::CreateMutexW;

    for scope in [r"Global\", r"Local\"] {
        let wide: Vec<u16> = format!("{scope}{name}")
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        // SAFETY: `wide` is a NUL-terminated string alive across the call.
        if let Ok(handle) = unsafe { CreateMutexW(None, false, PCWSTR(wide.as_ptr())) } {
            return Ok(handle);
        }
    }
    Err("could not create the lock".to_string())
}

/// A held named mutex, released on drop.
pub struct MutexGuard(windows::Win32::Foundation::HANDLE);

impl Drop for MutexGuard {
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

/// Take `name` cross-session, waiting up to 30 seconds for another instance.
pub fn lock(name: &str) -> Result<MutexGuard, String> {
    use windows::Win32::Foundation::WAIT_TIMEOUT;
    use windows::Win32::System::Threading::WaitForSingleObject;

    let handle = cross_session_mutex(name)?;
    // SAFETY: a live mutex handle from the call above.
    if unsafe { WaitForSingleObject(handle, 30_000) } == WAIT_TIMEOUT {
        // SAFETY: closing a handle we own and do not use again.
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(handle);
        }
        return Err(
            "another copy of BDO Optimizer is changing the same setting — wait for it to finish"
                .to_string(),
        );
    }
    Ok(MutexGuard(handle))
}

enum Create {
    /// The name was taken; try the next one.
    Exists,
    Failed(String),
}

/// Create `dir` carrying a `High` mandatory label from the moment it exists.
///
/// Applying the label as a second step would publish the path first, and a
/// medium-integrity process watching the temp folder could plant a file — or
/// take a writable handle, which relabelling does not revoke — in between.
/// `CreateDirectoryW` takes the security descriptor up front, so there is no
/// such moment.
///
/// Unelevated the label is skipped: raising one above your own integrity level
/// needs `SeRelabelPrivilege`, and unelevated there is nothing to escalate to
/// because whatever is staged runs with the privileges an attacker at that
/// level already has.
fn create_labelled(dir: &Path) -> Result<(), Create> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
    use windows::Win32::Storage::FileSystem::CreateDirectoryW;

    let path: Vec<u16> = dir
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    if !bdo_launch::is_elevated() {
        // SAFETY: NUL-terminated path alive across the call; no attributes.
        let made = unsafe { CreateDirectoryW(PCWSTR(path.as_ptr()), None) };
        return finish(made, dir);
    }

    // SDDL for a system-access-control-list holding one mandatory label:
    // no-write-up (NW) at the High integrity level (HI).
    let sddl: Vec<u16> = "S:(ML;;NW;;;HI)"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    // SAFETY: `sddl` is NUL-terminated and alive across the call; `descriptor`
    // is a valid out-parameter the callee allocates.
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(sddl.as_ptr()),
            SDDL_REVISION_1,
            &mut descriptor,
            None,
        )
        .map_err(|e| Create::Failed(format!("could not build the integrity label: {e}")))?;
    }

    let attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: false.into(),
    };
    // SAFETY: both the path and the descriptor outlive the call, and
    // `attributes` describes the descriptor built above.
    let made = unsafe { CreateDirectoryW(PCWSTR(path.as_ptr()), Some(&attributes)) };
    // SAFETY: allocated by the conversion above, not used after this point.
    unsafe {
        let _ = LocalFree(Some(HLOCAL(descriptor.0)));
    }
    finish(made, dir)
}

/// Classify a `CreateDirectoryW` result, keeping "the name is taken" separate
/// from a real failure so the caller can try the next name.
fn finish(made: windows::core::Result<()>, dir: &Path) -> Result<(), Create> {
    use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    match made {
        Ok(()) => Ok(()),
        Err(e) => {
            // SAFETY: reading the calling thread's last-error value.
            if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
                Err(Create::Exists)
            } else {
                Err(Create::Failed(format!(
                    "a folder protected from other programs could not be created at {} ({e}), \
                     so this step was not run",
                    dir.display()
                )))
            }
        }
    }
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
