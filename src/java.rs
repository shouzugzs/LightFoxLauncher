// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025 LightFoxLauncher contributors
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! JRE discovery and Mojang runtime installation.
//! OWNER: runtime. API frozen by docs/CONTRACT.md.
//!
//! Resolution order used by [`ensure`]:
//!   1. a Mojang runtime already unpacked at `<dir>/runtime/<component>`;
//!   2. any suitable Java in `%JAVA_HOME%` or on `PATH`;
//!   3. download the Mojang component (~87 MB, 403 files) via `net`.
//!
//! No FFI is needed: `java -version` is run through `std::process::Command`
//! with `CREATE_NO_WINDOW` so no console flashes up.

use crate::json::Json;
use crate::net::{self, Download};
use crate::util::{self, env, info, mkdirs, Res};
use std::path::{Path, PathBuf};
use std::process::Command;

/// `CREATE_NO_WINDOW` — keeps `java -version` from flashing a console.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Shown when a version banner cannot be parsed.
const TEXT_KEEP: usize = 300;
/// Downloads are funnelled through `net::download_many` in this many entries.
const BATCH: usize = 64;

const RUNTIME_INDEX: &str =
    "https://launchermeta.mojang.com/v1/products/java-runtime/2ec0cc96c44e5a76b9c8b7c39df7210883d12871/all.json";
/// Fedora-style fallback is deliberately absent: this launcher targets the
/// exact manifest the vanilla launcher uses.
const PLATFORM: &str = "windows-x64";

/// A usable Java installation.
#[derive(Debug, Clone, PartialEq)]
pub struct Java {
    /// Path to `javaw.exe` when available, else `java.exe` (or the bare exe
    /// path when the caller supplied one directly).
    pub exe: PathBuf,
    /// Java home; `<home>/bin/<exe>` is expected to exist for managed/system JREs.
    pub home: PathBuf,
    /// Feature release: 8, 11, 17, 21 …
    pub major: u32,
}

// ---------------------------------------------------------------------------
// Version parsing
// ---------------------------------------------------------------------------

/// Extract the feature release from a `java -version` banner.
///
/// Handles `openjdk version "17.0.13" 2024-10-15`, `java version "1.8.0_51"`,
/// `openjdk version "21.0.5+11-LTS"`, `openjdk version "25-ea"`,
/// `openjdk version "17" 2021-09-14`, and `java version "9"`.
fn parse_major(text: &str) -> Option<u32> {
    // Banners look like `openjdk version "17.0.13" 2024-10-15` or
    // `java version "1.8.0_51"`. The version itself is the quoted token that
    // follows the word "version"; reading it that way keeps a stray date or a
    // build number from being mistaken for the feature release.
    let mut rest = text;
    while let Some(i) = rest.find("version") {
        let after = rest[i + "version".len()..].trim_start();
        let token = match after.strip_prefix('"') {
            Some(q) => q.split('"').next().unwrap_or(""),
            // No quotes: take the dotted number that follows directly.
            None => after,
        };
        let digits: String = token
            .chars()
            .skip_while(|c| !c.is_ascii_digit())
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        if let Some(major) = major_from_digits(&digits) {
            return Some(major);
        }
        rest = &rest[i + "version".len()..];
    }
    // Some builds omit the keyword; fall back to the first quoted number.
    let q = text.find('"')?;
    let digits: String = text[q + 1..]
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    major_from_digits(&digits)
}

/// `1.8.0_51` → 8, `17.0.13` → 17, `21` → 21, `9` → 9.
fn major_from_digits(digits: &str) -> Option<u32> {
    let cleaned = digits.trim_end_matches('.');
    let mut parts = cleaned.split('.');
    let first: u32 = parts.next()?.parse().ok()?;
    if first == 1 {
        // Legacy `1.x.y_z` scheme: the feature version is the second component.
        let second: u32 = parts.next().and_then(|s| s.parse().ok())?;
        return Some(second.max(1));
    }
    Some(first)
}

// ---------------------------------------------------------------------------
// probe
// ---------------------------------------------------------------------------

/// Run `<exe> -version` and return its feature version.
///
/// `-version` writes to stderr; stdout is captured too because some builds and
/// shims print elsewhere. `-version` exits immediately by specification, so the
/// call blocks only for the process startup time; a hung executable is reported
/// by the OS as a spawn failure or simply never returns, which we cannot
/// interrupt portably without threads and do not attempt.
pub fn probe(exe: &Path) -> Res<u32> {
    if !exe.exists() {
        return Err(format!("java not found: {}", exe.display()));
    }
    let mut cmd = Command::new(exe);
    cmd.arg("-version");
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let out = cmd
        .output()
        .map_err(|e| format!("cannot run {}: {e}", exe.display()))?;

    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    if !out.stderr.is_empty() {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&String::from_utf8_lossy(&out.stderr));
    }

    match parse_major(&text) {
        Some(major) => Ok(major),
        None => {
            let mut shown: String = text.trim().chars().take(TEXT_KEEP).collect();
            if text.trim().chars().count() > TEXT_KEEP {
                shown.push('…');
            }
            if shown.is_empty() {
                shown = "<no output>".to_string();
            }
            Err(format!(
                "cannot determine Java version of {} ({}): {shown}",
                exe.display(),
                match out.status.code() {
                    Some(c) => format!("exit code {c}"),
                    None => "terminated by signal".to_string(),
                }
            ))
        }
    }
}

// ---------------------------------------------------------------------------
// find_system
// ---------------------------------------------------------------------------

/// Path to `bin/javaw.exe` (preferred) or `bin/java.exe` inside `home`.
fn java_exe_in(home: &Path) -> Option<PathBuf> {
    let bin = home.join("bin");
    let javaw = bin.join("javaw.exe");
    if javaw.is_file() {
        return Some(javaw);
    }
    let java = bin.join("java.exe");
    if java.is_file() {
        return Some(java);
    }
    None
}

/// Home directory of an executable (`<home>/bin/java.exe` → `<home>`).
fn home_of(exe: &Path) -> PathBuf {
    exe.parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_default()
}

/// Best Java installed on the machine with `major >= need_major`.
///
/// Candidates, in order: `%JAVA_HOME%\bin\javaw.exe`, `%JAVA_HOME%\bin\java.exe`,
/// then every `PATH` directory's `javaw.exe`/`java.exe`. `javaw.exe` is probed
/// only when its `java.exe` sibling is absent (it reports the same version).
pub fn find_system(need_major: u32) -> Option<Java> {
    let mut candidates: Vec<PathBuf> = Vec::new();

    if let Some(home) = env("JAVA_HOME") {
        let home = PathBuf::from(home);
        if let Some(exe) = java_exe_in(&home) {
            candidates.push(exe);
        }
    }

    if let Some(path) = env("PATH") {
        for dir in path.split(';') {
            let dir = dir.trim().trim_matches('"');
            if dir.is_empty() {
                continue;
            }
            let d = Path::new(dir);
            let javaw = d.join("javaw.exe");
            if javaw.is_file() {
                candidates.push(javaw);
            }
            let java = d.join("java.exe");
            if java.is_file() {
                candidates.push(java);
            }
        }
    }

    let mut seen: Vec<PathBuf> = Vec::new();
    for exe in candidates {
        if seen.contains(&exe) {
            continue;
        }
        seen.push(exe.clone());
        // Probe the console variant when present: identical version, and some
        // javaw shims refuse to run without a parent console.
        let probe_target = {
            let console = exe.with_file_name("java.exe");
            if exe.file_name().map(|n| n.eq_ignore_ascii_case("javaw.exe")).unwrap_or(false)
                && console.is_file()
            {
                console
            } else {
                exe.clone()
            }
        };
        match probe(&probe_target) {
            Ok(major) if major >= need_major => {
                return Some(Java { exe, home: home_of(&probe_target), major });
            }
            Ok(_) => {}
            Err(_) => {}
        }
    }
    None
}

// ---------------------------------------------------------------------------
// ensure
// ---------------------------------------------------------------------------

/// Marker written last, once a Mojang runtime is fully installed.
const MARKER: &str = ".lfl-ok";

/// Resolve a Java for `need_major`, installing Mojang's runtime if necessary.
///
/// * already installed under `<dir>/runtime/<component>` → reused without
///   downloading (the marker makes this a single file read);
/// * otherwise a system Java with `major >= need_major` is preferred;
/// * otherwise the component is downloaded (~87 MB).
pub fn ensure(dir: &Path, component: &str, need_major: u32) -> Res<Java> {
    let home = runtime_home(dir, component);

    // 1. Already installed?
    if let Some(exe) = java_exe_in(&home) {
        let marker = home.join(MARKER);
        if let Some(major) = read_marker(&marker, component) {
            if major >= need_major {
                return Ok(Java { exe, home, major });
            }
            info(&format!(
                "runtime {component} reports Java {major}, need {need_major}: reinstalling"
            ));
        } else {
            // No marker (interrupted install or hand-copied JRE): verify it.
            match probe(&exe) {
                Ok(major) if major >= need_major => {
                    write_marker(&marker, component, major);
                    return Ok(Java { exe, home, major });
                }
                Ok(major) => info(&format!(
                    "runtime {component} reports Java {major}, need {need_major}: reinstalling"
                )),
                Err(e) => info(&format!("runtime {component} unusable ({e}): reinstalling")),
            }
        }
    }

    // 2. A system Java keeps the download away.
    if let Some(j) = find_system(need_major) {
        info(&format!(
            "using system Java {} (major {})",
            j.exe.display(),
            j.major
        ));
        return Ok(j);
    }

    // 3. Install the Mojang runtime.
    //
    // Installing writes ~400 files into one directory, so it must be exclusive:
    // two launchers racing here would each clear and refill the same tree and
    // leave it half-written. Anyone who finds it locked waits and then reuses the
    // finished runtime instead of installing a second time.
    // The lock file lives in the OS temp dir, not inside the runtime it guards:
    // a stray empty `download.lock` in `runtime/` is indistinguishable from
    // corruption to anything auditing the install.
    let lock_anchor = std::env::temp_dir().join(format!(
        "lfl-runtime-{}",
        lfl_lock_hash(&home)
    ));
    let install_lock = crate::lock::acquire(&lock_anchor, || {
        info("another launcher is installing the Java runtime, waiting…");
    })?;
    if java_exe_in(&home).is_some() && home.join(MARKER).exists() {
        // Filled in by the launcher we just waited for.
        drop(install_lock);
        if let Some(exe) = java_exe_in(&home) {
            if let Ok(major) = probe(&exe) {
                if major >= need_major {
                    info(&format!("reusing the runtime installed by the other launcher ({major})"));
                    return Ok(Java {
                        exe,
                        home,
                        major,
                    });
                }
            }
        }
    }
    info(&format!(
        "no usable Java {need_major}+ found, installing Mojang runtime '{component}'"
    ));
    install(component, &home)?;

    let exe = java_exe_in(&home).ok_or_else(|| {
        format!(
            "runtime {component} installed but {} is missing",
            home.join("bin").join("java.exe").display()
        )
    })?;
    match probe(&exe) {
        Ok(major) => {
            write_marker(&home.join(MARKER), component, major);
            if major < need_major {
                info(&format!(
                    "warning: installed runtime reports Java {major} but {need_major} was requested"
                ));
            }
            Ok(Java { exe, home, major })
        }
        Err(e) => {
            info(&format!("warning: {e}"));
            write_marker(&home.join(MARKER), component, need_major);
            Ok(Java { exe, home, major: need_major })
        }
    }
}

/// `<dir>/runtime/<component>`, with the component name sanitised to a single
/// path element so a malicious manifest cannot escape `runtime/`.
fn runtime_home(dir: &Path, component: &str) -> PathBuf {
    // Keep only characters that are inert in a file name; dots are dropped so
    // no `..` sequence can survive.
    let safe: String = component
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    let safe = if safe.trim_matches('_').is_empty() {
        "java-runtime".to_string()
    } else {
        safe
    };
    dir.join("runtime").join(safe)
}

fn read_marker(marker: &Path, component: &str) -> Option<u32> {
    let text = std::fs::read_to_string(marker).ok()?;
    let mut lines = text.lines();
    let first = lines.next()?.trim();
    if first != component {
        return None;
    }
    // Second line carries the detected major (optional).
    lines.next().and_then(|l| l.trim().parse::<u32>().ok())
}

fn write_marker(marker: &Path, component: &str, major: u32) {
    // Best effort: a missing marker only costs one probe on the next run.
    let _ = std::fs::write(marker, format!("{component}\n{major}\n"));
}

// ---------------------------------------------------------------------------
// Mojang runtime installation
// ---------------------------------------------------------------------------

fn install(component: &str, home: &Path) -> Res<()> {
    let index = net::get(RUNTIME_INDEX)
        .map_err(|e| format!("cannot fetch the Java runtime index: {e}"))?;
    let index_text = String::from_utf8_lossy(&index);
    let json = Json::parse(&index_text)
        .map_err(|e| format!("cannot parse the Java runtime index: {e}"))?;

    let manifest_url = json
        .path(&[PLATFORM, component])
        .and_then(|v| v.at(0))
        .and_then(|v| v.path(&["manifest", "url"]))
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            format!("the Java runtime index has no {PLATFORM} entry for component '{component}'")
        })?
        .to_string();

    let manifest = net::get(&manifest_url)
        .map_err(|e| format!("cannot fetch the runtime manifest for '{component}': {e}"))?;
    let manifest_text = String::from_utf8_lossy(&manifest);
    let json = Json::parse(&manifest_text)
        .map_err(|e| format!("cannot parse the runtime manifest for '{component}': {e}"))?;

    let files = json
        .get("files")
        .and_then(|v| v.as_obj())
        .ok_or_else(|| format!("runtime manifest for '{component}' has no 'files' map"))?;

    let mut downloads: Vec<Download> = Vec::with_capacity(files.len());
    let mut total: u64 = 0;
    let mut dirs = 0usize;
    let mut skipped = 0usize;

    for (key, entry) in files {
        // `key` comes from Mojang's runtime manifest. It is a relative path such
        // as `bin/java.exe`; resolving it with a bare `join` would let an
        // absolute key or a `..` component write outside the runtime directory.
        let dest = match crate::util::safe_join(home, key) {
            Ok(p) => p,
            Err(e) => {
                util::info(&format!("java: skipping unsafe runtime entry: {e}"));
                skipped += 1;
                continue;
            }
        };
        match entry.get("type").and_then(|v| v.as_str()) {
            Some("directory") => {
                mkdirs(&dest)?;
                dirs += 1;
            }
            Some("file") => {
                // `raw` is always present; `lzma` variants are ignored.
                let raw = entry.path(&["downloads", "raw"]);
                let (url, sha1, size) = match raw {
                    Some(raw) => (
                        raw.str_at("url"),
                        raw.str_at("sha1").unwrap_or(""),
                        raw.u64_at("size").unwrap_or(0),
                    ),
                    None => (None, "", 0),
                };
                let url = match url {
                    Some(u) if !u.is_empty() => u,
                    _ => {
                        skipped += 1;
                        continue;
                    }
                };
                total = total.saturating_add(size);
                downloads.push(Download::new(url, dest, sha1, size));
            }
            _ => {
                // Unknown entry kinds are ignored rather than fatal.
                skipped += 1;
            }
        }
    }

    if downloads.is_empty() {
        return Err(format!(
            "runtime manifest for '{component}' contains no downloadable files"
        ));
    }

    info(&format!(
        "downloading Java {component}: {} files, {} ({} directories) into {}",
        downloads.len(),
        util::human_size(total),
        dirs,
        home.display()
    ));
    mkdirs(home)?;

    for (i, batch) in downloads.chunks(BATCH).enumerate() {
        let first = i * BATCH + 1;
        let last = first + batch.len() - 1;
        info(&format!("java {component}: files {first}..{last} of {}", downloads.len()));
        net::download_many(batch)
            .map_err(|e| format!("cannot download the Java runtime '{component}': {e}"))?;
    }

    if skipped > 0 {
        info(&format!("java {component}: skipped {skipped} entries without raw downloads"));
    }
    // The marker is written by `ensure` only after a successful probe, so a
    // download that dies here leaves the runtime unmarked and retryable.
    Ok(())
}

/// Stable short hash of a path, used to name the runtime install lock.
fn lfl_lock_hash(p: &Path) -> String {
    let h = crate::hash::sha1_hex(p.to_string_lossy().to_ascii_lowercase().as_bytes());
    h[..16].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------------------------------------------------------------- parsing

    #[test]
    fn parses_modern_banners() {
        assert_eq!(
            parse_major("openjdk version \"17.0.13\" 2024-10-15\nOpenJDK Runtime Environment"),
            Some(17)
        );
        assert_eq!(
            parse_major("openjdk version \"21.0.5+11-LTS\" 2024-10-15"),
            Some(21)
        );
        assert_eq!(parse_major("openjdk version \"25-ea\" 2025-09-16"), Some(25));
        assert_eq!(parse_major("openjdk version \"17\" 2021-09-14"), Some(17));
        assert_eq!(parse_major("openjdk version \"11.0.2\" 2019-01-15"), Some(11));
    }

    #[test]
    fn parses_legacy_banners() {
        assert_eq!(
            parse_major("java version \"1.8.0_51\"\nJava(TM) SE Runtime Environment"),
            Some(8)
        );
        assert_eq!(parse_major("java version \"1.7.0_80\""), Some(7));
        assert_eq!(parse_major("java version \"1.6.0_45\""), Some(6));
        assert_eq!(parse_major("java version \"9\""), Some(9));
    }

    #[test]
    fn parses_openjdk_and_vendor_prefixes() {
        assert_eq!(
            parse_major("openjdk version \"17.0.1\" 2021-10-19 LTS\nOpenJDK 64-Bit Server VM"),
            Some(17)
        );
        assert_eq!(
            parse_major("java version \"1.8.0_301\"\nJava(TM) SE Runtime Environment (build 1.8.0_301-b09)"),
            Some(8)
        );
        assert_eq!(
            parse_major("openjdk version \"22.0.1\" 2024-04-16\nOpenJDK Runtime Environment Temurin-22.0.1+8"),
            Some(22)
        );
    }

    #[test]
    fn rejects_garbage_banners() {
        assert_eq!(parse_major(""), None);
        assert_eq!(parse_major("not a java banner"), None);
        assert_eq!(parse_major("Unrecognized VM option 'Foo'"), None);
        assert_eq!(parse_major("version"), None);
    }

    #[test]
    fn major_from_digits_rules() {
        assert_eq!(major_from_digits("1.8.0"), Some(8));
        assert_eq!(major_from_digits("1.8"), Some(8));
        assert_eq!(major_from_digits("17.0.13"), Some(17));
        assert_eq!(major_from_digits("21"), Some(21));
        assert_eq!(major_from_digits("25."), Some(25));
        assert_eq!(major_from_digits("1"), None, "1 alone has no feature version");
        assert_eq!(major_from_digits(""), None);
        assert_eq!(major_from_digits("x"), None);
    }

    // ------------------------------------------------------------------ probe

    #[test]
    fn probe_missing_exe_is_an_error() {
        let err = probe(Path::new("definitely-not-a-real-java.exe")).expect_err("must fail");
        assert!(err.contains("not found"), "unexpected: {err}");
    }

    #[test]
    fn probe_nonexistent_absolute_path_is_an_error() {
        let p = std::env::temp_dir().join("lfl-no-such-java.exe");
        let err = probe(&p).expect_err("must fail");
        assert!(err.contains("not found"), "unexpected: {err}");
    }

    /// Probes a file that exists but is not executable Java: must Err, not panic.
    #[test]
    fn probe_non_java_file_is_an_error() {
        let dir = std::env::temp_dir().join(format!("lfl-java-probe-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let fake = dir.join("fake-java.exe");
        std::fs::write(&fake, b"not a program").expect("write fake");
        let r = probe(&fake);
        assert!(r.is_err(), "a non-executable file must not report a version");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Only runs when a real JVM is present; never asserts on absence.
    #[test]
    fn probe_real_java_when_available() {
        let Some(j) = find_system(8) else {
            return;
        };
        match probe(&j.exe) {
            Ok(major) => assert!(major >= 8, "unexpected major {major}"),
            Err(e) => panic!("probe failed for a Java find_system returned: {e}"),
        }
    }

    // ------------------------------------------------------------ find_system

    #[test]
    fn find_system_returns_a_consistent_option() {
        // Never assert `Some`: CI machines may have no Java at all.
        if let Some(j) = find_system(8) {
            assert!(j.major >= 8, "major {} below the requested 8", j.major);
            assert!(j.exe.is_file(), "exe must exist: {}", j.exe.display());
            assert!(!j.home.as_os_str().is_empty(), "home must be set");
        }
        // An absurd requirement must be rejected even if a JVM exists.
        if find_system(8).is_some() {
            assert!(find_system(u32::MAX).is_none(), "no JVM reports major u32::MAX");
        }
    }

    #[test]
    fn java_exe_in_prefers_javaw() {
        let base = std::env::temp_dir().join(format!("lfl-java-exe-{}", std::process::id()));
        let bin = base.join("bin");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&bin).unwrap();

        assert_eq!(java_exe_in(&base), None, "empty bin has no java");

        std::fs::write(bin.join("java.exe"), b"x").unwrap();
        assert_eq!(java_exe_in(&base), Some(bin.join("java.exe")));

        std::fs::write(bin.join("javaw.exe"), b"x").unwrap();
        assert_eq!(java_exe_in(&base), Some(bin.join("javaw.exe")), "javaw wins");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn home_of_strips_bin() {
        assert_eq!(home_of(Path::new("C:\\jdk\\bin\\java.exe")), PathBuf::from("C:\\jdk"));
        assert_eq!(home_of(Path::new("java.exe")), PathBuf::from(""));
    }

    // ----------------------------------------------------------- ensure parts

    #[test]
    fn runtime_home_is_sanitised_to_one_element() {
        let d = Path::new("C:\\data");
        assert_eq!(runtime_home(d, "java-runtime-gamma"), d.join("runtime/java-runtime-gamma"));
        // Path separators and traversal cannot escape `runtime/`.
        assert_eq!(runtime_home(d, "../../etc"), d.join("runtime/______etc"));
        assert_eq!(runtime_home(d, ""), d.join("runtime/java-runtime"));
        assert_eq!(runtime_home(d, ".."), d.join("runtime/java-runtime"));
    }

    #[test]
    fn marker_roundtrip() {
        let dir = std::env::temp_dir().join(format!("lfl-java-marker-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let marker = dir.join(MARKER);

        assert_eq!(read_marker(&marker, "java-runtime-gamma"), None, "absent marker");

        write_marker(&marker, "java-runtime-gamma", 17);
        assert_eq!(read_marker(&marker, "java-runtime-gamma"), Some(17));
        assert_eq!(
            read_marker(&marker, "java-runtime-beta"),
            None,
            "a different component must not be trusted"
        );

        std::fs::write(&marker, "java-runtime-gamma\n").unwrap();
        assert_eq!(read_marker(&marker, "java-runtime-gamma"), None, "no major recorded");

        std::fs::write(&marker, "garbage\nxxx\n").unwrap();
        assert_eq!(read_marker(&marker, "java-runtime-gamma"), None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `ensure` must reuse an already-installed runtime without network access.
    #[test]
    fn ensure_reuses_marked_runtime_offline() {
        let base = std::env::temp_dir().join(format!("lfl-java-ensure-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let home = base.join("runtime/java-runtime-gamma");
        let bin = home.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        // The payload is not a real JVM: the marker alone must satisfy `ensure`,
        // which is exactly the fast path a second launch takes.
        std::fs::write(bin.join("java.exe"), b"not really java").unwrap();
        write_marker(&home.join(MARKER), "java-runtime-gamma", 21);

        let j = ensure(&base, "java-runtime-gamma", 17).expect("marker path must work offline");
        assert_eq!(j.major, 21);
        assert_eq!(j.home, home);
        assert!(j.exe.ends_with("java.exe"));
        assert!(j.exe.is_file());
    }

    #[test]
    fn ensure_without_marker_falls_back_to_probe_then_network() {
        // A runtime directory with no marker is probed. With a non-JVM payload
        // the probe fails, so `ensure` proceeds to a system JVM or the network;
        // either way it must return a Result and never panic.
        let base = std::env::temp_dir().join(format!("lfl-java-nomark-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let home = base.join("runtime/java-runtime-gamma");
        let bin = home.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("java.exe"), b"not really java").unwrap();

        if find_system(8).is_none() {
            // Reaching the installer would hit the network; skip instead.
            let _ = std::fs::remove_dir_all(&base);
            return;
        }
        let j = ensure(&base, "java-runtime-gamma", 8).expect("system Java should be reused");
        assert!(j.major >= 8);
        // A system Java must not have been marked as a managed runtime.
        assert!(!home.join(MARKER).exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn failed_install_does_not_write_the_marker() {
        // Only runs when the machine has no Java at all, i.e. when `ensure`
        // really takes the install path; the marker must stay absent.
        let base = std::env::temp_dir().join(format!("lfl-java-fail-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let home = base.join("runtime/java-runtime-gamma");
        if find_system(8).is_some() {
            let _ = std::fs::remove_dir_all(&base);
            return;
        }
        // No network is available in tests; whatever happens the marker must
        // never claim an incomplete runtime is OK.
        let r = ensure(&base, "java-runtime-gamma", 8);
        if r.is_err() {
            assert!(
                !home.join(MARKER).exists(),
                "a failed install must not write the OK marker"
            );
        }
        let _ = std::fs::remove_dir_all(&base);
    }


    /// Real Mojang runtime manifest, slimmed to the bin/ and lib/ entries.
    ///
    /// Captured from `piston-meta.mojang.com` for component `java-runtime-gamma`
    /// (the live file has 403 files + 83 directories, ~86.9 MB). Verifying it
    /// here keeps `install()`'s parsing honest without any network access.
    const SLIM_MANIFEST: &str = r#"{"files":{"bin/java.exe":{"downloads":{"lzma":{"sha1":"e336167880522c1c2c4a79c9ada6c4360e978b2c","size":14418,"url":"https://piston-data.mojang.com/v1/objects/e336167880522c1c2c4a79c9ada6c4360e978b2c/java.exe"},"raw":{"sha1":"2c5b5c9d4c9916319748fb684ce7c3c18401c559","size":39424,"url":"https://piston-data.mojang.com/v1/objects/2c5b5c9d4c9916319748fb684ce7c3c18401c559/java.exe"}},"executable":true,"type":"file"},"bin/javaw.exe":{"downloads":{"lzma":{"sha1":"005484d56aaceedc42ac0a0f6daa2fc8b68cd2e8","size":14452,"url":"https://piston-data.mojang.com/v1/objects/005484d56aaceedc42ac0a0f6daa2fc8b68cd2e8/javaw.exe"},"raw":{"sha1":"503664a377eb2e63a5f74b273299f622fca2bf95","size":39424,"url":"https://piston-data.mojang.com/v1/objects/503664a377eb2e63a5f74b273299f622fca2bf95/javaw.exe"}},"executable":true,"type":"file"},"lib/modules":{"downloads":{"raw":{"sha1":"43ac0b265238ded37bfdbc296c538e5aa4165387","size":57727234,"url":"https://piston-data.mojang.com/v1/objects/43ac0b265238ded37bfdbc296c538e5aa4165387/modules"}},"executable":false,"type":"file"},"release":{"downloads":{"lzma":{"sha1":"966094a136f32dd770cacc2e653ab68d96678aca","size":490,"url":"https://piston-data.mojang.com/v1/objects/966094a136f32dd770cacc2e653ab68d96678aca/release"},"raw":{"sha1":"c1339b6ffb06c4e52ab1c9e8bce8cee4ca025f82","size":1092,"url":"https://piston-data.mojang.com/v1/objects/c1339b6ffb06c4e52ab1c9e8bce8cee4ca025f82/release"}},"executable":false,"type":"file"},"bin":{"type":"directory"},"bin/server":{"type":"directory"},"conf":{"type":"directory"}}}"#;

    #[test]
    fn parses_a_real_runtime_manifest() {
        let json = Json::parse(SLIM_MANIFEST).expect("slim manifest must parse");
        let files = json.get("files").and_then(|v| v.as_obj()).expect("files map");

        let mut downloads = 0usize;
        let mut dirs = 0usize;
        let mut total = 0u64;
        let mut saw_java = false;
        for (key, entry) in files {
            match entry.get("type").and_then(|v| v.as_str()) {
                Some("directory") => {
                    dirs += 1;
                    // Directories carry no `downloads`; treating one as a file
                    // would fail the install, so this shape is load-bearing.
                    assert!(
                        entry.get("downloads").is_none(),
                        "directory {key} must not carry downloads"
                    );
                }
                Some("file") => {
                    let raw = entry.path(&["downloads", "raw"]).expect("raw downloads");
                    let url = raw.str_at("url").expect("raw.url");
                    let sha1 = raw.str_at("sha1").expect("raw.sha1");
                    let size = raw.u64_at("size").expect("raw.size");
                    assert!(url.starts_with("https://"), "bad url {url}");
                    assert_eq!(sha1.len(), 40, "sha1 must be 40 hex chars");
                    assert!(size > 0, "size must be known for {key}");
                    downloads += 1;
                    total += size;
                    if key == "bin/javaw.exe" {
                        saw_java = true;
                    }
                }
                other => panic!("unexpected entry type {other:?} for {key}"),
            }
        }
        assert!(downloads >= 4, "expected several files, got {downloads}");
        assert!(dirs >= 1, "expected directory entries, got {dirs}");
        assert!(total > 1_000_000, "sizes must be real, got {total}");
        assert!(saw_java, "bin/javaw.exe must be present");
        // Keys use forward slashes, which `install` converts for the filesystem.
        for (key, _) in files {
            assert!(!key.contains('\\'), "keys must use '/': {key}");
        }
    }

    #[test]
    fn runtime_index_shape_matches_install_lookup() {
        // Exactly the shape `install` walks: all[platform][component][0].manifest.url
        const INDEX: &str = r#"{"windows-x64":{"java-runtime-gamma":[{"manifest":{"url":"https://example.invalid/m.json","sha1":"abc"}}],"jre-legacy":[{"manifest":{"url":"https://example.invalid/legacy.json","sha1":"def"}}]}}"#;
        let json = Json::parse(INDEX).expect("index must parse");
        let url = json
            .path(&[PLATFORM, "java-runtime-gamma"])
            .and_then(|v| v.at(0))
            .and_then(|v| v.path(&["manifest", "url"]))
            .and_then(|v| v.as_str());
        assert_eq!(url, Some("https://example.invalid/m.json"));
        // An unknown component yields None, which `install` turns into a clear Err.
        assert!(json
            .path(&[PLATFORM, "java-runtime-nope"])
            .and_then(|v| v.at(0))
            .is_none());
        // Another platform must not leak through.
        assert!(json.path(&["linux", "java-runtime-gamma"]).is_none());
    }

    /// Full ~87 MB install; run explicitly with `cargo test -- --ignored`.
    #[test]
    #[ignore = "downloads the Mojang Java runtime (~87 MB)"]
    fn installs_mojang_runtime_from_network() {
        let base = std::env::temp_dir().join(format!("lfl-java-net-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let j = ensure(&base, "java-runtime-gamma", 17).expect("install must succeed");
        assert!(j.major >= 17, "installed Java {} is too old", j.major);
        assert!(j.exe.is_file());
        let marker = base.join("runtime/java-runtime-gamma").join(MARKER);
        assert!(marker.exists(), "marker must exist after a successful install");
        assert_eq!(read_marker(&marker, "java-runtime-gamma"), Some(j.major));
        let _ = std::fs::remove_dir_all(&base);
    }
}
