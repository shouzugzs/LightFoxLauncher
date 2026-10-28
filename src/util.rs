// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025 LightFoxLauncher contributors
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Small shared helpers: error type, env, files, logging, wide strings.
//! OWNER: lead (stable, do not change the public API without telling the lead)

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

/// Every fallible function in this crate returns `Res<T> == Result<T, String>`.
pub type Res<T> = Result<T, String>;

static QUIET: AtomicBool = AtomicBool::new(false);

/// A sink for status lines and progress, for front-ends with no console.
///
/// The CLI leaves this unset and everything goes to stdout as before; the GUI
/// installs one that appends to a log pane and repaints a progress bar.
pub trait Reporter: Send + Sync {
    /// A human-readable status line (the `[1/4] …` style messages).
    fn line(&self, msg: &str);
    /// Overall download progress: `label`, bytes done, bytes total.
    fn progress(&self, label: &str, done: u64, total: u64);
}

static REPORTER: OnceLock<Mutex<Option<Arc<dyn Reporter>>>> = OnceLock::new();

fn reporter_cell() -> &'static Mutex<Option<Arc<dyn Reporter>>> {
    REPORTER.get_or_init(|| Mutex::new(None))
}

/// Install (or clear) the global reporter. Safe to call from any thread.
pub fn set_reporter(r: Option<Arc<dyn Reporter>>) {
    if let Ok(mut g) = reporter_cell().lock() {
        *g = r;
    }
}

/// The active reporter, if a front-end installed one.
pub fn reporter() -> Option<Arc<dyn Reporter>> {
    match reporter_cell().lock() {
        Ok(g) => g.clone(),
        Err(p) => p.into_inner().clone(),
    }
}

pub fn set_quiet(v: bool) {
    QUIET.store(v, Ordering::Relaxed);
}

pub fn quiet() -> bool {
    QUIET.load(Ordering::Relaxed)
}

/// A non-fatal warning sink (e.g. a GUI message box). Unlike [`Reporter`], which
/// carries progress, this is for a single advisory line the user should see but
/// which must not abort the operation — such as "downloading without a checksum".
///
/// The CLI leaves this unset and warnings fall back to `eprintln!`.
pub trait Warn: Send + Sync {
    fn warn(&self, msg: &str);
}

static WARNER: OnceLock<Mutex<Option<Arc<dyn Warn>>>> = OnceLock::new();

fn warner_cell() -> &'static Mutex<Option<Arc<dyn Warn>>> {
    WARNER.get_or_init(|| Mutex::new(None))
}

/// Install (or clear) the global warning sink. Safe to call from any thread.
pub fn set_warner(w: Option<Arc<dyn Warn>>) {
    if let Ok(mut g) = warner_cell().lock() {
        *g = w;
    }
}

/// Emit a non-fatal warning. No-op when no sink is installed (the CLI path),
/// where the caller is expected to log it instead.
pub fn warn(msg: &str) {
    match warner_cell().lock() {
        Ok(g) => {
            if let Some(w) = g.as_ref() {
                w.warn(msg);
            }
        }
        Err(p) => {
            if let Some(w) = p.into_inner().as_ref() {
                w.warn(msg);
            }
        }
    }
}

/// Print a progress/status line unless `--quiet`. Also forwards to the reporter.
pub fn info(msg: &str) {
    if let Some(r) = reporter() {
        r.line(msg);
    }
    if !quiet() {
        println!("{msg}");
    }
}

/// Forward a progress sample to the reporter (no-op without one).
pub fn report_progress(label: &str, done: u64, total: u64) {
    if let Some(r) = reporter() {
        r.progress(label, done, total);
    }
}

/// UTF-16 + NUL, for Win32 wide APIs.
pub fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Read a NUL-terminated UTF-16 string.
///
/// # Safety
/// `p` must point to a valid NUL-terminated UTF-16 buffer.
pub unsafe fn from_wide(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    let mut n = 0usize;
    while *p.add(n) != 0 {
        n += 1;
    }
    String::from_utf16_lossy(std::slice::from_raw_parts(p, n))
}

pub fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|s| !s.is_empty())
}

pub fn home_dir() -> Option<PathBuf> {
    env("USERPROFILE").or_else(|| env("HOME")).map(PathBuf::from)
}

/// Default game/data directory: `.minecraft` next to the executable.
///
/// Keeping the whole tree (`versions/`, `libraries/`, `assets/`, `runtime/`,
/// `instances/`) beside the binary means one folder holds everything the launcher
/// needs, so it can be copied or moved as a unit.
pub fn default_dir() -> PathBuf {
    exe_dir().join(".minecraft")
}

pub fn mkdirs(p: &Path) -> Res<()> {
    std::fs::create_dir_all(p).map_err(|e| format!("cannot create directory {}: {e}", p.display()))
}

/// Join an **untrusted** relative path onto `base`, refusing anything that could
/// escape it.
///
/// `Path::join` has a trap that matters here: if the argument is absolute, it
/// *replaces* the base instead of extending it, so `base.join("C:/Windows/x")`
/// is `C:/Windows/x`. Combined with a leading `..`, that turns any remotely
/// supplied path — a library `path` from a version JSON, an asset hash, a
/// runtime manifest key — into an arbitrary-file-write primitive.
///
/// Accepts only `a/b/c` style relative paths: no drive letters, no leading
/// separator, no `.`/`..` component, no empty component, no NUL. `\` is treated
/// as `/` so a Windows-authored path is validated the same way it resolves.
pub fn safe_join(base: &Path, rel: &str) -> Res<PathBuf> {
    let normalised = rel.replace('\\', "/");
    if normalised.is_empty() {
        return Err("path is empty".to_string());
    }
    if normalised.contains('\0') {
        return Err(format!("path {rel:?} contains a NUL byte"));
    }
    if normalised.starts_with('/') {
        return Err(format!("path {rel:?} must be relative"));
    }
    let bytes = normalised.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return Err(format!("path {rel:?} must not contain a drive letter"));
    }
    let mut out = base.to_path_buf();
    for part in normalised.split('/') {
        match part {
            "" | "." | ".." => {
                return Err(format!("path {rel:?} must not contain '.', '..' or empty components"))
            }
            p => out.push(p),
        }
    }
    Ok(out)
}

/// Reject a value that is used as a **single** file or directory name.
///
/// Used for anything that becomes one path component but arrives from outside:
/// a Modrinth `filename`, an asset-index id, a loader profile id. Rejects path
/// separators, `.`/`..`, drive letters, control characters, and the Windows
/// reserved device names (`con`, `nul`, `com1`, …) — the last of which are legal
/// directory components but still unusable as files.
pub fn safe_component(value: &str) -> Res<()> {
    if value.is_empty() {
        return Err("name is empty".to_string());
    }
    if value == "." || value == ".." {
        return Err(format!("illegal name {value:?}"));
    }
    if value.contains('\0') || value.chars().any(|c| c.is_control()) {
        return Err(format!("name {value:?} contains a control character"));
    }
    if value.contains('/') || value.contains('\\') || value.contains(':') {
        return Err(format!("name {value:?} must not contain a path separator or drive"));
    }
    const BAD: [char; 6] = ['<', '>', '"', '|', '?', '*'];
    if value.chars().any(|c| BAD.contains(&c)) {
        return Err(format!("name {value:?} contains a reserved character"));
    }
    let stem = value.split('.').next().unwrap_or(value).to_ascii_uppercase();
    const RESERVED: [&str; 22] = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    if RESERVED.contains(&stem.as_str()) {
        return Err(format!("name {value:?} is a reserved Windows device name"));
    }
    Ok(())
}

pub fn file_size(p: &Path) -> u64 {
    std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)
}

pub fn human_size(n: u64) -> String {
    const K: f64 = 1024.0;
    let n = n as f64;
    if n >= K * K * K {
        format!("{:.2} GB", n / (K * K * K))
    } else if n >= K * K {
        format!("{:.1} MB", n / (K * K))
    } else if n >= K {
        format!("{:.0} KB", n / K)
    } else {
        format!("{n:.0} B")
    }
}

/// Directory that contains the running executable (falls back to cwd).
pub fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

/// `"windows"` — the only platform this launcher supports.
pub fn os_name() -> &'static str {
    "windows"
}

/// `"x64"` on 64-bit Windows builds.
pub fn os_arch() -> &'static str {
    if cfg!(target_pointer_width = "64") {
        "x64"
    } else {
        "x86"
    }
}

pub fn os_version() -> &'static str {
    "10.0"
}

pub fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// `true` if `s` looks like an absolute Windows path (`C:\...`, `\\?\...`, `\\server\share`).
pub fn is_absolute_win(s: &str) -> bool {
    let b = s.as_bytes();
    (b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/'))
        || s.starts_with("\\\\")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1024), "1 KB");
        assert_eq!(human_size(1024 * 1024), "1.0 MB");
        assert_eq!(human_size(1024 * 1023), "1023 KB");
        assert_eq!(human_size(3 * 1024 * 1024), "3.0 MB");
        assert_eq!(human_size(2 * 1024 * 1024 * 1024), "2.00 GB");
    }

    #[test]
    fn absolute_paths() {
        assert!(is_absolute_win("C:\\x"));
        assert!(is_absolute_win("D:/x"));
        assert!(is_absolute_win("\\\\server\\share"));
        assert!(!is_absolute_win("x/y"));
        assert!(!is_absolute_win(""));
    }

    #[test]
    fn wide_roundtrip() {
        let w = to_wide("héllo");
        assert_eq!(w.last(), Some(&0));
        assert_eq!(unsafe { from_wide(w.as_ptr()) }, "héllo");
    }

    #[test]
    fn safe_join_accepts_relative_paths() {
        let base = Path::new("C:/mc/libraries");
        assert_eq!(
            safe_join(base, "org/lwjgl/lwjgl/3.3.1/lwjgl-3.3.1.jar").unwrap(),
            base.join("org/lwjgl/lwjgl/3.3.1/lwjgl-3.3.1.jar")
        );
        // A Windows-authored path is normalised the same way it resolves.
        assert_eq!(
            safe_join(base, "org\\lwjgl\\x.jar").unwrap(),
            base.join("org/lwjgl/x.jar")
        );
        assert!(safe_join(base, "bin/java.exe").is_ok());
    }

    #[test]
    fn safe_join_rejects_escapes() {
        let base = Path::new("C:/mc/libraries");
        for bad in [
            "",              // empty
            "../evil.jar",   // parent
            "a/../../evil",  // nested parent
            "..",            // parent only
            ".",             // current
            "/abs.jar",      // absolute
            "C:/Windows/x",  // drive letter replaces the base under Path::join
            "C:\\Windows\\x",
            "a//b",          // empty component
            "a/\0/b",        // NUL
        ] {
            assert!(safe_join(base, bad).is_err(), "should reject {bad:?}");
        }
    }

    #[test]
    fn safe_component_accepts_real_names() {
        for good in ["1.20.1", "fabric-loader-0.16.9-1.20.1", "sodium-0.5.8.jar", "默认", "a-b_c"] {
            assert!(safe_component(good).is_ok(), "should accept {good:?}");
        }
    }

    #[test]
    fn safe_component_rejects_paths_and_devices() {
        for bad in [
            "", ".", "..", "a/b", "a\\b", "C:", "x:y", "a\0b", "con", "CON.ini", "nul.jar",
            "com1", "lpt9.txt", "a<b", "a|b", "a?b", "a*b", "a\"b",
        ] {
            assert!(safe_component(bad).is_err(), "should reject {bad:?}");
        }
    }
}
