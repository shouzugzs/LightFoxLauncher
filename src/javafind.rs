// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025 LightFoxLauncher contributors
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Deep Java discovery for the 设置 page.
//!
//! `java::find_system` only looks at `%JAVA_HOME%` and `PATH`, which misses the
//! very common case of a JDK installed by an installer that never touched either.
//! This module scans the places JDKs actually land on Windows, probes everything
//! it finds, and returns a de-duplicated, version-sorted list so the UI can offer
//! a real choice instead of asking the user to paste a path.
//!
//! OWNER: lead.

use std::path::{Path, PathBuf};

use crate::java;

/// One discovered runtime.
#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    /// `javaw.exe` when present, else `java.exe`.
    pub exe: PathBuf,
    /// JRE/JDK root.
    pub home: PathBuf,
    /// Detected major version.
    pub major: u32,
    /// Where it came from, for display: `JAVA_HOME`, `PATH`, registry, …
    pub source: String,
    /// `true` for a runtime this launcher installed under `<data>/runtime/…`.
    pub managed: bool,
}

impl Found {
    /// `17 (C:\Program Files\Java\jdk-17)` for display.
    pub fn label(&self) -> String {
        format!("Java {} — {}", self.major, self.home.display())
    }
}

/// Registry keys that record installed JDKs/JREs.
///
/// Read through `reg.exe` rather than the registry API: one subprocess call
/// replaces a dozen `RegOpenKeyEx`/`RegEnumKeyEx` bindings, and this runs at most
/// once per scan.
///
/// Both 32- and 64-bit views are queried: a 32-bit JRE registers under WOW6432Node
/// even on a 64-bit machine, and a JDK installed by an MSI lands in the plain
/// `JavaSoft` tree. `JDK`/`JRE` are the vendor-neutral keys Oracle's installer
/// writes; the long-form names are what older installers used.
const REG_KEYS: [&str; 7] = [
    r"HKLM\SOFTWARE\JavaSoft\JDK",
    r"HKLM\SOFTWARE\JavaSoft\JRE",
    r"HKLM\SOFTWARE\JavaSoft\Java Runtime Environment",
    r"HKLM\SOFTWARE\JavaSoft\Java Development Kit",
    r"HKLM\SOFTWARE\WOW6432Node\JavaSoft\JDK",
    r"HKLM\SOFTWARE\WOW6432Node\JavaSoft\JRE",
    r"HKLM\SOFTWARE\WOW6432Node\JavaSoft\Java Runtime Environment",
];

/// Per-user Java installs (an MSI run without elevation registers here).
const REG_KEYS_HKCU: [&str; 3] = [
    r"HKCU\SOFTWARE\JavaSoft\JDK",
    r"HKCU\SOFTWARE\JavaSoft\JRE",
    r"HKCU\SOFTWARE\JavaSoft\Java Runtime Environment",
];

/// Vendor sub-directory names under a Program Files root.
///
/// Vendors disagree on the folder name (`Java`, `Eclipse Adoptium`, `Zulu`, …),
/// and an installer can be told to use any of them, so each known spelling is
/// tried. Everything found is still *verified* by probing `bin/java.exe`, so a
/// folder that merely shares a name costs one `stat`.
const VENDOR_DIRS: [&str; 16] = [
    "Java",
    "Eclipse Adoptium",
    "Eclipse Foundation",
    "AdoptOpenJDK",
    "Amazon Corretto",
    "Zulu",
    "Microsoft",
    "Microsoft\\jdk",
    "Microsoft\\OpenJDK",
    "BellSoft",
    "BellSoft\\Liberica",
    "Semeru",
    "IBM\\Semeru",
    "JetBrains",
    "Android",
    "RedHat\\java",
];

/// Well-known install roots, including the layouts used by common vendors.
///
/// The scan is deliberately *shallow and bounded*: it only reads a fixed list of
/// directories that Java installers are known to use, never walks the disk. A
/// full-disk scan would spend minutes on I/O for every user and find nothing a
/// targeted list misses.
fn vendor_roots() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    let mut push = |p: PathBuf| {
        if p.is_dir() && !roots.contains(&p) {
            roots.push(p);
        }
    };
    for base in ["ProgramFiles", "ProgramFiles(x86)", "ProgramW6432"] {
        if let Some(b) = crate::util::env(base) {
            let b = PathBuf::from(b);
            for dir in VENDOR_DIRS {
                push(b.join(dir));
            }
            // JetBrains and Android Studio ship their own JBR.
            push(b.join("JetBrains").join("Runtime"));
            push(
                b.join("Android")
                    .join("Android Studio")
                    .join("jbr"),
            );
            // A portable JDK is often unpacked straight into a tool folder.
            push(b.join("Java"));
        }
    }
    if let Some(local) = crate::util::env("LOCALAPPDATA") {
        let l = PathBuf::from(local);
        for dir in ["Eclipse Adoptium", "Programs\\Eclipse Adoptium", "Microsoft", "Zulu"] {
            push(l.join(dir));
        }
        // JetBrains Toolbox keeps every runtime under apps\<product>\<build>.
        push(l.join("JetBrains").join("Toolbox").join("apps"));
        // Android Studio's default install.
        push(
            l.join("Programs")
                .join("Android Studio")
                .join("jbr"),
        );
    }
    if let Some(home) = crate::util::home_dir() {
        // IntelliJ's "download JDK" and Gradle's toolchain cache.
        push(home.join(".jdks"));
        push(home.join(".gradle").join("jdks"));
        // scoop / chocolatey put their Java shims in one place each.
        push(home.join("scoop").join("apps"));
        push(
            crate::util::env("ChocolateyInstall")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join("scoop")),
        );
        push(home.join(".m2").join("wrapper").join("dists"));
    }
    if let Some(sdk) = crate::util::env("USERPROFILE") {
        push(PathBuf::from(sdk).join(".gradle").join("jdks"));
    }
    // A developer's own checkout is the most common place a JDK hides when no
    // installer was ever run.
    for d in ["D:", "E:"] {
        if std::path::Path::new(&format!("{d}\\")).exists() {
            push(PathBuf::from(format!("{d}\\jdk")));
            push(PathBuf::from(format!("{d}\\java")));
            push(PathBuf::from(format!("{d}\\Java")));
        }
    }
    roots
}

/// Read `JavaHome` values out of the JavaSoft registry keys.
fn registry_homes() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for key in REG_KEYS.iter().chain(REG_KEYS_HKCU.iter()) {
        let output = std::process::Command::new("reg.exe")
            .args(["query", key, "/s", "/v", "JavaHome"])
            .creation_flags_quiet()
            .output();
        let text = match output {
            Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
            _ => continue,
        };
        for line in text.lines() {
            // `    JavaHome    REG_SZ    C:\Program Files\Java\jdk-17`
            let mut parts = line.split_whitespace();
            if parts.next().map(|t| t.eq_ignore_ascii_case("JavaHome")).unwrap_or(false) {
                let rest: Vec<&str> = line.splitn(3, "REG_SZ").collect();
                if rest.len() == 2 {
                    let p = rest[1].trim().trim_matches('"');
                    if !p.is_empty() {
                        out.push(PathBuf::from(p));
                    }
                }
            }
        }
    }
    out
}

/// `creation_flags(CREATE_NO_WINDOW)` so scanning never flashes console windows.
trait Quiet {
    fn creation_flags_quiet(&mut self) -> &mut Self;
}

impl Quiet for std::process::Command {
    fn creation_flags_quiet(&mut self) -> &mut Self {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            self.creation_flags(0x0800_0000);
        }
        self
    }
}

/// Candidate executable paths under an install root.
fn exes_under(home: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let bin = home.join("bin");
    for name in ["javaw.exe", "java.exe"] {
        let p = bin.join(name);
        if p.is_file() {
            out.push(p);
        }
    }
    out
}

/// Look for JRE/JDK homes under an install root, at most `depth` levels deep.
///
/// Bounded on purpose: the roots are specific (`Program Files\Java`, a Toolbox
/// `apps` folder), so two levels cover `jdk-17` and `apps\<product>\<build>`
/// while a stray `bin/java.exe` three levels down is left alone.
///
/// The name filter applies to *every* directory, not just the ones descended
/// into: a folder that contains `bin/java.exe` is only reported when its name
/// says it is a runtime. Otherwise scanning a root that happens to contain a
/// bundled Java (a game, a tool with an embedded JRE) would report it as an
/// installed JDK, which is exactly the noise the scan is meant to avoid.
fn scan_root(root: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    if depth == 0 {
        return;
    }
    let rd = match std::fs::read_dir(root) {
        Ok(r) => r,
        Err(_) => return,
    };
    for e in rd.flatten() {
        if !e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let p = e.path();
        let plausible = could_hold_jdk(&p);
        if plausible && has_java(&p) {
            out.push(p);
        } else if plausible {
            scan_root(&p, out, depth - 1);
        }
    }
}

/// `true` when `home` looks like a JRE/JDK root (`bin/java.exe` or `bin/javaw.exe`).
fn has_java(home: &Path) -> bool {
    home.join("bin").join("java.exe").is_file() || home.join("bin").join("javaw.exe").is_file()
}

/// Every `java.exe` the current PATH resolves to, via `where.exe`.
///
/// A process's own PATH snapshot is what this launcher inherited at launch, so a
/// JDK installed *after* the launcher started (or from a console whose PATH was
/// extended) would otherwise be invisible. `where` asks Windows instead.
fn where_java() -> Vec<PathBuf> {
    let output = std::process::Command::new("where.exe")
        .arg("java")
        .creation_flags_quiet()
        .output();
    let text = match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
        _ => return Vec::new(),
    };
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(PathBuf::from)
        .collect()
}

/// Whether a directory *name* is worth descending into.
///
/// JetBrains Toolbox keeps runtimes at `apps\<product>\<build>\jbr`, so the
/// middle level is a product name, not a JDK folder. The list is deliberately
/// generous — anything that mentions a runtime family — because a false
/// positive costs one directory read and one `stat`, while a false negative
/// hides a perfectly usable JDK.
fn could_hold_jdk(dir: &Path) -> bool {
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if name.is_empty() {
        return false;
    }
    const WANT: [&str; 20] = [
        "jdk", "jre", "jbr", "java", "openjdk", "adoptium", "corretto", "zulu", "liberica",
        "semeru", "runtime", "apps", "idea", "studio", "jetbrains", "toolbox", "gradle",
        "sdkman", "jvm", "bellsoft",
    ];
    WANT.iter().any(|w| name.contains(w))
        || name.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false)
}

/// Probe one candidate root; `None` when it is not a usable runtime.
fn probe_root(home: &Path, source: &str, managed: bool) -> Option<Found> {
    for exe in exes_under(home) {
        let probe_target = exe.with_file_name("java.exe");
        let target = if probe_target.is_file() { probe_target } else { exe.clone() };
        if let Ok(major) = java::probe(&target) {
            return Some(Found {
                exe,
                home: home.to_path_buf(),
                major,
                source: source.to_string(),
                managed,
            });
        }
    }
    None
}

/// Every Java runtime this machine has, best-effort, sorted newest first.
///
/// Includes runtimes managed by this launcher (`<data>/runtime/<component>`), so
/// the list is a complete picture rather than only the system ones.
pub fn scan_all(data_dir: &Path) -> Vec<Found> {
    let mut found: Vec<Found> = Vec::new();

    // 1. Managed runtimes installed by this launcher.
    let runtime_root = data_dir.join("runtime");
    if let Ok(rd) = std::fs::read_dir(&runtime_root) {
        for e in rd.flatten() {
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                if let Some(f) = probe_root(&e.path(), "本启动器安装", true) {
                    found.push(f);
                }
            }
        }
    }

    // 2. JAVA_HOME.
    if let Some(h) = crate::util::env("JAVA_HOME") {
        if let Some(f) = probe_root(&PathBuf::from(h), "JAVA_HOME", false) {
            found.push(f);
        }
    }

    // 3. PATH entries and the parent of each (a JDK's `bin` is on PATH).
    //
    // `where.exe` is asked first: it resolves the PATH the *shell* would use,
    // which includes entries a stale in-process snapshot misses.
    for exe in where_java() {
        let home = exe.parent().and_then(Path::parent).map(Path::to_path_buf);
        if let Some(h) = home {
            if let Some(f) = probe_root(&h, "PATH", false) {
                found.push(f);
            }
        }
    }
    if let Some(path) = crate::util::env("PATH") {
        for dir in path.split(';') {
            let dir = dir.trim().trim_matches('"');
            if dir.is_empty() {
                continue;
            }
            let d = PathBuf::from(dir);
            if d.join("java.exe").is_file() || d.join("javaw.exe").is_file() {
                let home = d.parent().map(Path::to_path_buf).unwrap_or_else(|| d.clone());
                if let Some(f) = probe_root(&home, "PATH", false) {
                    found.push(f);
                }
            }
        }
    }

    // 4. Registry.
    for home in registry_homes() {
        if let Some(f) = probe_root(&home, "注册表", false) {
            found.push(f);
        }
    }

    // 5. Vendor install roots, up to three levels deep.
    //
    // Three levels cover `Program Files\Java\jdk-17` (1) and the Toolbox layout
    // `…\JetBrains\Toolbox\apps\<product>\<build>` (3). The name filter keeps the
    // walk from wandering: a directory is only entered when its name says it
    // could hold a runtime, so the read set stays small even at depth 3.
    for root in vendor_roots() {
        let mut homes = Vec::new();
        if has_java(&root) {
            homes.push(root.clone());
        } else {
            scan_root(&root, &mut homes, 3);
        }
        for h in homes {
            if let Some(f) = probe_root(&h, "安装目录", false) {
                found.push(f);
            }
        }
    }

    // De-duplicate by canonicalised home, keeping the first (best) source.
    let mut seen: Vec<PathBuf> = Vec::new();
    found.retain(|f| {
        let key = std::fs::canonicalize(&f.home).unwrap_or_else(|_| f.home.clone());
        if seen.contains(&key) {
            false
        } else {
            seen.push(key);
            true
        }
    });
    found.sort_by(|a, b| b.major.cmp(&a.major).then_with(|| a.home.cmp(&b.home)));
    found
}

/// The best runtime with `major >= need`, preferring the managed install.
pub fn best_for(found: &[Found], need: u32) -> Option<&Found> {
    found
        .iter()
        .filter(|f| f.major >= need)
        .min_by(|a, b| {
            // Closest sufficient major first (a JDK 17 beats a JDK 21 for MC 1.20),
            // then the managed one, then path order.
            a.major
                .cmp(&b.major)
                .then_with(|| b.managed.cmp(&a.managed))
                .then_with(|| a.home.cmp(&b.home))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn best_for_prefers_the_closest_sufficient_major() {
        let mk = |major: u32, managed: bool, path: &str| Found {
            exe: PathBuf::from(path).join("bin").join("javaw.exe"),
            home: PathBuf::from(path),
            major,
            source: "test".to_string(),
            managed,
        };
        let list = vec![mk(8, false, "a"), mk(17, false, "b"), mk(21, false, "c")];
        assert_eq!(best_for(&list, 17).map(|f| f.major), Some(17));
        assert_eq!(best_for(&list, 21).map(|f| f.major), Some(21));
        assert_eq!(best_for(&list, 25).map(|f| f.major), None);
        assert_eq!(best_for(&list, 8).map(|f| f.major), Some(8));
    }

    #[test]
    fn best_for_prefers_managed_on_a_tie() {
        let mk = |managed: bool, path: &str| Found {
            exe: PathBuf::from(path).join("bin").join("javaw.exe"),
            home: PathBuf::from(path),
            major: 17,
            source: "test".to_string(),
            managed,
        };
        let list = vec![mk(false, "system"), mk(true, "managed")];
        assert_eq!(best_for(&list, 17).map(|f| f.home.clone()), Some(PathBuf::from("managed")));
    }

    #[test]
    fn label_mentions_version_and_path() {
        let f = Found {
            exe: PathBuf::from(r"C:\jdk\bin\javaw.exe"),
            home: PathBuf::from(r"C:\jdk"),
            major: 17,
            source: "test".to_string(),
            managed: false,
        };
        let l = f.label();
        assert!(l.contains("17"), "{l}");
        assert!(l.contains(r"C:\jdk"), "{l}");
    }

    #[test]
    fn scanning_never_panics_on_a_missing_data_dir() {
        let missing = std::env::temp_dir().join("lfl-java-scan-does-not-exist");
        let _ = std::fs::remove_dir_all(&missing);
        // Must return (possibly empty), never panic.
        let _ = scan_all(&missing);
    }

    #[test]
    fn has_java_needs_the_bin_executable() {
        let base = std::env::temp_dir().join(format!("lfl-jf-has-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        // An empty directory is not a runtime.
        std::fs::create_dir_all(&base).unwrap();
        assert!(!has_java(&base));
        // `bin/` alone is not enough; the executable must be there.
        std::fs::create_dir_all(base.join("bin")).unwrap();
        assert!(!has_java(&base));
        std::fs::write(base.join("bin").join("java.exe"), b"x").unwrap();
        assert!(has_java(&base));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn could_hold_jdk_accepts_runtime_names_only() {
        for yes in [
            "jdk-17",
            "jdk-17.0.1+12",
            "jre1.8.0_361",
            "jbr",
            "Corretto",
            "zulu21",
            "apps",
            "21",
        ] {
            assert!(could_hold_jdk(Path::new(yes)), "{yes} should be descended into");
        }
        for no in ["documents", "photos", "games", "node_modules", ".git", "temp"] {
            assert!(!could_hold_jdk(Path::new(no)), "{no} should be skipped");
        }
    }

    #[test]
    fn scan_root_is_bounded_and_finds_nested_runtimes() {
        let root = std::env::temp_dir().join(format!("lfl-jf-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        // A Toolbox-shaped layout: apps\<product>\jbr-21\bin\java.exe. The product
        // level is named like a runtime (`apps` matches), so the walk descends.
        let deep = root.join("apps").join("java-runtime").join("jbr-21").join("bin");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("java.exe"), b"x").unwrap();
        // A sibling that must be ignored: its name says nothing about Java, even
        // though it happens to contain a bundled java.exe.
        let junk = root.join("apps").join("games").join("bin");
        std::fs::create_dir_all(&junk).unwrap();
        std::fs::write(junk.join("java.exe"), b"x").unwrap();

        let mut homes = Vec::new();
        scan_root(&root, &mut homes, 3);
        let names: Vec<String> = homes
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert!(names.contains(&"jbr-21".to_string()), "{names:?}");
        // The `games` folder holds a java.exe but is not a runtime by name, so
        // it is not reported as an installed JDK.
        assert!(!homes.iter().any(|p| p.ends_with("games")), "{homes:?}");
        let _ = std::fs::remove_dir_all(&root);
    }
}
