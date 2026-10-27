// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025 LightFoxLauncher contributors
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Library downloads, native-jar extraction and classpath assembly.
//! OWNER: meta. Public API frozen by `docs/CONTRACT.md`.
//!
//! Supported native-jar declarations — all three shapes occur in real data:
//!
//! 1. **legacy classifiers** (`downloads.classifiers` + `natives` map), e.g.
//!    `org.lwjgl.lwjgl:lwjgl-platform:2.9.4` (1.8.9). Only the classifier jar is
//!    native; a `downloads.artifact` on the same entry stays on the classpath.
//! 2. **legacy `natives` map with `${arch}`**, e.g. `tv.twitch:twitch-platform:6.5`
//!    → classifier key `natives-windows-64`.
//! 3. **modern separate entries** (1.19+): the natives bundle is its own library
//!    whose `downloads.artifact` is the DLL jar and whose maven name ends with
//!    `:natives-windows`. The sibling `:natives-windows-arm64` / `-x86` entries
//!    only test `os.name` in their rules, so the arch filter must look at the
//!    name/path instead (see `is_ours`).
//!
//! Classpath policy: artifact jars in library order, then the client jar last.
//! Native jars are **not** on the classpath — the JVM gets them via
//! `-Djava.library.path=<natives_dir>`, and LWJGL native jars contain no classes.

use std::path::{Path, PathBuf};

use crate::net::{self, Download as NetDownload};
use crate::util::{self, file_size, mkdirs, os_arch, Res};
use crate::version::{self, Download, Features, Library, Version};
use crate::zip;

/// Marker recording which native jars were extracted into `natives_dir`.
const NATIVES_MARKER: &str = ".lfl-natives-ok";
/// Cap on a single `net::download_many` call (bounds peak memory per batch).
const BATCH: usize = 64;

#[derive(Debug, Clone)]
pub struct Classpath {
    /// Library jars in JSON order, client jar last. No native jars.
    pub entries: Vec<PathBuf>,
    pub natives_dir: PathBuf,
    pub client_jar: PathBuf,
}

/// `<dir>/libraries/<artifact.path>`
///
/// `d.path` comes from the version JSON, which is downloaded. It must be
/// resolved as a *relative* path under `libraries/`: `Path::join` would let an
/// absolute path (or one containing `..`) replace or escape the base entirely.
fn library_path(dir: &Path, d: &Download) -> PathBuf {
    let base = dir.join("libraries");
    match util::safe_join(&base, &d.path) {
        Ok(p) => p,
        Err(e) => {
            // Keep the signature infallible: fall back to a path inside
            // `libraries/` that cannot collide, so the download fails its sha1
            // check instead of writing outside the tree.
            util::info(&format!("libraries: refusing unsafe artifact path: {e}"));
            base.join("invalid-artifact")
        }
    }
}

/// `<dir>/versions/<id>/natives`
pub fn natives_dir(dir: &Path, id: &str) -> PathBuf {
    version::version_dir_or_fallback(dir, id).join("natives")
}

/// The `natives` arch suffix matching this build: `"64"` on x64, `"32"` on x86.
fn legacy_arch() -> &'static str {
    if os_arch() == "x86" {
        "32"
    } else {
        "64"
    }
}

/// For a string mentioning `natives-windows`, the suffix after it
/// (`""` for the plain bundle, `"-arm64"`, `"-x86"`, `"-64"`, …).
fn windows_native_suffix(s: &str) -> Option<&str> {
    let stem = s.strip_suffix(".jar").unwrap_or(s);
    let i = stem.find("natives-windows")?;
    Some(&stem[i + "natives-windows".len()..])
}

/// Does this classifier/name/path denote the Windows natives bundle for *our*
/// arch? `natives-windows` and `natives-windows-64` qualify on x64; `-32`,
/// `-x86`, `-arm`, `-arm64` do not.
fn is_ours(s: &str) -> bool {
    match windows_native_suffix(s) {
        Some("") => true,
        Some(rest) => rest == format!("-{}", legacy_arch()),
        None => false,
    }
}

/// Name suffix of the maven classifier (`natives-windows`, `natives-linux`, …).
fn maven_classifier(name: &str) -> &str {
    name.rsplit(':').next().unwrap_or("")
}

/// Is this artifact *some* natives bundle (any OS/arch), rather than a class
/// jar? Modern version JSONs give every native its own library entry and mark
/// it via the artifact path (`.../foo-natives-windows.jar`) and the maven
/// classifier.
fn is_native_bundle(lib: &Library, art: &Download) -> bool {
    art.path.contains("-natives-") || maven_classifier(&lib.name).contains("natives-")
}

/// Is this artifact the natives bundle **for this OS and arch**?
fn is_our_native_bundle(lib: &Library, art: &Download) -> bool {
    is_ours(&art.path) || is_ours(maven_classifier(&lib.name))
}

/// Pick the native classifier jar of a legacy entry, if any.
fn native_classifier(lib: &Library) -> Option<&Download> {
    if lib.classifiers.is_empty() {
        return None;
    }
    // Preferred keys: legacy `natives["windows"]` with `${arch}` filled in
    // (`natives-windows-64`), then the bare `natives-windows`.
    let mut preferred: Vec<String> = Vec::new();
    for (os, val) in &lib.natives {
        if os.eq_ignore_ascii_case("windows") {
            preferred.push(val.replace("${arch}", legacy_arch()));
        }
    }
    if legacy_arch() == "64" {
        preferred.push("natives-windows-64".to_string());
    }
    preferred.push("natives-windows".to_string());

    for key in &preferred {
        if let Some((_, d)) = lib.classifiers.iter().find(|(k, _)| k == key) {
            if is_ours(key) {
                return Some(d);
            }
        }
    }
    // Last resort: any classifier that is unambiguously the Windows bundle.
    lib.classifiers
        .iter()
        .find(|(k, _)| is_ours(k))
        .map(|(_, d)| d)
}

/// Libraries that apply to this machine, split into classpath jars and natives.
struct Selection {
    /// Artifacts that belong on the classpath, in JSON order.
    jars: Vec<Download>,
    /// Natives to download + extract, with the entry's `extract.exclude` list.
    natives: Vec<(Download, Vec<String>)>,
}

fn select(v: &Version) -> Selection {
    let features = Features::default();
    let mut jars = Vec::new();
    let mut natives = Vec::new();
    for lib in &v.libraries {
        if !version::rules_allow(&lib.rules, &features) {
            continue;
        }
        // Native jars, shape 1/2: legacy `downloads.classifiers` (+ `natives`).
        if let Some(nc) = native_classifier(lib) {
            natives.push((nc.clone(), lib.extract_exclude.clone()));
        }
        if let Some(art) = &lib.artifact {
            if is_native_bundle(lib, art) {
                // Shape 3: the artifact *is* a natives bundle. Only this OS+arch
                // one is extracted; the arm64/x86 siblings are dropped entirely
                // (their rules merely say `os.name: windows`, so the arch test
                // has to live here).
                if is_our_native_bundle(lib, art) {
                    natives.push((art.clone(), lib.extract_exclude.clone()));
                }
            } else {
                jars.push(art.clone());
            }
        }
    }
    Selection { jars, natives }
}

/// `true` when `natives_dir` already holds a marker equal to `want`.
fn marker_matches(natives: &Path, want: &str) -> bool {
    match std::fs::read_to_string(natives.join(NATIVES_MARKER)) {
        Ok(have) => have == want,
        Err(_) => false,
    }
}

/// Drop every regular file from `natives` so a changed native set cannot leave
/// stale DLLs behind. Subdirectories (none are expected) are left alone.
fn clear_natives(natives: &Path) {
    let rd = match std::fs::read_dir(natives) {
        Ok(rd) => rd,
        Err(_) => return,
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_file() {
            let _ = std::fs::remove_file(&p);
        }
    }
}

/// Marker body: the sorted native jar file names, one per line.
fn marker_contents(natives: &[(Download, Vec<String>)]) -> String {
    let mut names: Vec<String> = natives
        .iter()
        .map(|(d, _)| {
            Path::new(&d.path)
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_else(|| d.path.clone())
        })
        .collect();
    names.sort();
    names.join("\n")
}

/// Download every rule-allowed library + native jar, extract natives into
/// `<dir>/versions/<id>/natives`, ensure the client jar exists and return the
/// ordered classpath.
pub fn prepare(dir: &Path, v: &Version) -> Res<Classpath> {
    if v.id.is_empty() {
        return Err("libraries: version has no id".to_string());
    }
    let sel = select(v);
    let natives_out = natives_dir(dir, &v.id);
    mkdirs(&natives_out)?;

    // ---- 1. downloads (artifacts, then native bundles) ---------------------
    let mut items: Vec<NetDownload> = Vec::with_capacity(sel.jars.len() + sel.natives.len());
    for d in sel.jars.iter().chain(sel.natives.iter().map(|(d, _)| d)) {
        items.push(NetDownload::new(&d.url, library_path(dir, d), &d.sha1, d.size));
    }
    for chunk in items.chunks(BATCH) {
        net::download_many(chunk).map_err(|e| format!("downloading libraries: {e}"))?;
    }

    // ---- 2. natives --------------------------------------------------------
    let want_marker = marker_contents(&sel.natives);
    if !marker_matches(&natives_out, &want_marker) {
        clear_natives(&natives_out);
        for (d, excludes) in &sel.natives {
            let jar = library_path(dir, d);
            let frags: Vec<&str> = excludes.iter().map(String::as_str).collect();
            zip::extract(&jar, &natives_out, &frags)
                .map_err(|e| format!("extracting natives from {}: {e}", jar.display()))?;
        }
        let marker = natives_out.join(NATIVES_MARKER);
        std::fs::write(&marker, &want_marker)
            .map_err(|e| format!("cannot write {}: {e}", marker.display()))?;
    }

    // ---- 3. client jar -----------------------------------------------------
    // `Version::jar` (mod loaders) overrides the file name; the parent's jar is
    // what actually gets launched.
    let ver_dir = v.jar.clone().filter(|j| !j.is_empty()).unwrap_or_else(|| v.id.clone());
    let client_jar = version::version_dir_or_fallback(dir, &ver_dir).join(format!("{ver_dir}.jar"));
    if !v.client.url.is_empty() {
        net::download(&v.client.url, &client_jar, &v.client.sha1, v.client.size)
            .map_err(|e| format!("downloading client jar for '{}': {e}", v.id))?;
    } else if file_size(&client_jar) == 0 {
        return Err(format!(
            "version '{}': missing client jar {} and the version JSON has no \
             downloads.client url",
            v.id,
            client_jar.display()
        ));
    }

    // ---- 4. classpath ------------------------------------------------------
    let mut entries: Vec<PathBuf> = Vec::with_capacity(sel.jars.len() + 1);
    for d in &sel.jars {
        entries.push(library_path(dir, d));
    }
    entries.push(client_jar.clone());
    Ok(Classpath {
        entries,
        natives_dir: natives_out,
        client_jar,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::version::{AssetIndex, Download as VDownload, Rule};

    fn dl(path: &str) -> VDownload {
        VDownload {
            url: format!("https://libraries.minecraft.net/{path}"),
            path: path.to_string(),
            sha1: "0123456789abcdef0123456789abcdef01234567".to_string(),
            size: 42,
        }
    }

    fn lib(name: &str, artifact: Option<VDownload>) -> Library {
        Library {
            name: name.to_string(),
            artifact,
            ..Library::default()
        }
    }

    fn rule(action: &str, os: Option<&str>) -> Rule {
        Rule {
            action: action.to_string(),
            os_name: os.map(str::to_string),
            os_arch: None,
            os_version: None,
            features: Vec::new(),
        }
    }

    fn classifier(k: &str, path: &str) -> (String, VDownload) {
        (k.to_string(), dl(path))
    }

    fn test_dir(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "lfl-meta-lib-{tag}-{}-{}",
            std::process::id(),
            crate::util::now_ms()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn artifact_path_mapping() {
        let dir = Path::new("D:/LFL");
        let d = dl("org/lwjgl/lwjgl/3.3.1/lwjgl-3.3.1.jar");
        assert_eq!(
            library_path(dir, &d),
            Path::new("D:/LFL/libraries/org/lwjgl/lwjgl/3.3.1/lwjgl-3.3.1.jar")
        );
    }

    #[test]
    fn selects_plain_artifacts_and_skips_rule_denied() {
        let mut osx_only = lib("b:b:1", Some(dl("b/b/1/b-1.jar")));
        osx_only.rules = vec![rule("allow", Some("osx"))];
        let v = Version {
            id: "1.20.1".into(),
            libraries: vec![
                lib("a:a:1", Some(dl("a/a/1/a-1.jar"))),
                osx_only,
                lib("c:c:1", Some(dl("c/c/1/c-1.jar"))),
            ],
            ..Version::default()
        };
        let sel = select(&v);
        let paths: Vec<&str> = sel.jars.iter().map(|d| d.path.as_str()).collect();
        assert_eq!(paths, vec!["a/a/1/a-1.jar", "c/c/1/c-1.jar"]);
        assert!(sel.natives.is_empty());
    }

    #[test]
    fn native_classifier_selection_x64() {
        // jinput style: bare `natives-windows` plus 32/64-bit siblings, and a
        // `${arch}` natives map => the -64 classifier must win.
        let mut l = lib("net.java.jinput:jinput-platform:2.0.5", None);
        l.classifiers = vec![
            classifier("natives-linux", "p/jinput-platform-2.0.5-natives-linux.jar"),
            classifier("natives-windows", "p/jinput-platform-2.0.5-natives-windows.jar"),
            classifier("natives-windows-32", "p/jinput-platform-2.0.5-natives-windows-32.jar"),
            classifier("natives-windows-64", "p/jinput-platform-2.0.5-natives-windows-64.jar"),
        ];
        l.natives = vec![
            ("linux".into(), "natives-linux".into()),
            ("windows".into(), "natives-windows-${arch}".into()),
        ];
        assert!(native_classifier(&l).unwrap().path.ends_with("natives-windows-64.jar"));
        assert_eq!(legacy_arch(), "64");

        // lwjgl-platform (1.8.9): natives map has no ${arch} => bare classifier.
        let mut l = lib("org.lwjgl.lwjgl:lwjgl-platform:2.9.4-nightly-20150209", None);
        l.classifiers = vec![
            classifier("natives-osx", "x/osx.jar"),
            classifier("natives-windows", "x/windows.jar"),
            classifier("natives-linux", "x/linux.jar"),
        ];
        l.natives = vec![
            ("linux".into(), "natives-linux".into()),
            ("osx".into(), "natives-osx".into()),
            ("windows".into(), "natives-windows".into()),
        ];
        assert!(native_classifier(&l).unwrap().path.ends_with("windows.jar"));

        // tv.twitch style: only -32/-64 keys exist.
        let mut l = lib("tv.twitch:twitch-external-platform:4.5", None);
        l.classifiers = vec![
            classifier("natives-windows-32", "x/w32.jar"),
            classifier("natives-windows-64", "x/w64.jar"),
        ];
        l.natives = vec![("windows".into(), "natives-windows-${arch}".into())];
        assert!(native_classifier(&l).unwrap().path.ends_with("w64.jar"));
    }

    #[test]
    fn native_classifier_rejects_other_oses() {
        // 1.20.1-style entry listing only linux/osx classifiers.
        let mut l = lib(
            "org.lwjgl:lwjgl:3.3.1",
            Some(dl("org/lwjgl/lwjgl/3.3.1/lwjgl-3.3.1.jar")),
        );
        l.classifiers = vec![
            classifier("natives-linux", "l.jar"),
            classifier("natives-macos", "m.jar"),
        ];
        l.natives = vec![("linux".into(), "natives-linux".into())];
        assert!(native_classifier(&l).is_none());
        let sel = select(&Version {
            id: "x".into(),
            libraries: vec![l],
            ..Version::default()
        });
        assert_eq!(sel.jars.len(), 1, "class jar stays on the classpath");
        assert!(sel.natives.is_empty());
    }

    #[test]
    fn modern_native_detection_by_name_and_path() {
        // 1.20.1: artifact is the natives jar, name ends with :natives-windows
        let l = lib(
            "org.lwjgl:lwjgl:3.3.1:natives-windows",
            Some(dl("org/lwjgl/lwjgl/3.3.1/lwjgl-3.3.1-natives-windows.jar")),
        );
        assert!(is_our_native_bundle(&l, l.artifact.as_ref().unwrap()));
        // path-only detection
        let l2 = lib(
            "org.lwjgl:lwjgl:3.3.1",
            Some(dl("org/lwjgl/lwjgl/3.3.1/lwjgl-3.3.1-natives-windows.jar")),
        );
        assert!(is_our_native_bundle(&l2, l2.artifact.as_ref().unwrap()));
        // arm64 / x86 siblings must NOT be picked on x64
        for bad in [
            "org/lwjgl/lwjgl/3.3.1/lwjgl-3.3.1-natives-windows-arm64.jar",
            "org/lwjgl/lwjgl/3.3.1/lwjgl-3.3.1-natives-windows-x86.jar",
            "org/lwjgl/lwjgl/3.3.1/lwjgl-3.3.1-natives-windows-32.jar",
            "org/lwjgl/lwjgl/3.3.1/lwjgl-3.3.1-natives-linux.jar",
            "org/lwjgl/lwjgl/3.3.1/lwjgl-3.3.1-natives-macos.jar",
        ] {
            let l = lib("org.lwjgl:lwjgl:3.3.1", Some(dl(bad)));
            assert!(!is_our_native_bundle(&l, l.artifact.as_ref().unwrap()), "{bad}");
        }
        let l = lib(
            "org.lwjgl:lwjgl:3.3.1:natives-windows-arm64",
            Some(dl("org/lwjgl/lwjgl/3.3.1/lwjgl-3.3.1-natives-windows-arm64.jar")),
        );
        assert!(!is_our_native_bundle(&l, l.artifact.as_ref().unwrap()));
        // a normal library is not a native
        let l = lib(
            "com.google.guava:guava:31.1-jre",
            Some(dl("com/google/guava/guava/31.1-jre/guava-31.1-jre.jar")),
        );
        assert!(!is_our_native_bundle(&l, l.artifact.as_ref().unwrap()));
        assert!(!is_native_bundle(&l, l.artifact.as_ref().unwrap()));
        // the arm64 sibling IS a native bundle, just not ours
        let l = lib(
            "org.lwjgl:lwjgl:3.3.1:natives-windows-arm64",
            Some(dl("org/lwjgl/lwjgl/3.3.1/lwjgl-3.3.1-natives-windows-arm64.jar")),
        );
        assert!(is_native_bundle(&l, l.artifact.as_ref().unwrap()));
        assert!(!is_our_native_bundle(&l, l.artifact.as_ref().unwrap()));
    }

    /// Live 1.20.1 shape: every native is its own library entry and the
    /// arm64/x86 siblings carry only `{"allow", os:{name:windows}}` rules — no
    /// `os.arch`. They must be dropped, not put on the classpath.
    #[test]
    fn foreign_arch_natives_are_dropped_entirely() {
        let win = || {
            vec![Rule {
                action: "allow".into(),
                os_name: Some("windows".into()),
                os_arch: None,
                os_version: None,
                features: Vec::new(),
            }]
        };
        let mk = |suffix: &str| {
            let mut l = lib(
                &format!("org.lwjgl:lwjgl:3.3.1:{suffix}"),
                Some(dl(&format!(
                    "org/lwjgl/lwjgl/3.3.1/lwjgl-3.3.1-{suffix}.jar"
                ))),
            );
            l.rules = win();
            l
        };
        let mut core = lib(
            "org.lwjgl:lwjgl:3.3.1",
            Some(dl("org/lwjgl/lwjgl/3.3.1/lwjgl-3.3.1.jar")),
        );
        core.rules = win();
        let v = Version {
            id: "1.20.1".into(),
            libraries: vec![
                core,
                mk("natives-windows"),
                mk("natives-windows-arm64"),
                mk("natives-windows-x86"),
            ],
            ..Version::default()
        };
        let sel = select(&v);
        assert_eq!(sel.jars.len(), 1, "only the class jar: {:?}", sel.jars);
        assert_eq!(sel.natives.len(), 1, "only x64 natives: {:?}", sel.natives);
        assert_eq!(
            sel.natives[0].0.path,
            "org/lwjgl/lwjgl/3.3.1/lwjgl-3.3.1-natives-windows.jar"
        );
    }

    #[test]
    fn select_modern_natives_are_not_on_classpath() {
        let v = Version {
            id: "1.20.1".into(),
            libraries: vec![
                lib(
                    "org.lwjgl:lwjgl:3.3.1",
                    Some(dl("org/lwjgl/lwjgl/3.3.1/lwjgl-3.3.1.jar")),
                ),
                lib(
                    "org.lwjgl:lwjgl:3.3.1:natives-windows",
                    Some(dl("org/lwjgl/lwjgl/3.3.1/lwjgl-3.3.1-natives-windows.jar")),
                ),
            ],
            ..Version::default()
        };
        let sel = select(&v);
        assert_eq!(sel.jars.len(), 1);
        assert_eq!(sel.natives.len(), 1);
        assert!(sel.natives[0].0.path.ends_with("-natives-windows.jar"));
    }

    #[test]
    fn select_carries_extract_exclude() {
        let mut l = lib("v:v:1", Some(dl("v/v/1/v-1-natives-windows.jar")));
        l.extract_exclude = vec!["META-INF/".into()];
        let sel = select(&Version {
            id: "x".into(),
            libraries: vec![l],
            ..Version::default()
        });
        assert_eq!(sel.natives.len(), 1);
        assert_eq!(sel.natives[0].1, vec!["META-INF/".to_string()]);
    }

    #[test]
    fn legacy_entry_with_artifact_and_classifiers_keeps_artifact_on_classpath() {
        let mut l = lib(
            "org.lwjgl.lwjgl:lwjgl-platform:2.9.4-nightly-20150209",
            Some(dl("org/lwjgl/lwjgl/lwjgl-platform/2.9.4-nightly-20150209/lwjgl-platform-2.9.4-nightly-20150209.jar")),
        );
        l.classifiers = vec![classifier(
            "natives-windows",
            "org/lwjgl/lwjgl/lwjgl-platform/2.9.4-nightly-20150209/lwjgl-platform-2.9.4-nightly-20150209-natives-windows.jar",
        )];
        l.natives = vec![("windows".into(), "natives-windows".into())];
        let sel = select(&Version {
            id: "1.8.9".into(),
            libraries: vec![l],
            ..Version::default()
        });
        assert_eq!(sel.jars.len(), 1);
        assert!(!sel.jars[0].path.contains("-natives-"));
        assert_eq!(sel.natives.len(), 1);
    }

    #[test]
    fn marker_contents_are_sorted_and_stable() {
        let a = (
            VDownload { path: "z/z/z-natives-windows.jar".into(), ..dl("z/z/z-natives-windows.jar") },
            Vec::new(),
        );
        let b = (
            VDownload { path: "a/a/a-natives-windows.jar".into(), ..dl("a/a/a-natives-windows.jar") },
            Vec::new(),
        );
        let one = marker_contents(&[a.clone(), b.clone()]);
        assert_eq!(one, "a-natives-windows.jar\nz-natives-windows.jar");
        assert_eq!(one, marker_contents(&[b, a]));
        assert_eq!(marker_contents(&[]), "");
    }

    #[test]
    fn marker_reuse_and_invalidation() {
        let dir = test_dir("marker");
        let natives = dir.join("natives");
        mkdirs(&natives).unwrap();
        assert!(!marker_matches(&natives, "a"));
        std::fs::write(natives.join(NATIVES_MARKER), "a").unwrap();
        assert!(marker_matches(&natives, "a"));
        assert!(!marker_matches(&natives, "a\nb"));
        std::fs::write(natives.join("old.dll"), b"x").unwrap();
        clear_natives(&natives);
        assert!(!natives.join("old.dll").exists(), "stale DLL removed");
        assert!(!natives.join(NATIVES_MARKER).exists(), "marker cleared too");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prepare_errors_when_client_jar_missing() {
        let dir = test_dir("noclient");
        let v = Version {
            id: "1.20.1".into(),
            ..Version::default()
        };
        let e = prepare(&dir, &v).unwrap_err();
        assert!(e.contains("missing client jar"), "{e}");
        assert!(e.contains("1.20.1"), "{e}");
        assert!(e.contains("1.20.1.jar"), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prepare_builds_classpath_and_natives_dir_offline() {
        let dir = test_dir("existing");
        let id = "1.20.1";
        let vdir = version::versions_dir(&dir).join(id);
        mkdirs(&vdir).unwrap();
        std::fs::write(vdir.join(format!("{id}.jar")), b"jar").unwrap();
        let v = Version {
            id: id.into(),
            libraries: Vec::new(),
            client: VDownload::default(), // no url => reuse the file on disk
            ..Version::default()
        };
        let cp = prepare(&dir, &v).unwrap();
        assert_eq!(cp.client_jar, vdir.join("1.20.1.jar"));
        assert_eq!(cp.entries, vec![vdir.join("1.20.1.jar")], "client jar last");
        assert_eq!(cp.natives_dir, vdir.join("natives"));
        assert!(cp.natives_dir.is_dir());
        // no natives => empty marker, so a second run skips work
        assert_eq!(
            std::fs::read_to_string(cp.natives_dir.join(NATIVES_MARKER)).unwrap(),
            ""
        );
        // second run must be a no-op and still succeed
        let cp2 = prepare(&dir, &Version { libraries: Vec::new(), ..v }).unwrap();
        assert_eq!(cp2.entries, cp.entries);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prepare_uses_jar_override_for_client_jar() {
        let dir = test_dir("jaroverride");
        let parent_dir = version::versions_dir(&dir).join("1.20.1");
        mkdirs(&parent_dir).unwrap();
        std::fs::write(parent_dir.join("1.20.1.jar"), b"parent jar").unwrap();
        let v = Version {
            id: "1.20.1-forge".into(),
            jar: Some("1.20.1".into()),
            client: VDownload::default(),
            ..Version::default()
        };
        let cp = prepare(&dir, &v).unwrap();
        assert_eq!(cp.client_jar, parent_dir.join("1.20.1.jar"));
        assert_eq!(cp.entries.last().unwrap(), &cp.client_jar);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prepare_rejects_empty_id() {
        let dir = test_dir("noid");
        let e = prepare(&dir, &Version::default()).unwrap_err();
        assert!(e.contains("no id"), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn asset_index_is_not_required_by_prepare() {
        // guard: prepare must not touch v.asset_index (assets::ensure owns it)
        let dir = test_dir("noassets");
        let vdir = version::versions_dir(&dir).join("1.0");
        mkdirs(&vdir).unwrap();
        std::fs::write(vdir.join("1.0.jar"), b"c").unwrap();
        let v = Version {
            id: "1.0".into(),
            asset_index: AssetIndex::default(),
            ..Version::default()
        };
        assert!(prepare(&dir, &v).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
