// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025 LightFoxLauncher contributors
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Cross-process exclusive lock for the shared data directory.
//!
//! Two launchers may run at once (that is the whole point of instances), but
//! they must not download into `libraries/`, `assets/` or `runtime/` at the same
//! time: two JRE installers racing on the same 403 files, or two ZIP extractors
//! writing the same native DLL, can leave a half-written file behind.
//!
//! The lock is a `CreateFileW` handle opened with `dwShareMode = 0`. Windows
//! closes it when the process exits — including on a crash — so unlike a
//! `create_new` sentinel file it can never go stale and never needs cleanup.
//! It is held only while preparing (downloading/verifying), not while playing,
//! so a second instance waits for the first download to finish and then starts.

#[cfg(not(windows))]
use std::path::PathBuf;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::util::Res;

/// How long to wait for the other process before giving up.
const DEFAULT_WAIT: Duration = Duration::from_secs(30 * 60);
const POLL: Duration = Duration::from_millis(250);

/// Held for as long as the guarded work runs; released on drop.
pub struct DirLock {
    #[cfg(windows)]
    handle: *mut std::ffi::c_void,
    #[cfg(not(windows))]
    path: PathBuf,
}

// The handle is only ever used to close itself, which is thread-safe.
unsafe impl Send for DirLock {}

impl Drop for DirLock {
    fn drop(&mut self) {
        #[cfg(windows)]
        unsafe {
            if !self.handle.is_null() {
                ffi::CloseHandle(self.handle);
            }
        }
        #[cfg(not(windows))]
        {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

#[cfg(windows)]
mod ffi {
    use std::ffi::c_void;

    #[link(name = "kernel32")]
    extern "system" {
        pub fn CreateFileW(
            name: *const u16,
            access: u32,
            share: u32,
            sa: *const c_void,
            disposition: u32,
            flags: u32,
            template: *mut c_void,
        ) -> *mut c_void;
        pub fn CloseHandle(h: *mut c_void) -> i32;
        pub fn GetLastError() -> u32;
    }

    pub const GENERIC_READ: u32 = 0x8000_0000;
    pub const GENERIC_WRITE: u32 = 0x4000_0000;
    pub const OPEN_ALWAYS: u32 = 4;
    pub const FILE_ATTRIBUTE_NORMAL: u32 = 0x0000_0080;
    /// Returned when another handle holds the file without sharing.
    pub const ERROR_SHARING_VIOLATION: u32 = 32;
    pub const ERROR_ACCESS_DENIED: u32 = 5;
}

/// Try once to take the lock. `Ok(None)` means someone else holds it.
fn try_acquire(path: &Path) -> Res<Option<DirLock>> {
    #[cfg(windows)]
    {
        let wide: Vec<u16> = path
            .display()
            .to_string()
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let handle = unsafe {
            ffi::CreateFileW(
                wide.as_ptr(),
                ffi::GENERIC_READ | ffi::GENERIC_WRITE,
                0, // no sharing: exclusive while we hold it
                std::ptr::null(),
                ffi::OPEN_ALWAYS,
                ffi::FILE_ATTRIBUTE_NORMAL,
                std::ptr::null_mut(),
            )
        };
        if !handle.is_null() && handle as isize != -1 {
            return Ok(Some(DirLock { handle }));
        }
        let err = unsafe { ffi::GetLastError() };
        if err == ffi::ERROR_SHARING_VIOLATION {
            return Ok(None);
        }
        if err == ffi::ERROR_ACCESS_DENIED {
            // The file exists but cannot be opened for write at all: treat it as
            // "busy" rather than failing the launch, the retry loop will decide.
            return Ok(None);
        }
        Err(format!(
            "cannot open lock file {} (win32 error {err})",
            path.display()
        ))
    }
    #[cfg(not(windows))]
    {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
        {
            Ok(mut f) => {
                use std::io::Write;
                let _ = writeln!(f, "{}", std::process::id());
                Ok(Some(DirLock {
                    path: path.to_path_buf(),
                }))
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(None),
            Err(e) => Err(format!("cannot create lock file {}: {e}", path.display())),
        }
    }
}

/// Try to take a lock on one file, without waiting.
///
/// `Ok(None)` means another process is transferring that file right now; the
/// caller should skip it (the other process will produce the same bytes) rather
/// than queue behind it. `Err` means the lock file itself could not be touched,
/// which the caller treats as "not mine to write".
pub fn try_acquire_file(path: &Path) -> Res<Option<DirLock>> {
    match path.parent() {
        Some(d) if !d.as_os_str().is_empty() => crate::util::mkdirs(d)?,
        _ => {}
    }
    try_acquire(path)
}

/// Take the data-directory lock, waiting for any other launcher to finish.
///
/// `on_wait` runs once, the first time we find the lock held, so the front-end
/// can tell the user why nothing is happening yet.
pub fn acquire(dir: &Path, on_wait: impl FnOnce()) -> Res<DirLock> {
    acquire_with(dir, DEFAULT_WAIT, on_wait)
}

pub fn acquire_with(
    dir: &Path,
    max_wait: Duration,
    on_wait: impl FnOnce(),
) -> Res<DirLock> {
    crate::util::mkdirs(dir)?;
    let path = dir.join("download.lock");
    let started = Instant::now();
    // `FnOnce` must be stashed so the loop can keep checking without calling it twice.
    let mut on_wait = Some(on_wait);
    loop {
        if let Some(lock) = try_acquire(&path)? {
            return Ok(lock);
        }
        if let Some(f) = on_wait.take() {
            f();
        }
        if started.elapsed() >= max_wait {
            return Err(format!(
                "another launcher has been using {} for over {} minutes; \
                 close it and try again",
                dir.display(),
                max_wait.as_secs() / 60
            ));
        }
        std::thread::sleep(POLL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_dir(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "lfl-lock-test-{}-{}",
            tag,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn second_acquire_in_same_process_is_blocked() {
        let dir = temp_dir("excl");
        let first = acquire_with(&dir, Duration::from_millis(200), || {}).unwrap();
        // A second handle with no sharing must not be granted.
        let mut notified = false;
        let err = acquire_with(&dir, Duration::from_millis(300), || notified = true)
            .err()
            .expect("second lock should time out");
        assert!(notified, "on_wait should have fired");
        assert!(err.contains("another launcher"), "unexpected: {err}");
        drop(first);
        // After release it must succeed again.
        let _again = acquire_with(&dir, Duration::from_millis(500), || {})
            .expect("lock must be reacquirable once released");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn creates_missing_directory() {
        let dir = temp_dir("mkdir").join("nested").join("deeper");
        let lock = acquire_with(&dir, Duration::from_millis(500), || {}).unwrap();
        drop(lock);
        assert!(dir.exists());
        let _ = std::fs::remove_dir_all(std::env::temp_dir().join(format!(
            "lfl-lock-test-mkdir-{}",
            std::process::id()
        )));
    }
}
