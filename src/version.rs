// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025 LightFoxLauncher contributors
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Version manifest, version-JSON parsing, rule evaluation and inheritance.
//! OWNER: meta. Public API frozen by `docs/CONTRACT.md`.
//!
//! Everything here is pure metadata handling: `load` downloads the version JSON
//! (and its parents' JSONs) but never the client jar — `libraries::prepare`
//! owns every binary download, so nothing is fetched twice.

use std::path::{Path, PathBuf};

use crate::json::Json;
use crate::net;
use crate::util::{self, mkdirs, Res};

/// Base URL for Maven-style library artifacts.
pub const LIBRARY_BASE: &str = "https://libraries.minecraft.net/";

const MANIFEST_URL: &str = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";
const MANIFEST_FILE: &str = "version_manifest_v2.json";
/// Re-use a cached manifest for 30 minutes before hitting the network again.
const MANIFEST_TTL_MS: u128 = 30 * 60 * 1000;
/// Hard stop for `inheritsFrom` chains (guards against cycles).
const MAX_DEPTH: usize = 8;

// ---------------------------------------------------------------------------
// Download / rules / arguments
// ---------------------------------------------------------------------------

/// A file referenced by a version JSON (`downloads.*` entries).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Download {
    pub url: String,
    /// Path relative to `<dir>/libraries` for library artifacts, `"<id>.jar"`
    /// for the client jar. Empty when the JSON only gave a `name`.
    pub path: String,
    pub sha1: String,
    pub size: u64,
}

/// One entry of a `rules` array.
#[derive(Debug, Clone)]
pub struct Rule {
    pub action: String,
    pub os_name: Option<String>,
    pub os_arch: Option<String>,
    pub os_version: Option<String>,
    pub features: Vec<(String, bool)>,
}

/// Launcher feature flags referenced by `rules[].features`.
#[derive(Debug, Clone, Copy, Default)]
pub struct Features {
    pub demo_user: bool,
    pub custom_resolution: bool,
    pub quick_play_support: bool,
    pub quick_play_singleplayer: bool,
    pub quick_play_multiplayer: bool,
    pub quick_play_realms: bool,
}

/// One element of `arguments.game` / `arguments.jvm`.
#[derive(Debug, Clone, Default)]
pub struct Arg {
    pub rules: Vec<Rule>,
    pub value: Vec<String>,
}

/// `assetIndex` object, or the legacy `assets` id.
#[derive(Debug, Clone, Default)]
pub struct AssetIndex {
    pub id: String,
    pub sha1: String,
    pub size: u64,
    pub total_size: u64,
    pub url: String,
}

/// `logging.client` (log4j config).
#[derive(Debug, Clone, Default)]
pub struct Logging {
    pub arg: String,
    pub id: String,
    pub url: String,
    pub sha1: String,
    pub size: u64,
}

/// One `libraries[]` entry.
#[derive(Debug, Clone, Default)]
pub struct Library {
    pub name: String,
    pub artifact: Option<Download>,
    /// `downloads.classifiers`; key is e.g. `"natives-windows"`.
    pub classifiers: Vec<(String, Download)>,
    /// Legacy `natives` map, e.g. `[("windows", "natives-windows-${arch}")]`.
    pub natives: Vec<(String, String)>,
    pub extract_exclude: Vec<String>,
    pub rules: Vec<Rule>,
}

/// A fully merged, launch-ready description of one game version.
#[derive(Debug, Clone, Default)]
pub struct Version {
    pub id: String,
    pub version_type: String,
    pub main_class: String,
    pub asset_index: AssetIndex,
    /// Legacy pre-1.7 asset index name (`"pre-1.6"`, `"legacy"`, ...).
    pub assets: Option<String>,
    pub libraries: Vec<Library>,
    pub game_args: Vec<Arg>,
    pub jvm_args: Vec<Arg>,
    /// Pre-1.13 `minecraftArguments`; the launcher splits it on whitespace.
    pub legacy_args: Option<String>,
    pub logging: Option<Logging>,
    pub client: Download,
    pub java_major: u32,
    pub java_component: String,
    pub inherits_from: Option<String>,
    /// `jar`: version id whose client jar this version runs on (mod loaders).
    pub jar: Option<String>,
}

impl Version {
    /// Path of the client jar this version launches, relative to `<dir>/versions`.
    pub fn client_jar_rel(&self) -> String {
        let id = self.jar.as_deref().unwrap_or(&self.id);
        format!("{id}/{id}.jar")
    }
}

// ---------------------------------------------------------------------------
// Rule evaluation
// ---------------------------------------------------------------------------

/// `true` when the rules allow this platform/feature combination.
///
/// Vanilla semantics: no rules at all means "allowed"; otherwise the *last*
/// matching rule wins and "no matching rule" means "not allowed".
pub fn rules_allow(rules: &[Rule], features: &Features) -> bool {
    if rules.is_empty() {
        return true;
    }
    let mut allow = false;
    for r in rules {
        if rule_matches(r, features) {
            allow = r.action != "disallow";
        }
    }
    allow
}

fn rule_matches(r: &Rule, features: &Features) -> bool {
    if let Some(want) = &r.os_name {
        if !want.eq_ignore_ascii_case(util::os_name()) {
            return false;
        }
    }
    if let Some(want) = &r.os_arch {
        if normalize_arch(want) != util::os_arch() {
            return false;
        }
    }
    if let Some(want) = &r.os_version {
        if !os_version_matches(want) {
            return false;
        }
    }
    for (name, want) in &r.features {
        match feature_value(features, name) {
            Some(have) if have == *want => {}
            _ => return false,
        }
    }
    true
}

/// Map the arch spellings found in real JSONs onto `util::os_arch()` values.
fn normalize_arch(a: &str) -> &'static str {
    match a.to_ascii_lowercase().as_str() {
        "x64" | "amd64" | "x86_64" | "x86-64" => "x64",
        "x86" | "i386" | "i686" | "x86-32" => "x86",
        "arm64" | "aarch64" => "arm64",
        _ => "other",
    }
}

/// `os.version` is a regex in real JSONs (e.g. `"^10\\.5\\.\\d$"`).
///
/// SIMPLIFICATION: we do not ship a regex engine. Anchors and escapes are
/// stripped and the rest is matched as a prefix/contains test against
/// `util::os_version()` (`"10.0"` on Windows 10/11). This is correct for every
/// rule Mojang ships that can match a Windows release: the old macOS-only
/// `^10\.5\.\d$` style rules simply do not match our `"10.0"`.
fn os_version_matches(pat: &str) -> bool {
    let os = util::os_version();
    let p: String = pat
        .chars()
        .filter(|c| !matches!(c, '^' | '$' | '\\'))
        .collect();
    if p.is_empty() {
        return true;
    }
    // Regex classes/quantifiers cannot be evaluated without a regex engine;
    // fall back to "does the literal prefix line up".
    if p.contains('[') || p.contains('(') || p.contains('|') {
        return os.starts_with(&p[..p.len().min(2)]);
    }
    let p = p.replace("\\d", "");
    let p = p.replace('d', "");
    if p.is_empty() {
        return true;
    }
    os.starts_with(&p) || p.starts_with(os) || os.contains(&p)
}

fn feature_value(f: &Features, name: &str) -> Option<bool> {
    Some(match name {
        "is_demo_user" => f.demo_user,
        "has_custom_resolution" => f.custom_resolution,
        "has_quick_plays_support" => f.quick_play_support,
        "is_quick_play_singleplayer" => f.quick_play_singleplayer,
        "is_quick_play_multiplayer" => f.quick_play_multiplayer,
        "is_quick_play_realms" => f.quick_play_realms,
        _ => return None,
    })
}

/// Keep only the arguments whose rules allow this platform, in JSON order.
/// Placeholders (`${classpath}`, …) are deliberately **not** expanded here.
pub fn resolve_args(args: &[Arg], features: &Features) -> Vec<String> {
    let mut out = Vec::new();
    for a in args {
        if rules_allow(&a.rules, features) {
            out.extend(a.value.iter().cloned());
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Version manifest
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ManifestEntry {
    pub id: String,
    /// `"release"`, `"snapshot"`, `"old_beta"`, `"old_alpha"`.
    pub kind: String,
    pub url: String,
    /// Additional fields (not part of the frozen 3-field shape) so `load` can
    /// verify the downloaded version JSON. Both are empty/0 for old manifests.
    pub sha1: String,
    pub size: u64,
}

impl ManifestEntry {
    pub fn new(id: &str, kind: &str, url: &str, sha1: &str, size: u64) -> Self {
        ManifestEntry {
            id: id.to_string(),
            kind: kind.to_string(),
            url: url.to_string(),
            sha1: sha1.to_string(),
            size,
        }
    }
}

/// `<dir>/versions`
pub fn versions_dir(dir: &Path) -> PathBuf {
    dir.join("versions")
}

/// `<dir>/versions/<id>`, refusing an id that is not a single safe component.
///
/// Version ids reach here from the command line, the version manifest, a loader
/// profile and the on-disk directory listing. Every one of those is outside the
/// launcher's control, and `versions_dir().join(id)` would happily resolve
/// `../../x` or `C:/Windows` to a path outside `versions/`. Callers that must
/// stay infallible use [`version_dir_or_fallback`].
pub fn version_dir(dir: &Path, id: &str) -> Res<PathBuf> {
    crate::util::safe_component(id)
        .map_err(|e| format!("版本 id {id:?} 不合法：{e}"))?;
    Ok(versions_dir(dir).join(id))
}

/// Like [`version_dir`], but never fails: an invalid id maps to a path inside
/// `versions/` that cannot collide with a real version, so a subsequent read or
/// download fails on its own terms instead of escaping the tree.
pub fn version_dir_or_fallback(dir: &Path, id: &str) -> PathBuf {
    match version_dir(dir, id) {
        Ok(p) => p,
        Err(e) => {
            crate::util::info(&format!("versions: {e}"));
            versions_dir(dir).join("invalid-version-id")
        }
    }
}

/// Well-known versions, used only when the network fails *and* no cache exists.
/// Ids/urls/sha1s were taken from the live `version_manifest_v2.json`.
const FALLBACK: &[(&str, &str, &str)] = &[
    ("1.21.8", "https://piston-meta.mojang.com/v1/packages/6b61868e9cc18b59f97241ac46cc84ab605c8152/1.21.8.json", "6b61868e9cc18b59f97241ac46cc84ab605c8152"),
    ("1.21.4", "https://piston-meta.mojang.com/v1/packages/ee8ab0f6ec89b3127436d35e36aa766c1e85c3a9/1.21.4.json", "ee8ab0f6ec89b3127436d35e36aa766c1e85c3a9"),
    ("1.20.6", "https://piston-meta.mojang.com/v1/packages/c16c1275df4c25b237c34cd1b11a3126c23f2f82/1.20.6.json", "c16c1275df4c25b237c34cd1b11a3126c23f2f82"),
    ("1.20.1", "https://piston-meta.mojang.com/v1/packages/38d412af3b247e485c7f7e50ac25febc4e79ab1d/1.20.1.json", "38d412af3b247e485c7f7e50ac25febc4e79ab1d"),
    ("1.19.4", "https://piston-meta.mojang.com/v1/packages/50ee76030472297b1be01e414f3f29acd7112334/1.19.4.json", "50ee76030472297b1be01e414f3f29acd7112334"),
    ("1.18.2", "https://piston-meta.mojang.com/v1/packages/334b33fcba3c9be4b7514624c965256535bd7eba/1.18.2.json", "334b33fcba3c9be4b7514624c965256535bd7eba"),
    ("1.16.5", "https://piston-meta.mojang.com/v1/packages/fba9f7833e858a1257d810d21a3a9e3c967f9077/1.16.5.json", "fba9f7833e858a1257d810d21a3a9e3c967f9077"),
    ("1.12.2", "https://piston-meta.mojang.com/v1/packages/832d95b9f40699d4961394dcf6cf549e65f15dc5/1.12.2.json", "832d95b9f40699d4961394dcf6cf549e65f15dc5"),
    ("1.8.9", "https://piston-meta.mojang.com/v1/packages/d546f1707a3f2b7d034eece5ea2e311eda875787/1.8.9.json", "d546f1707a3f2b7d034eece5ea2e311eda875787"),
];

fn fallback_manifest() -> Vec<ManifestEntry> {
    FALLBACK
        .iter()
        .map(|(id, url, sha1)| ManifestEntry::new(id, "release", url, sha1, 0))
        .collect()
}

/// `true` if `p` exists and its mtime is younger than `ttl_ms`.
/// Missing file / unreadable mtime => not fresh (caller re-fetches).
fn cache_fresh(p: &Path, ttl_ms: u128) -> bool {
    let md = match std::fs::metadata(p) {
        Ok(m) => m,
        Err(_) => return false,
    };
    let mtime = match md.modified() {
        Ok(t) => t,
        Err(_) => return false,
    };
    match mtime.elapsed() {
        Ok(age) => age.as_millis() < ttl_ms,
        // mtime in the future (clock skew): treat as stale
        Err(_) => false,
    }
}

fn parse_manifest(text: &str) -> Res<Vec<ManifestEntry>> {
    let root = Json::parse(text).map_err(|e| format!("version manifest: {e}"))?;
    let arr = root
        .arr_at("versions")
        .ok_or_else(|| "version manifest: missing \"versions\" array".to_string())?;
    let mut out = Vec::with_capacity(arr.len());
    for v in arr {
        let id = match v.str_at("id") {
            Some(s) if !s.is_empty() => s,
            _ => continue,
        };
        let url = match v.str_at("url") {
            Some(s) if !s.is_empty() => s,
            _ => continue,
        };
        out.push(ManifestEntry::new(
            id,
            v.str_at("type").unwrap_or("release"),
            url,
            v.str_at("sha1").unwrap_or(""),
            v.u64_at("size").unwrap_or(0),
        ));
    }
    if out.is_empty() {
        return Err("version manifest: no usable entries".to_string());
    }
    Ok(out)
}

fn read_manifest_file(p: &Path) -> Option<Vec<ManifestEntry>> {
    let text = std::fs::read_to_string(p).ok()?;
    parse_manifest(&text).ok()
}

/// The version list, cached at `<dir>/versions/version_manifest_v2.json`.
///
/// Cache is reused while younger than 30 minutes. On network failure a stale
/// cache is still better than nothing; without any cache we fall back to the
/// small built-in table above.
pub fn fetch_manifest(dir: &Path) -> Res<Vec<ManifestEntry>> {
    let vdir = versions_dir(dir);
    mkdirs(&vdir)?;
    let cache = vdir.join(MANIFEST_FILE);

    if cache_fresh(&cache, MANIFEST_TTL_MS) {
        if let Some(list) = read_manifest_file(&cache) {
            return Ok(list);
        }
    }

    match net::get(MANIFEST_URL) {
        Ok(bytes) => {
            let text = String::from_utf8_lossy(&bytes).into_owned();
            let parsed = parse_manifest(&text);
            if parsed.is_ok() {
                // A failed cache write only costs us a re-fetch next run.
                let _ = std::fs::write(&cache, text.as_bytes());
                return parsed;
            }
            if let Some(list) = read_manifest_file(&cache) {
                return Ok(list);
            }
            Err(format!(
                "{} (downloaded from {MANIFEST_URL})",
                parsed.unwrap_err()
            ))
        }
        Err(e) => {
            if let Some(list) = read_manifest_file(&cache) {
                util::info(&format!("offline: using cached version manifest ({e})"));
                return Ok(list);
            }
            util::info(&format!(
                "offline: using built-in version list of {} releases ({e})",
                FALLBACK.len()
            ));
            Ok(fallback_manifest())
        }
    }
}

fn select_entry<'a>(list: &'a [ManifestEntry], want: &str) -> Option<&'a ManifestEntry> {
    let w = want.trim();
    let lower = w.to_ascii_lowercase();
    match lower.as_str() {
        "latest" | "latest-release" | "release" => {
            list.iter().find(|e| e.kind == "release").or_else(|| list.first())
        }
        "latest-snapshot" | "snapshot" => list.iter().find(|e| e.kind == "snapshot"),
        _ => list.iter().find(|e| e.id.eq_ignore_ascii_case(w)),
    }
}

/// Map a user-supplied selector onto a manifest entry.
pub fn resolve_entry(dir: &Path, want: &str) -> Res<ManifestEntry> {
    let list = fetch_manifest(dir)?;
    select_entry(&list, want).cloned().ok_or_else(|| {
        format!("unknown version '{want}': not in the version manifest — try `LFL --list`")
    })
}

fn manifest_lookup(dir: &Path, id: &str) -> Option<ManifestEntry> {
    let list = fetch_manifest(dir).ok()?;
    list.into_iter().find(|e| e.id.eq_ignore_ascii_case(id))
}

// ---------------------------------------------------------------------------
// Version discovery: what is installed, and what Mojang offers
// ---------------------------------------------------------------------------

/// A version row for the UI: its id, where it came from, and whether it can
/// already be launched without another download.
#[derive(Debug, Clone, PartialEq)]
pub struct VersionChoice {
    pub id: String,
    /// `release`, `snapshot`, `old_beta`, `old_alpha`, or `installed` for
    /// versions that exist only on disk (modded or hand-copied jars).
    pub kind: String,
    /// True when `<dir>/versions/<id>/<id>.jar` and a natives dir exist.
    pub installed: bool,
}

impl VersionChoice {
    pub fn new(id: &str, kind: &str, installed: bool) -> VersionChoice {
        VersionChoice {
            id: id.to_string(),
            kind: kind.to_string(),
            installed,
        }
    }
}

/// True when this version looks launchable on disk already.
pub fn is_installed(dir: &Path, id: &str) -> bool {
    let base = version_dir_or_fallback(dir, id);
    base.join(format!("{id}.json")).exists() && base.join(format!("{id}.jar")).exists()
}

/// Ids that have a version directory with a jar next to it.
///
/// This is what makes the launcher usable after the first run: the version list
/// reflects what is actually on this machine, instead of a hard-coded guess.
pub fn installed_versions(dir: &Path) -> Vec<VersionChoice> {
    let root = versions_dir(dir);
    let mut out = Vec::new();
    let rd = match std::fs::read_dir(&root) {
        Ok(r) => r,
        Err(_) => return out,
    };
    for e in rd.flatten() {
        if !e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let name = match e.file_name().to_str() {
            Some(n) => n.to_string(),
            None => continue,
        };
        // A version is usable when its jar is there, with or without a manifest
        // entry: a jar copied in by hand still gets its own entry.
        if !e.path().join(format!("{name}.jar")).exists() {
            continue;
        }
        out.push(VersionChoice::new(&name, "installed", true));
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// True when `id` could be a plain Mojang version id (`1.20.1`, `1.8.9`, `24w14a`).
///
/// Loader profile ids (`fabric-loader-0.16.9-1.20.1`, `1.20.1-forge-47.2.0`) and
/// `latest*` aliases are deliberately excluded: passing one of those to Modrinth
/// or to the loader feed as a *game version* matches nothing.
pub fn is_plain_version(id: &str) -> bool {
    let v = id.trim();
    if v.is_empty() || v.starts_with("latest") {
        return false;
    }
    let lower = v.to_ascii_lowercase();
    if lower.contains("fabric")
        || lower.contains("quilt")
        || lower.contains("forge")
        || lower.contains("neoforge")
    {
        return false;
    }
    v.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false)
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_')
}

/// The vanilla Minecraft version a local version id is built on.
///
/// A loader profile such as `fabric-loader-0.16.9-1.20.1` inherits from `1.20.1`
/// through its `inheritsFrom` field, and that parent id is the only thing
/// Modrinth and the loader feeds accept as a game version. Following the chain
/// is more reliable than pattern-matching the id, but a chain can be missing on
/// disk (a hand-copied profile), so the id itself is tried as a last resort.
///
/// Returns `""` when nothing usable is found, which callers treat as
/// "do not filter by game version".
pub fn base_version(dir: &Path, id: &str) -> String {
    let mut current = id.trim().to_string();
    if current.is_empty() {
        return String::new();
    }
    // Walk `inheritsFrom` down to the root, with a hard bound so a malformed
    // chain cannot loop forever.
    for _ in 0..MAX_DEPTH {
        if is_plain_version(&current) {
            return current;
        }
        let cdir = version_dir_or_fallback(dir, &current);
        let json = cdir.join(format!("{current}.json"));
        let text = match std::fs::read_to_string(&json) {
            Ok(t) => t,
            Err(_) => break,
        };
        let root = match Json::parse(&text) {
            Ok(j) => j,
            Err(_) => break,
        };
        let parent = root.str_at("inheritsFrom").unwrap_or("").trim().to_string();
        if parent.is_empty() || parent.eq_ignore_ascii_case(&current) {
            break;
        }
        current = parent;
    }
    if is_plain_version(&current) {
        return current;
    }
    // Last resort: the newest version-looking segment of the id itself. Loader
    // ids end with their MC version by convention (`...-1.20.1`), so this
    // recovers the right answer even when the profile JSON is gone.
    id.split(|c: char| c == '-' || c == '_' || c == ' ')
        .filter(|seg| is_plain_version(seg) && seg.chars().filter(|c| *c == '.').count() >= 1)
        .last()
        .map(|s| s.to_string())
        .unwrap_or_default()
}

/// Every version Mojang knows about, newest first, annotated with local state.
///
/// Falls back to the on-disk list when the manifest cannot be fetched (offline
/// or on first run without network), so the UI is never empty when it does not
/// have to be.
pub fn available_versions(dir: &Path) -> Res<Vec<VersionChoice>> {
    let installed: Vec<String> = installed_versions(dir).into_iter().map(|v| v.id).collect();
    let mark = |id: &str, kind: &str| {
        let is_local = installed.iter().any(|x| x.eq_ignore_ascii_case(id));
        VersionChoice::new(id, kind, is_local || is_installed(dir, id))
    };

    match fetch_manifest(dir) {
        Ok(list) if !list.is_empty() => {
            let mut out: Vec<VersionChoice> =
                list.iter().map(|e| mark(&e.id, &e.kind)).collect();
            // Local-only versions (modded jars, hand-copied clients) appended.
            for v in &installed {
                if !out.iter().any(|c| c.id.eq_ignore_ascii_case(v)) {
                    out.push(VersionChoice::new(v, "installed", true));
                }
            }
            Ok(out)
        }
        Ok(_) => Ok(installed
            .iter()
            .map(|v| VersionChoice::new(v, "installed", true))
            .collect()),
        Err(e) => {
            if installed.is_empty() {
                Err(e)
            } else {
                Ok(installed
                    .iter()
                    .map(|v| VersionChoice::new(v, "installed", true))
                    .collect())
            }
        }
    }
}

/// Filter `choices` by a case-insensitive substring of the id.
pub fn filter_versions(choices: &[VersionChoice], needle: &str) -> Vec<VersionChoice> {
    let n = needle.trim().to_ascii_lowercase();
    if n.is_empty() {
        return choices.to_vec();
    }
    choices
        .iter()
        .filter(|c| c.id.to_ascii_lowercase().contains(&n))
        .cloned()
        .collect()
}

/// What a bare `LFL` (no version given) should launch.
///
/// Preference order: the newest installed release, else any installed version,
/// else the manifest's latest release. This is the "just works" path: after the
/// first install the launcher reuses what is on disk instead of re-resolving.
pub fn default_version(choices: &[VersionChoice]) -> String {
    let installed: Vec<&VersionChoice> = choices.iter().filter(|c| c.installed).collect();
    if let Some(best) = installed
        .iter()
        .filter(|c| c.kind == "release" || c.kind == "installed")
        .max_by(|a, b| compare_version_ids(&a.id, &b.id))
    {
        return best.id.clone();
    }
    if let Some(any) = installed.first() {
        return any.id.clone();
    }
    if let Some(first_release) = choices.iter().find(|c| c.kind == "release") {
        return first_release.id.clone();
    }
    choices
        .first()
        .map(|c| c.id.clone())
        .unwrap_or_else(|| "latest-release".to_string())
}

/// Compare version ids numerically per dotted segment, so `1.20.1 > 1.9.4`.
pub fn compare_version_ids(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let key = |s: &str| -> Vec<i64> {
        s.split(['.', '-', '_'])
            .map(|p| p.parse::<i64>().unwrap_or(-1))
            .collect()
    };
    let (ka, kb) = (key(a), key(b));
    for i in 0..ka.len().max(kb.len()) {
        let x = ka.get(i).copied().unwrap_or(-1);
        let y = kb.get(i).copied().unwrap_or(-1);
        match x.cmp(&y) {
            Ordering::Equal => {}
            other => return other,
        }
    }
    a.cmp(b)
}

// ---------------------------------------------------------------------------
// Version JSON parsing
// ---------------------------------------------------------------------------

fn parse_rules(v: Option<&Json>) -> Vec<Rule> {
    let arr = match v.and_then(|j| j.as_arr()) {
        Some(a) => a,
        None => return Vec::new(),
    };
    let mut out = Vec::with_capacity(arr.len());
    for r in arr {
        let action = match r.str_at("action") {
            Some(a) => a,
            None => continue,
        };
        if action != "allow" && action != "disallow" {
            continue;
        }
        let os = r.get("os");
        let features = match r.get("features").and_then(|f| f.as_obj()) {
            Some(obj) => obj
                .iter()
                .filter_map(|(k, v)| Some((k.clone(), v.as_bool()?)))
                .collect(),
            None => Vec::new(),
        };
        out.push(Rule {
            action: action.to_string(),
            os_name: os.and_then(|o| o.str_at("name")).map(str::to_string),
            os_arch: os.and_then(|o| o.str_at("arch")).map(str::to_string),
            os_version: os.and_then(|o| o.str_at("version")).map(str::to_string),
            features,
        });
    }
    out
}

fn parse_download(j: &Json) -> Option<Download> {
    let path = j.str_at("path").unwrap_or("").to_string();
    let mut url = j.str_at("url").unwrap_or("").to_string();
    if url.is_empty() && !path.is_empty() {
        // Some modded/old JSONs omit the URL; libraries.minecraft.net serves
        // the canonical Maven layout.
        url = format!("{LIBRARY_BASE}{path}");
    }
    if url.is_empty() {
        return None;
    }
    Some(Download {
        url,
        path,
        sha1: j.str_at("sha1").unwrap_or("").to_string(),
        size: j.u64_at("size").unwrap_or(0),
    })
}

/// Maven coordinate `group:artifact:version[:classifier]` => `group/as/artifact/version/file.jar`.
fn maven_path(name: &str) -> Option<String> {
    let mut it = name.split(':');
    let group = it.next()?;
    let artifact = it.next()?;
    let version = it.next()?;
    let classifier = it.next().filter(|c| !c.is_empty());
    if group.is_empty() || artifact.is_empty() || version.is_empty() {
        return None;
    }
    let file = match classifier {
        Some(c) => format!("{artifact}-{version}-{c}.jar"),
        None => format!("{artifact}-{version}.jar"),
    };
    Some(format!(
        "{}/{artifact}/{version}/{file}",
        group.replace('.', "/")
    ))
}

fn parse_library(j: &Json) -> Option<Library> {
    let name = match j.str_at("name") {
        Some(n) if !n.is_empty() => n.to_string(),
        _ => return None,
    };
    let dl = j.get("downloads");

    let artifact = dl
        .and_then(|d| d.get("artifact"))
        .and_then(parse_download)
        .or_else(|| {
            // Older/modded JSONs only carry `name`. Synthesize the Maven URL as
            // a LAST RESORT, and only for plain jars: entries that declare
            // classifiers/natives have no main artifact by design (e.g.
            // `net.java.jinput:jinput-platform`), so inventing one would make
            // us download a file that does not exist.
            let has_classifiers = dl
                .and_then(|d| d.get("classifiers"))
                .and_then(|c| c.as_obj())
                .map(|o| !o.is_empty())
                .unwrap_or(false);
            let has_natives = j
                .get("natives")
                .and_then(|n| n.as_obj())
                .map(|o| !o.is_empty())
                .unwrap_or(false);
            if has_classifiers || has_natives {
                return None;
            }
            let path = maven_path(&name)?;
            // Loader profiles (Fabric/Quilt) put the Maven *repository root* in
            // a sibling `url` field instead of a `downloads` block, e.g.
            // {"name":"net.fabricmc:fabric-loader:0.20.1",
            //  "url":"https://maven.fabricmc.net/"}.
            // Honour it, falling back to Mojang's repo.
            let base = j
                .str_at("url")
                .filter(|u| u.starts_with("http"))
                .map(|u| {
                    if u.ends_with('/') {
                        u.to_string()
                    } else {
                        format!("{u}/")
                    }
                })
                .unwrap_or_else(|| LIBRARY_BASE.to_string());
            Some(Download {
                url: format!("{base}{path}"),
                path,
                sha1: String::new(),
                size: 0,
            })
        });

    let mut classifiers = Vec::new();
    if let Some(obj) = dl.and_then(|d| d.get("classifiers")).and_then(|c| c.as_obj()) {
        for (k, v) in obj {
            if let Some(d) = parse_download(v) {
                classifiers.push((k.clone(), d));
            }
        }
    }

    let mut natives = Vec::new();
    if let Some(obj) = j.get("natives").and_then(|n| n.as_obj()) {
        for (k, v) in obj {
            if let Some(s) = v.as_str() {
                natives.push((k.clone(), s.to_string()));
            }
        }
    }

    let extract_exclude = j
        .get("extract")
        .and_then(|e| e.get("exclude"))
        .and_then(|e| e.as_arr())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    Some(Library {
        name,
        artifact,
        classifiers,
        natives,
        extract_exclude,
        rules: parse_rules(j.get("rules")),
    })
}

fn parse_args(v: Option<&Json>) -> Vec<Arg> {
    let arr = match v.and_then(|j| j.as_arr()) {
        Some(a) => a,
        None => return Vec::new(),
    };
    let mut out = Vec::with_capacity(arr.len());
    for e in arr {
        match e {
            // Bare string element: always allowed.
            Json::Str(s) => out.push(Arg {
                rules: Vec::new(),
                value: vec![s.clone()],
            }),
            Json::Obj(_) => {
                let rules = parse_rules(e.get("rules"));
                let mut value = Vec::new();
                match e.get("value") {
                    Some(Json::Str(s)) => value.push(s.clone()),
                    Some(Json::Arr(items)) => {
                        for it in items {
                            if let Some(s) = it.as_str() {
                                value.push(s.to_string());
                            }
                        }
                    }
                    _ => continue,
                }
                out.push(Arg { rules, value });
            }
            _ => continue,
        }
    }
    out
}

fn parse_asset_index(root: &Json) -> AssetIndex {
    if let Some(j) = root.get("assetIndex") {
        let mut ai = AssetIndex {
            id: j.str_at("id").unwrap_or("").to_string(),
            sha1: j.str_at("sha1").unwrap_or("").to_string(),
            size: j.u64_at("size").unwrap_or(0),
            total_size: j.u64_at("totalSize").unwrap_or(0),
            url: j.str_at("url").unwrap_or("").to_string(),
        };
        if ai.id.is_empty() {
            if let Some(a) = root.str_at("assets") {
                ai.id = a.to_string();
            }
        }
        ai
    } else {
        // Legacy JSONs (< 1.7) only expose the `assets` string; a child document
        // (mod loader) may expose neither — leave it empty so `merge_versions`
        // can inherit the parent's index.
        AssetIndex {
            id: root.str_at("assets").unwrap_or("").to_string(),
            ..Default::default()
        }
    }
}

fn parse_logging(root: &Json) -> Option<Logging> {
    let c = root.get("logging")?.get("client")?;
    let file = c.get("file");
    Some(Logging {
        arg: c.str_at("argument").unwrap_or("").to_string(),
        id: file.and_then(|f| f.str_at("id")).unwrap_or("").to_string(),
        url: file.and_then(|f| f.str_at("url")).unwrap_or("").to_string(),
        sha1: file.and_then(|f| f.str_at("sha1")).unwrap_or("").to_string(),
        size: file.and_then(|f| f.u64_at("size")).unwrap_or(0),
    })
}

/// Parse one version JSON document. `inheritsFrom` is recorded but not followed
/// (see `load`); absent optional fields simply stay empty/default so that a
/// child document (mod loader) can be merged with its parent.
pub fn parse_version(root: &Json, id_hint: &str) -> Res<Version> {
    if root.as_obj().is_none() {
        return Err(format!("version '{id_hint}': JSON root is not an object"));
    }
    let id = match root.str_at("id") {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => id_hint.to_string(),
    };
    let mut java_major = 0u32;
    let mut java_component = String::new();
    if let Some(jv) = root.get("javaVersion") {
        java_major = jv.u64_at("majorVersion").unwrap_or(0) as u32;
        java_component = jv.str_at("component").unwrap_or("").to_string();
    }
    let client = root
        .path(&["downloads", "client"])
        .and_then(parse_download)
        .map(|mut d| {
            d.path = format!("{id}.jar");
            d
        })
        .unwrap_or_default();
    Ok(Version {
        id,
        version_type: root.str_at("type").unwrap_or("").to_string(),
        main_class: root.str_at("mainClass").unwrap_or("").to_string(),
        asset_index: parse_asset_index(root),
        assets: root.str_at("assets").map(str::to_string),
        libraries: root
            .arr_at("libraries")
            .map(|a| a.iter().filter_map(parse_library).collect())
            .unwrap_or_default(),
        game_args: parse_args(root.get("arguments").and_then(|a| a.get("game"))),
        jvm_args: parse_args(root.get("arguments").and_then(|a| a.get("jvm"))),
        legacy_args: root.str_at("minecraftArguments").map(str::to_string),
        logging: parse_logging(root),
        client,
        java_major,
        java_component,
        inherits_from: root
            .str_at("inheritsFrom")
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        jar: root.str_at("jar").filter(|s| !s.is_empty()).map(str::to_string),
    })
}

fn parse_version_text(text: &str, id_hint: &str, src: &Path) -> Res<Version> {
    let root = Json::parse(text).map_err(|e| format!("{}: {e}", src.display()))?;
    parse_version(&root, id_hint).map_err(|e| format!("{}: {e}", src.display()))
}

/// Parent first, child overrides: `child` wins for scalars, libraries are
/// appended (child wins for identical `name`), arguments are concatenated.
fn merge_versions(parent: &Version, child: &Version) -> Version {
    let mut libraries = parent.libraries.clone();
    for lib in &child.libraries {
        match libraries.iter().position(|l| l.name == lib.name) {
            Some(i) => libraries[i] = lib.clone(),
            None => libraries.push(lib.clone()),
        }
    }
    let mut game_args = parent.game_args.clone();
    game_args.extend(child.game_args.iter().cloned());
    let mut jvm_args = parent.jvm_args.clone();
    jvm_args.extend(child.jvm_args.iter().cloned());

    let pick_str = |c: &str, p: &str| {
        if c.is_empty() {
            p.to_string()
        } else {
            c.to_string()
        }
    };
    let asset_index = if child.asset_index.id.is_empty() && child.asset_index.url.is_empty() {
        parent.asset_index.clone()
    } else {
        child.asset_index.clone()
    };
    let java_major = if child.java_major > 0 {
        child.java_major
    } else {
        parent.java_major
    };
    Version {
        id: child.id.clone(),
        version_type: pick_str(&child.version_type, &parent.version_type),
        main_class: pick_str(&child.main_class, &parent.main_class),
        asset_index,
        assets: child.assets.clone().or_else(|| parent.assets.clone()),
        libraries,
        game_args,
        jvm_args,
        legacy_args: child
            .legacy_args
            .clone()
            .or_else(|| parent.legacy_args.clone()),
        logging: child.logging.clone().or_else(|| parent.logging.clone()),
        client: if child.client.url.is_empty() {
            parent.client.clone()
        } else {
            child.client.clone()
        },
        java_major,
        java_component: pick_str(&child.java_component, &parent.java_component),
        inherits_from: child.inherits_from.clone(),
        jar: child.jar.clone().or_else(|| parent.jar.clone()),
    }
}

/// Walk one version chain: `<dir>/versions/<id>/<id>.json` for the child and
/// every `inheritsFrom` ancestor, then merge parent-first.
fn load_chain(dir: &Path, entry: &ManifestEntry) -> Res<Vec<Version>> {
    let vdir = versions_dir(dir);
    let mut chain: Vec<Version> = Vec::new();
    let mut want = Some(entry.clone());
    let mut id = entry.id.clone();

    for depth in 0..=MAX_DEPTH {
        if depth == MAX_DEPTH {
            return Err(format!(
                "version '{}': inheritsFrom chain longer than {MAX_DEPTH} (cycle?)",
                entry.id
            ));
        }
        let json_path = vdir.join(&id).join(format!("{id}.json"));
        let e = match want.take() {
            Some(e) => e,
            // A parent that is already on disk needs no network at all: check the
            // file first and only consult the manifest when it is missing.
            // (Modded parents live on disk only, which is perfectly normal.)
            None if json_path.is_file() => ManifestEntry::new(&id, "release", "", "", 0),
            None => match manifest_lookup(dir, &id) {
                Some(e) => e,
                None => ManifestEntry::new(&id, "release", "", "", 0),
            },
        };
        if !e.url.is_empty() {
            if let Some(parent) = json_path.parent() {
                mkdirs(parent)?;
            }
            net::download(&e.url, &json_path, &e.sha1, e.size)?;
        }
        if !json_path.exists() {
            return Err(format!(
                "version '{id}': missing version JSON {} (not in the manifest and not on disk)",
                json_path.display()
            ));
        }
        let text = std::fs::read_to_string(&json_path)
            .map_err(|e| format!("cannot read {}: {e}", json_path.display()))?;
        let v = parse_version_text(&text, &id, &json_path)?;
        match v.inherits_from.clone() {
            Some(parent_id) => {
                if parent_id.eq_ignore_ascii_case(&id) {
                    return Err(format!("version '{id}': inheritsFrom itself"));
                }
                if chain.iter().any(|c| c.id.eq_ignore_ascii_case(&parent_id)) {
                    return Err(format!(
                        "version '{}': inheritsFrom cycle via '{parent_id}'",
                        entry.id
                    ));
                }
                chain.push(v);
                id = parent_id;
            }
            None => {
                chain.push(v);
                return Ok(chain);
            }
        }
    }
    unreachable!()
}

/// Download (if needed) and parse `<dir>/versions/<id>/<id>.json`, following
/// `inheritsFrom`, and return the merged version.
///
/// Note: binary downloads (client jar, libraries, natives) are done by
/// `libraries::prepare`; this function only resolves metadata.
pub fn load(dir: &Path, entry: &ManifestEntry) -> Res<Version> {
    let mut chain = load_chain(dir, entry)?;
    let mut merged = chain
        .pop()
        .ok_or_else(|| format!("version '{}': empty version chain", entry.id))?;
    while let Some(child) = chain.pop() {
        merged = merge_versions(&merged, &child);
    }
    if merged.main_class.is_empty() {
        return Err(format!(
            "version '{}': missing \"mainClass\" (and no parent provides one)",
            merged.id
        ));
    }
    Ok(merged)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("lfl-ver-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn plain_version_accepts_real_ids_only() {
        for yes in ["1.20.1", "1.8.9", "24w14a", "1.21.8", "26.1.2-rc1"] {
            assert!(is_plain_version(yes), "{yes}");
        }
        // Loader profile ids and aliases are not game versions.
        for no in [
            "fabric-loader-0.16.9-1.20.1",
            "1.20.1-forge-47.2.0",
            "neoforge-21.1.0",
            "latest-release",
            "latest-snapshot",
            "latest",
            "",
            "release",
        ] {
            assert!(!is_plain_version(no), "{no}");
        }
    }

    #[test]
    fn base_version_follows_inherits_from() {
        let d = temp("inherit");
        // A Fabric profile inheriting from 1.20.1, the real on-disk shape.
        let p = versions_dir(&d).join("fabric-loader-0.16.9-1.20.1");
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(
            p.join("fabric-loader-0.16.9-1.20.1.json"),
            br#"{"inheritsFrom":"1.20.1"}"#,
        )
        .unwrap();
        assert_eq!(
            base_version(&d, "fabric-loader-0.16.9-1.20.1"),
            "1.20.1"
        );
        // A plain id passes straight through.
        assert_eq!(base_version(&d, "1.8.9"), "1.8.9");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn base_version_recovers_from_the_id_alone() {
        let d = temp("noid");
        // The profile JSON is gone (hand-copied version): the trailing segment
        // still names the game version, and that must not be lost.
        assert_eq!(
            base_version(&d, "fabric-loader-0.16.9-1.20.1"),
            "1.20.1"
        );
        assert_eq!(base_version(&d, "quilt-loader-0.26.1-1.20.4"), "1.20.4");
        // Nothing recoverable => empty, which callers treat as "no filter".
        assert_eq!(base_version(&d, "latest-release"), "");
        assert_eq!(base_version(&d, ""), "");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn base_version_survives_a_cycle() {
        let d = temp("cycle");
        for id in ["a-1.20.1", "b-2.0.0"] {
            let p = versions_dir(&d).join(id);
            std::fs::create_dir_all(&p).unwrap();
            let other = if id == "a-1.20.1" { "b-2.0.0" } else { "a-1.20.1" };
            std::fs::write(p.join(format!("{id}.json")), format!(r#"{{"inheritsFrom":"{other}"}}"#))
                .unwrap();
        }
        // Must terminate and return something, never hang.
        let _ = base_version(&d, "a-1.20.1");
        let _ = std::fs::remove_dir_all(&d);
    }

    fn win() -> Features {
        Features::default()
    }

    fn allow(os_name: Option<&str>, os_arch: Option<&str>) -> Rule {
        Rule {
            action: "allow".into(),
            os_name: os_name.map(str::to_string),
            os_arch: os_arch.map(str::to_string),
            os_version: None,
            features: Vec::new(),
        }
    }

    fn disallow(os_name: Option<&str>, os_arch: Option<&str>) -> Rule {
        Rule {
            action: "disallow".into(),
            ..allow(os_name, os_arch)
        }
    }

    fn feat_rule(name: &str, want: bool) -> Rule {
        Rule {
            action: "allow".into(),
            os_name: None,
            os_arch: None,
            os_version: None,
            features: vec![(name.to_string(), want)],
        }
    }

    #[test]
    fn rules_no_rules_allows() {
        assert!(rules_allow(&[], &win()));
    }

    #[test]
    fn rules_allow_with_os() {
        // 1.20.1 natives: [{"allow", os:{name:windows}}]
        assert!(rules_allow(&[allow(Some("windows"), None)], &win()));
        assert!(!rules_allow(&[allow(Some("linux"), None)], &win()));
        assert!(!rules_allow(&[allow(Some("osx"), None)], &win()));
    }

    #[test]
    fn rules_disallow_with_os() {
        // 1.8.9 lwjgl: [allow, disallow osx]
        let r = vec![allow(None, None), disallow(Some("osx"), None)];
        assert!(rules_allow(&r, &win()));
        // last matching rule wins: disallow windows blocks it
        let r = vec![allow(None, None), disallow(Some("windows"), None)];
        assert!(!rules_allow(&r, &win()));
        // order matters: allow after disallow re-allows
        let r = vec![disallow(Some("windows"), None), allow(None, None)];
        assert!(rules_allow(&r, &win()));
    }

    #[test]
    fn rules_arch_filtering() {
        // 32-bit/arm64 rules must not match an x64 build.
        assert!(!rules_allow(&[allow(Some("windows"), Some("x86"))], &win()));
        assert!(!rules_allow(&[allow(Some("windows"), Some("arm64"))], &win()));
        assert!(rules_allow(&[allow(Some("windows"), Some("x64"))], &win()));
        // omitted arch matches (real 1.20.1 natives rules)
        assert!(rules_allow(&[allow(Some("windows"), None)], &win()));
        assert_eq!(util::os_arch(), "x64");
    }

    #[test]
    fn rules_features() {
        assert!(!rules_allow(&[feat_rule("is_demo_user", true)], &win()));
        let f = Features {
            demo_user: true,
            ..Features::default()
        };
        assert!(rules_allow(&[feat_rule("is_demo_user", true)], &f));
        assert!(!rules_allow(&[feat_rule("is_demo_user", false)], &f));
        // unknown feature names never match
        assert!(!rules_allow(&[feat_rule("does_not_exist", true)], &f));
        assert!(!rules_allow(&[feat_rule("does_not_exist", false)], &f));
        // multiple feature conditions must all hold
        let f = Features {
            custom_resolution: true,
            demo_user: true,
            ..Features::default()
        };
        let r = Rule {
            features: vec![
                ("is_demo_user".into(), true),
                ("has_custom_resolution".into(), true),
            ],
            ..allow(None, None)
        };
        assert!(rules_allow(&[r.clone()], &f));
        let r2 = Rule {
            features: vec![
                ("is_demo_user".into(), true),
                ("has_custom_resolution".into(), false),
            ],
            ..allow(None, None)
        };
        assert!(!rules_allow(&[r2], &f));
    }

    #[test]
    fn rules_os_version_is_prefix_match() {
        // "^10\\.5\\.\\d$" is a macOS rule: must not match Windows "10.0"
        let r = Rule {
            os_version: Some("^10\\.5\\.\\d$".into()),
            ..allow(None, None)
        };
        assert!(!rules_allow(&[r], &win()));
        let r = Rule {
            os_version: Some("10\\.".into()),
            ..allow(None, None)
        };
        assert!(rules_allow(&[r], &win()));
    }

    #[test]
    fn args_modern_and_features() {
        let json = r#"{"arguments":{
            "game":[
              "--username","${auth_player_name}",
              {"rules":[{"action":"allow","features":{"is_demo_user":true}}],"value":"--demo"},
              {"rules":[{"action":"allow","features":{"has_custom_resolution":true}}],
               "value":["--width","${resolution_width}"]},
              {"rules":[{"action":"disallow","os":{"name":"windows"}}],"value":"--never"}
            ],
            "jvm":[
              {"rules":[{"action":"allow","os":{"name":"osx"}}],"value":["-XstartOnFirstThread"]},
              {"rules":[{"action":"allow","os":{"name":"windows"}}],"value":"-Dwin=1"},
              "-cp","${classpath}"
            ]}}"#;
        let v = parse_version(&Json::parse(json).unwrap(), "test").unwrap();
        assert_eq!(v.game_args.len(), 5);
        assert_eq!(
            resolve_args(&v.game_args, &win()),
            vec!["--username", "${auth_player_name}"]
        );
        let f = Features {
            demo_user: true,
            custom_resolution: true,
            ..Features::default()
        };
        assert_eq!(
            resolve_args(&v.game_args, &f),
            vec![
                "--username",
                "${auth_player_name}",
                "--demo",
                "--width",
                "${resolution_width}"
            ]
        );
        // osx-only jvm arg filtered out, windows one kept, placeholders raw
        assert_eq!(
            resolve_args(&v.jvm_args, &win()),
            vec!["-Dwin=1", "-cp", "${classpath}"]
        );
        // 1.13+ versions carry no legacy args
        assert!(v.legacy_args.is_none());
    }

    #[test]
    fn args_legacy_minecraft_arguments() {
        let json = r#"{"id":"1.8.9","mainClass":"net.minecraft.client.main.Main",
            "minecraftArguments":"--username ${auth_player_name} --gameDir ${game_directory}",
            "type":"release"}"#;
        let v = parse_version(&Json::parse(json).unwrap(), "1.8.9").unwrap();
        assert!(v.game_args.is_empty());
        assert_eq!(
            v.legacy_args.as_deref(),
            Some("--username ${auth_player_name} --gameDir ${game_directory}")
        );
        assert_eq!(v.version_type, "release");
    }

    #[test]
    fn parses_modern_version_fields() {
        let json = r#"{
          "id":"1.20.1","type":"release","mainClass":"net.minecraft.client.main.Main",
          "javaVersion":{"component":"java-runtime-gamma","majorVersion":17},
          "assetIndex":{"id":"5","sha1":"8a45","size":413067,"totalSize":651728646,
                        "url":"https://piston-meta.mojang.com/v1/packages/8a45/5.json"},
          "assets":"5",
          "downloads":{"client":{"sha1":"0c3e","size":23028853,
                        "url":"https://piston-data.mojang.com/v1/objects/0c3e/client.jar"}},
          "logging":{"client":{"argument":"-Dlog4j.configurationFile=${path}",
                     "file":{"id":"client-1.12.xml","sha1":"bd65","size":888,
                             "url":"https://piston-data.mojang.com/v1/objects/bd65/client-1.12.xml"},
                     "type":"log4j2-xml"}},
          "libraries":[
            {"name":"org.lwjgl:lwjgl:3.3.1",
             "downloads":{"artifact":{"path":"org/lwjgl/lwjgl/3.3.1/lwjgl-3.3.1.jar",
                                      "sha1":"aaa","size":100,
                                      "url":"https://libraries.minecraft.net/org/lwjgl/lwjgl/3.3.1/lwjgl-3.3.1.jar"}},
             "rules":[{"action":"allow"},{"action":"disallow","os":{"name":"osx"}}]},
            {"name":"org.lwjgl:lwjgl:3.3.1:natives-windows",
             "downloads":{"artifact":{"path":"org/lwjgl/lwjgl/3.3.1/lwjgl-3.3.1-natives-windows.jar",
                                      "sha1":"bbb","size":159361,
                                      "url":"https://libraries.minecraft.net/org/lwjgl/lwjgl/3.3.1/lwjgl-3.3.1-natives-windows.jar"}},
             "rules":[{"action":"allow","os":{"name":"windows"}}]}
          ]}"#;
        let v = parse_version(&Json::parse(json).unwrap(), "1.20.1").unwrap();
        assert_eq!(v.id, "1.20.1");
        assert_eq!(v.main_class, "net.minecraft.client.main.Main");
        assert_eq!(v.java_major, 17);
        assert_eq!(v.java_component, "java-runtime-gamma");
        assert_eq!(v.asset_index.id, "5");
        assert_eq!(v.asset_index.total_size, 651_728_646);
        assert_eq!(v.assets.as_deref(), Some("5"));
        assert_eq!(v.client.size, 23_028_853);
        assert_eq!(v.client.path, "1.20.1.jar");
        let lg = v.logging.expect("logging");
        assert_eq!(lg.id, "client-1.12.xml");
        assert!(lg.arg.contains("${path}"));
        assert_eq!(v.libraries.len(), 2);
        assert_eq!(v.libraries[0].artifact.as_ref().unwrap().size, 100);
        assert_eq!(v.libraries[1].classifiers.len(), 0);
        assert!(v.libraries[1].natives.is_empty());
        assert!(rules_allow(&v.libraries[0].rules, &win()));
        assert!(rules_allow(&v.libraries[1].rules, &win()));
        assert!(v.inherits_from.is_none());
        assert!(v.jar.is_none());
    }

    #[test]
    fn parses_legacy_library_natives_and_classifiers() {
        let json = r#"{"id":"1.8.9","mainClass":"x","libraries":[
          {"name":"tv.twitch:twitch-platform:6.5",
           "downloads":{"classifiers":{
              "natives-osx":{"path":"tv/twitch/twitch-platform/6.5/twitch-platform-6.5-natives-osx.jar","sha1":"1","size":1,"url":"https://libraries.minecraft.net/a.jar"},
              "natives-windows-64":{"path":"tv/twitch/twitch-platform/6.5/twitch-platform-6.5-natives-windows-64.jar","sha1":"2","size":2,"url":"https://libraries.minecraft.net/b.jar"}}},
           "extract":{"exclude":["META-INF/"]},
           "natives":{"linux":"natives-linux","windows":"natives-windows-${arch}"},
           "rules":[{"action":"allow"},{"action":"disallow","os":{"name":"linux"}}]},
          {"name":"net.java.jinput:jinput-platform:2.0.5",
           "downloads":{"classifiers":{
              "natives-windows":{"path":"net/java/jinput/jinput-platform/2.0.5/jinput-platform-2.0.5-natives-windows.jar","sha1":"3","size":3,"url":"https://libraries.minecraft.net/c.jar"}}},
           "natives":{"windows":"natives-windows"}}
        ]}"#;
        let v = parse_version(&Json::parse(json).unwrap(), "1.8.9").unwrap();
        let twitch = &v.libraries[0];
        assert_eq!(twitch.classifiers.len(), 2);
        assert!(twitch.artifact.is_none(), "classifier-only lib gets no artifact");
        assert_eq!(twitch.extract_exclude, vec!["META-INF/"]);
        assert_eq!(
            twitch.natives,
            vec![
                ("linux".to_string(), "natives-linux".to_string()),
                ("windows".to_string(), "natives-windows-${arch}".to_string())
            ]
        );
        let jinput = &v.libraries[1];
        assert!(jinput.artifact.is_none());
        assert_eq!(jinput.natives.len(), 1);
    }

    #[test]
    fn synthesizes_maven_url_only_for_plain_name_only_libs() {
        let json = r#"{"id":"x","mainClass":"x","libraries":[
            {"name":"com.example:thing:1.2.3"},
            {"name":"org.lwjgl.lwjgl:lwjgl-platform:2.9.1",
             "downloads":{"classifiers":{"natives-windows":{
                "path":"org/lwjgl/lwjgl/lwjgl-platform/2.9.1/lwjgl-platform-2.9.1-natives-windows.jar","sha1":"","size":0}}},
             "natives":{"windows":"natives-windows"}}
        ]}"#;
        let v = parse_version(&Json::parse(json).unwrap(), "x").unwrap();
        let a = v.libraries[0].artifact.as_ref().unwrap();
        assert_eq!(a.path, "com/example/thing/1.2.3/thing-1.2.3.jar");
        assert_eq!(
            a.url,
            "https://libraries.minecraft.net/com/example/thing/1.2.3/thing-1.2.3.jar"
        );
        assert_eq!(a.sha1, "");
        // classifier-only entries must NOT get a synthesized main artifact
        assert!(v.libraries[1].artifact.is_none());
    }

    #[test]
    fn legacy_assets_field_only() {
        let json = r#"{"id":"1.5.2","mainClass":"net.minecraft.launchwrapper.Launch",
            "assets":"pre-1.6","minecraftArguments":"--x"}"#;
        let v = parse_version(&Json::parse(json).unwrap(), "1.5.2").unwrap();
        assert_eq!(v.asset_index.id, "pre-1.6");
        assert!(v.asset_index.url.is_empty());
        // defaults for absent javaVersion
        assert_eq!(v.java_major, 0);
        assert_eq!(v.java_component, "");
        assert_eq!(v.inherits_from, None);
    }

    #[test]
    fn missing_fields_are_clean_errors() {
        // malformed JSON text
        let e = parse_version_text("{not json", "1.2.3", Path::new("v.json")).unwrap_err();
        assert!(e.contains("v.json"), "error names the file: {e}");
        // root is not an object
        let e = parse_version(&Json::parse("[1,2]").unwrap(), "1.2.3").unwrap_err();
        assert!(e.contains("1.2.3"));
        // missing mainClass in a standalone doc => load() rejects it
        let e = load_chain_missing_main();
        assert!(e.contains("mainClass"), "{e}");
        // missing version json on disk, not in manifest. Seed a fresh manifest
        // cache so the lookup below is served from disk (tests stay offline).
        let dir = temp_dir("missing");
        let vdir = versions_dir(&dir);
        std::fs::create_dir_all(&vdir).unwrap();
        std::fs::write(
            vdir.join(MANIFEST_FILE),
            r#"{"versions":[{"id":"1.20.1","type":"release","url":"https://x/1.20.1.json"}]}"#,
        )
        .unwrap();
        let entry = ManifestEntry::new("nope", "release", "", "", 0);
        let e = load_chain(&dir, &entry).unwrap_err();
        assert!(e.contains("nope"), "{e}");
        assert!(e.contains("missing version JSON"), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn load_chain_missing_main() -> String {
        let dir = temp_dir("nomain");
        let vdir = versions_dir(&dir);
        std::fs::create_dir_all(vdir.join("1.2.3")).unwrap();
        std::fs::write(
            vdir.join("1.2.3").join("1.2.3.json"),
            r#"{"id":"1.2.3","type":"release"}"#,
        )
        .unwrap();
        // local-only entry: url empty so no network
        let entry = ManifestEntry::new("1.2.3", "release", "", "", 0);
        let e = load(&dir, &entry).unwrap_err();
        let _ = std::fs::remove_dir_all(&dir);
        e
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "lfl-meta-{tag}-{}-{}",
            std::process::id(),
            util::now_ms()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn inheritance_merge_parent_then_child() {
        let parent_json = r#"{
          "id":"1.20.1","type":"release","mainClass":"net.minecraft.client.main.Main",
          "javaVersion":{"component":"java-runtime-gamma","majorVersion":17},
          "assetIndex":{"id":"5","sha1":"s5","size":10,"totalSize":20,"url":"https://x/5.json"},
          "assets":"5",
          "downloads":{"client":{"sha1":"c","size":23,"url":"https://x/client.jar"}},
          "logging":{"client":{"argument":"-Dlog4j.configurationFile=${path}",
                    "file":{"id":"client-1.12.xml","sha1":"l","size":8,"url":"https://x/l.xml"}}},
          "libraries":[
            {"name":"lib:parent:1","downloads":{"artifact":{"path":"lib/parent/1/parent-1.jar","sha1":"p","size":1,"url":"https://x/p.jar"}}},
            {"name":"lib:shared:1","downloads":{"artifact":{"path":"lib/shared/1/shared-1.jar","sha1":"s","size":1,"url":"https://x/s.jar"}}}
          ],
          "arguments":{"game":["--username","${auth_player_name}"],
                       "jvm":["-cp","${classpath}"]}}"#;
        let child_json = r#"{
          "id":"1.20.1-forge","type":"release","inheritsFrom":"1.20.1",
          "mainClass":"cpw.mods.bootstraplauncher.BootstrapLauncher",
          "jar":"1.20.1",
          "libraries":[
            {"name":"lib:child:2","downloads":{"artifact":{"path":"lib/child/2/child-2.jar","sha1":"k","size":1,"url":"https://x/k.jar"}}},
            {"name":"lib:shared:1","downloads":{"artifact":{"path":"lib/shared/1/shared-1-forge.jar","sha1":"s2","size":2,"url":"https://x/s2.jar"}}}
          ],
          "arguments":{"game":["--launchTarget","forgeclient"]}}"#;
        let parent = parse_version(&Json::parse(parent_json).unwrap(), "1.20.1").unwrap();
        let child = parse_version(&Json::parse(child_json).unwrap(), "1.20.1-forge").unwrap();
        assert_eq!(child.asset_index.id, "", "child has no assetIndex");
        assert!(child.client.url.is_empty());

        let m = merge_versions(&parent, &child);
        assert_eq!(m.id, "1.20.1-forge");
        assert_eq!(m.main_class, "cpw.mods.bootstraplauncher.BootstrapLauncher");
        assert_eq!(m.asset_index.id, "5");
        assert_eq!(m.asset_index.total_size, 20);
        assert_eq!(m.java_major, 17);
        assert_eq!(m.java_component, "java-runtime-gamma");
        assert_eq!(m.client.url, "https://x/client.jar");
        assert_eq!(m.logging.as_ref().unwrap().id, "client-1.12.xml");
        assert_eq!(m.jar.as_deref(), Some("1.20.1"));
        assert_eq!(m.client_jar_rel(), "1.20.1/1.20.1.jar");
        // parent libs first, child libs appended, shared name replaced in place
        let names: Vec<&str> = m.libraries.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["lib:parent:1", "lib:shared:1", "lib:child:2"]
        );
        assert_eq!(
            m.libraries[1].artifact.as_ref().unwrap().path,
            "lib/shared/1/shared-1-forge.jar"
        );
        // args concatenated parent-then-child, placeholders untouched
        assert_eq!(
            resolve_args(&m.game_args, &win()),
            vec!["--username", "${auth_player_name}", "--launchTarget", "forgeclient"]
        );
        assert_eq!(resolve_args(&m.jvm_args, &win()), vec!["-cp", "${classpath}"]);
    }

    #[test]
    fn inheritance_guards_cycles() {
        let dir = temp_dir("cycle");
        let vdir = versions_dir(&dir);
        for (id, parent) in [("a", "b"), ("b", "a")] {
            let d = vdir.join(id);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(
                d.join(format!("{id}.json")),
                format!(r#"{{"id":"{id}","mainClass":"x","inheritsFrom":"{parent}"}}"#),
            )
            .unwrap();
        }
        let entry = ManifestEntry::new("a", "release", "", "", 0);
        let e = load_chain(&dir, &entry).unwrap_err();
        assert!(e.contains("cycle"), "{e}");
        // self reference
        std::fs::write(
            vdir.join("a").join("a.json"),
            r#"{"id":"a","mainClass":"x","inheritsFrom":"a"}"#,
        )
        .unwrap();
        let e = load_chain(&dir, &entry).unwrap_err();
        assert!(e.contains("itself"), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn manifest_select_entry() {
        let list = vec![
            ManifestEntry::new("26.4-snapshot-5", "snapshot", "u1", "", 0),
            ManifestEntry::new("1.21.8", "release", "u2", "", 0),
            ManifestEntry::new("1.20.1", "release", "u3", "", 0),
        ];
        assert_eq!(select_entry(&list, "latest").unwrap().id, "1.21.8");
        assert_eq!(select_entry(&list, "latest-release").unwrap().id, "1.21.8");
        assert_eq!(select_entry(&list, "release").unwrap().id, "1.21.8");
        assert_eq!(select_entry(&list, "snapshot").unwrap().id, "26.4-snapshot-5");
        assert_eq!(
            select_entry(&list, "latest-snapshot").unwrap().id,
            "26.4-snapshot-5"
        );
        assert_eq!(select_entry(&list, "1.20.1").unwrap().id, "1.20.1");
        assert_eq!(select_entry(&list, "  1.20.1 ").unwrap().id, "1.20.1");
        assert!(select_entry(&list, "1.2.3").is_none());
    }

    #[test]
    fn manifest_parses_cached_bytes() {
        let text = r#"{"latest":{"release":"1.21.8","snapshot":"26.4-snapshot-5"},
          "versions":[
            {"id":"26.4-snapshot-5","type":"snapshot","url":"https://x/snap.json","time":"t","sha1":"aa"},
            {"id":"1.21.8","type":"release","url":"https://x/rel.json","releaseTime":"t"}
          ]}"#;
        let list = parse_manifest(text).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, "26.4-snapshot-5");
        assert_eq!(list[0].kind, "snapshot");
        assert_eq!(list[0].sha1, "aa");
        assert_eq!(list[1].url, "https://x/rel.json");
        assert_eq!(list[1].size, 0);
        assert!(parse_manifest("{}").is_err());
        assert!(parse_manifest("{\"versions\":[]}").is_err());
    }

    #[test]
    fn fallback_list_is_usable_offline() {
        let list = fallback_manifest();
        assert_eq!(list.len(), 9);
        assert!(list.iter().all(|e| e.kind == "release" && e.url.starts_with("https://")));
        assert_eq!(select_entry(&list, "latest").unwrap().id, "1.21.8");
        assert_eq!(select_entry(&list, "1.8.9").unwrap().id, "1.8.9");
        // every fallback sha1 matches the package hash embedded in its URL
        for e in &list {
            assert!(e.url.contains(&e.sha1), "{} url/sha1 mismatch", e.id);
        }
    }

    #[test]
    fn cache_freshness_rules() {
        let dir = temp_dir("cache");
        let f = dir.join(MANIFEST_FILE);
        assert!(!cache_fresh(&f, MANIFEST_TTL_MS), "missing file is stale");
        std::fs::write(&f, "{}").unwrap();
        assert!(cache_fresh(&f, MANIFEST_TTL_MS), "just written is fresh");
        assert!(!cache_fresh(&f, 0), "ttl 0 is always stale");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn versions_dir_join() {
        assert_eq!(versions_dir(Path::new("d")), Path::new("d").join("versions"));
    }

    #[test]
    fn version_dir_rejects_escapes() {
        let d = Path::new("C:/mc");
        assert_eq!(version_dir(d, "1.20.1").unwrap(), d.join("versions/1.20.1"));
        assert_eq!(
            version_dir(d, "fabric-loader-0.16.9-1.20.1").unwrap(),
            d.join("versions/fabric-loader-0.16.9-1.20.1")
        );
        for bad in ["../../evil", "C:/Windows/x", "a/b", "..", "."] {
            assert!(version_dir(d, bad).is_err(), "should reject {bad:?}");
        }
    }

    #[test]
    fn version_dir_fallback_stays_inside_versions() {
        let d = Path::new("C:/mc");
        for bad in ["../../evil", "C:/Windows/x", ".."] {
            let p = version_dir_or_fallback(d, bad);
            assert_eq!(p.parent(), Some(versions_dir(d).as_path()), "{p:?}");
        }
    }

    /// Network test: needs `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn live_1_20_1_json() {
        let dir = temp_dir("live");
        let list = fetch_manifest(&dir).unwrap();
        let entry = select_entry(&list, "1.20.1").unwrap().clone();
        let v = load(&dir, &entry).unwrap();
        assert_eq!(v.main_class, "net.minecraft.client.main.Main");
        assert_eq!(v.java_major, 17);
        assert!(v.libraries.len() > 80, "libraries = {}", v.libraries.len());
        assert!(!v.client.url.is_empty());
        assert_eq!(v.asset_index.id, "5");
        assert!(v.logging.is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
