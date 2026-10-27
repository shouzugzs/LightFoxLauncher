// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025 LightFoxLauncher contributors
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Mod loaders (Fabric, Quilt, Forge, NeoForge) and mod management.
//!
//! A loader is installed the same way the official installer does it: fetch the
//! loader's *version profile* (a normal Minecraft version JSON with
//! `inheritsFrom` set to the vanilla version), write it to
//! `versions/<profile-id>/<profile-id>.json`, and let the existing
//! `version::load` chain-merge it with its parent. No loader-specific launch
//! logic is needed — `mainClass` and extra libraries come from the profile.
//!
//! Mods are plain `.jar` files in `<game dir>/mods/`. Installing one is a copy,
//! so this module only needs to validate the jar and keep names unique.
//!
//! OWNER: lead.

use std::path::{Path, PathBuf};

use crate::json::Json;
use crate::util::{self, Res};
use crate::version;

/// Supported loader families.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Loader {
    Fabric,
    Quilt,
    Forge,
    NeoForge,
}

impl Loader {
    pub fn id(self) -> &'static str {
        match self {
            Loader::Fabric => "fabric",
            Loader::Quilt => "quilt",
            Loader::Forge => "forge",
            Loader::NeoForge => "neoforge",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Loader::Fabric => "Fabric",
            Loader::Quilt => "Quilt",
            Loader::Forge => "Forge",
            Loader::NeoForge => "NeoForge",
        }
    }

    pub fn parse(s: &str) -> Option<Loader> {
        match s.trim().to_ascii_lowercase().as_str() {
            "fabric" => Some(Loader::Fabric),
            "quilt" => Some(Loader::Quilt),
            "forge" => Some(Loader::Forge),
            "neoforge" | "neo" => Some(Loader::NeoForge),
            _ => None,
        }
    }

    pub fn all() -> [Loader; 4] {
        [Loader::Fabric, Loader::Quilt, Loader::Forge, Loader::NeoForge]
    }

    /// True when this loader is installed via a ready-made profile JSON.
    ///
    /// Fabric and Quilt publish one; Forge and NeoForge only ship a Java
    /// installer, which is out of scope here (it would mean running arbitrary
    /// downloaded code, and the result is a profile JSON anyway).
    pub fn has_profile_feed(self) -> bool {
        matches!(self, Loader::Fabric | Loader::Quilt)
    }
}

/// A loader build that can be installed for a given Minecraft version.
#[derive(Debug, Clone, PartialEq)]
pub struct LoaderBuild {
    pub loader: Loader,
    pub mc_version: String,
    pub loader_version: String,
    /// Resulting version id, e.g. `fabric-loader-0.20.1-1.20.1`.
    pub profile_id: String,
}

impl LoaderBuild {
    pub fn label(&self) -> String {
        format!("{} {} ({})", self.loader.name(), self.loader_version, self.mc_version)
    }
}

fn meta_base(loader: Loader) -> &'static str {
    match loader {
        Loader::Fabric => "https://meta.fabricmc.net/v2",
        Loader::Quilt => "https://meta.quiltmc.org/v3",
        _ => "",
    }
}

/// Loader versions available for `mc`, newest first.
///
/// Hits the loader's meta API. A 404 means "this loader has no build for that
/// Minecraft version", which is reported as an empty list rather than an error.
pub fn available_builds(loader: Loader, mc: &str) -> Res<Vec<LoaderBuild>> {
    if !loader.has_profile_feed() {
        return Err(format!(
            "{} 需要运行其官方安装器，本启动器只支持有 profile 源的加载器（Fabric/Quilt）",
            loader.name()
        ));
    }
    let url = format!("{}/versions/loader/{}", meta_base(loader), mc);
    // A 4xx means "this loader has no build for that Minecraft version" (the
    // Fabric API answers 400 for e.g. 1.8.9), which is a normal answer, not an
    // error worth surfacing as a failure.
    let body = match crate::net::get_opt(&url) {
        Ok(Some(b)) => b,
        Ok(None) => return Ok(Vec::new()),
        Err(e) if e.starts_with("HTTP 4") => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let text = String::from_utf8_lossy(&body);
    let json = Json::parse(&text).map_err(|e| format!("cannot parse {url}: {e}"))?;
    let arr = json
        .as_arr()
        .ok_or_else(|| format!("{url}: expected an array of loader builds"))?;
    let mut out = Vec::new();
    for item in arr {
        let ver = item
            .path(&["loader", "version"])
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if ver.is_empty() {
            continue;
        }
        let stable = item
            .path(&["loader", "stable"])
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        // Unstable builds are listed too, but the UI shows the flag.
        if !stable && out.len() > 40 {
            continue;
        }
        out.push(LoaderBuild {
            loader,
            mc_version: mc.to_string(),
            loader_version: ver.to_string(),
            profile_id: profile_id(loader, ver, mc),
        });
    }
    Ok(out)
}

/// The version id a loader profile uses, matching the loaders' own convention.
pub fn profile_id(loader: Loader, loader_version: &str, mc: &str) -> String {
    match loader {
        Loader::Fabric => format!("fabric-loader-{loader_version}-{mc}"),
        Loader::Quilt => format!("quilt-loader-{loader_version}-{mc}"),
        Loader::Forge => format!("{mc}-forge-{loader_version}"),
        Loader::NeoForge => format!("neoforge-{loader_version}"),
    }
}

/// URL of the profile JSON for a build, for loaders that publish one.
pub fn profile_url(b: &LoaderBuild) -> Res<String> {
    match b.loader {
        Loader::Fabric => Ok(format!(
            "https://meta.fabricmc.net/v2/versions/loader/{}/{}/profile/json",
            b.mc_version, b.loader_version
        )),
        Loader::Quilt => Ok(format!(
            "https://meta.quiltmc.org/v3/versions/loader/{}/{}/profile/json",
            b.mc_version, b.loader_version
        )),
        _ => Err(format!("{} 没有 profile 源", b.loader.name())),
    }
}

/// Install a loader profile into `<dir>/versions/<profile-id>/`.
///
/// Only the profile JSON is written; its libraries are downloaded later by the
/// normal `libraries::prepare` pass, which already understands the
/// `{name, url}` (maven-base) shape loader profiles use.
pub fn install(dir: &Path, b: &LoaderBuild) -> Res<PathBuf> {
    if !crate::version::is_installed(dir, &b.mc_version) {
        return Err(format!(
            "请先安装原版 {} —— 加载器需要它作为父版本",
            b.mc_version
        ));
    }
    let url = profile_url(b)?;
    let body = crate::net::get(&url)?;
    // Validate before writing: a truncated body must not become a version entry.
    let text = String::from_utf8_lossy(&body);
    let json = Json::parse(&text).map_err(|e| format!("cannot parse {url}: {e}"))?;
    let inherits = json.str_at("inheritsFrom").unwrap_or("");
    if inherits.is_empty() {
        return Err(format!("{url}: profile has no inheritsFrom"));
    }
    if !inherits.eq_ignore_ascii_case(&b.mc_version) {
        return Err(format!(
            "{url}: profile inherits {inherits}, expected {}",
            b.mc_version
        ));
    }
    let main_class = json.str_at("mainClass").unwrap_or("");
    if main_class.is_empty() {
        return Err(format!("{url}: profile has no mainClass"));
    }

    // `profile_id` is built from loader/mc version strings that came off the
    // network. It becomes a directory name under `versions/`, so it is validated
    // as one safe component before any path is built from it.
    util::safe_component(&b.profile_id)
        .map_err(|e| format!("加载器返回了不安全的版本 id：{e}"))?;
    let vdir = version::version_dir_or_fallback(dir, &b.profile_id);
    util::mkdirs(&vdir)?;
    let target = vdir.join(format!("{}.json", b.profile_id));
    let tmp = vdir.join(format!("{}.json.tmp", b.profile_id));
    std::fs::write(&tmp, &body).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    if target.exists() {
        let _ = std::fs::remove_file(&target);
    }
    std::fs::rename(&tmp, &target)
        .map_err(|e| format!("cannot replace {}: {e}", target.display()))?;
    Ok(target)
}

/// A mod jar sitting in `<game dir>/mods/`.
#[derive(Debug, Clone, PartialEq)]
pub struct Mod {
    pub file: PathBuf,
    pub size: u64,
    /// `fabric.mod.json` id when readable, else the file stem.
    pub name: String,
    /// Declared version when readable.
    pub version: String,
}

/// Directory holding the mods of one instance.
pub fn mods_dir(dir: &Path, instance: &str) -> PathBuf {
    dir.join("instances").join(instance).join("mods")
}

/// Scan a mods directory. Missing directory ⇒ empty list, never an error.
pub fn list_mods(mods: &Path) -> Vec<Mod> {
    let mut out = Vec::new();
    let rd = match std::fs::read_dir(mods) {
        Ok(r) => r,
        Err(_) => return out,
    };
    for e in rd.flatten() {
        let p = e.path();
        let name = p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
        if !name.ends_with(".jar") || name.ends_with(".jar.disabled") {
            continue;
        }
        let size = util::file_size(&p);
        let (mod_name, version) = read_mod_meta(&p).unwrap_or_else(|| {
            (
                p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default(),
                String::new(),
            )
        });
        out.push(Mod {
            file: p,
            size,
            name: mod_name,
            version,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Read `fabric.mod.json` (or `quilt.mod.json`/`META-INF/mods.toml` name) from a jar.
fn read_mod_meta(jar: &Path) -> Option<(String, String)> {
    // Unpacking every jar would be expensive; pull just the metadata entry.
    let json = crate::zip::read_entry_string(jar, &["fabric.mod.json", "quilt.mod.json"]).ok()?;
    let parsed = Json::parse(&json).ok()?;
    let id = parsed.str_at("id")?.to_string();
    let version = parsed.str_at("version").unwrap_or("").to_string();
    Some((id, version))
}

/// Copy `source` into the mods directory, refusing to clobber an existing file.
///
/// Returns the installed path. A file with the same name already present yields
/// an error rather than an overwrite, so a second install cannot silently break
/// a working setup.
pub fn install_mod(mods: &Path, source: &Path) -> Res<PathBuf> {
    let name = source
        .file_name()
        .ok_or_else(|| format!("{} 没有文件名", source.display()))?;
    let lname = name.to_string_lossy().to_lowercase();
    if !lname.ends_with(".jar") {
        return Err(format!("{} 不是 .jar 文件", source.display()));
    }
    if util::file_size(source) == 0 {
        return Err(format!("{} 是空文件", source.display()));
    }
    util::mkdirs(mods)?;
    let dest = mods.join(name);
    if dest.exists() {
        return Err(format!("已存在同名模组 {}", dest.display()));
    }
    std::fs::copy(source, &dest)
        .map_err(|e| format!("cannot copy {} to {}: {e}", source.display(), dest.display()))?;
    Ok(dest)
}

/// Remove a mod by file path.
pub fn remove_mod(file: &Path) -> Res<()> {
    std::fs::remove_file(file).map_err(|e| format!("cannot delete {}: {e}", file.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loader_names_and_ids() {
        assert_eq!(Loader::Fabric.id(), "fabric");
        assert_eq!(Loader::parse("NeoForge"), Some(Loader::NeoForge));
        assert_eq!(Loader::parse("QUILT"), Some(Loader::Quilt));
        assert_eq!(Loader::parse("none"), None);
        assert_eq!(Loader::all().len(), 4);
    }

    #[test]
    fn profile_ids_match_loader_conventions() {
        assert_eq!(
            profile_id(Loader::Fabric, "0.20.1", "1.20.1"),
            "fabric-loader-0.20.1-1.20.1"
        );
        assert_eq!(
            profile_id(Loader::Quilt, "0.27.0", "1.20.1"),
            "quilt-loader-0.27.0-1.20.1"
        );
        assert_eq!(profile_id(Loader::Forge, "47.2.0", "1.20.1"), "1.20.1-forge-47.2.0");
    }

    #[test]
    fn only_profile_feed_loaders_have_urls() {
        let b = LoaderBuild {
            loader: Loader::Fabric,
            mc_version: "1.20.1".into(),
            loader_version: "0.20.1".into(),
            profile_id: profile_id(Loader::Fabric, "0.20.1", "1.20.1"),
        };
        let u = profile_url(&b).unwrap();
        assert!(u.contains("meta.fabricmc.net"), "{u}");
        assert!(u.contains("1.20.1"));
        assert!(u.contains("0.20.1"));

        let forge = LoaderBuild {
            loader: Loader::Forge,
            ..b
        };
        assert!(profile_url(&forge).is_err());
        assert!(available_builds(Loader::Forge, "1.20.1").is_err());
    }

    #[test]
    fn listing_a_missing_mods_dir_is_empty() {
        let p = std::env::temp_dir().join("lfl-mods-does-not-exist");
        let _ = std::fs::remove_dir_all(&p);
        assert!(list_mods(&p).is_empty());
    }

    #[test]
    fn install_mod_rejects_non_jars_and_empty_files() {
        let dir = std::env::temp_dir().join(format!("lfl-mods-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let txt = dir.join("readme.txt");
        std::fs::write(&txt, b"hello").unwrap();
        assert!(install_mod(&dir.join("mods"), &txt).is_err());

        let empty = dir.join("empty.jar");
        std::fs::write(&empty, b"").unwrap();
        assert!(install_mod(&dir.join("mods"), &empty).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn install_mod_refuses_to_overwrite() {
        let dir = std::env::temp_dir().join(format!("lfl-mods-dup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("x.jar");
        std::fs::write(&src, b"PK\x03\x04stub").unwrap();
        let mods = dir.join("mods");

        let first = install_mod(&mods, &src).expect("first install");
        assert!(first.exists());
        let err = install_mod(&mods, &src).expect_err("second install must fail");
        assert!(err.contains("已存在"), "{err}");
        // The original must be untouched (the stub is 8 bytes: PK + stub).
        assert_eq!(util::file_size(&first), 8);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mods_dir_is_per_instance() {
        let d = PathBuf::from("DATA");
        assert_eq!(mods_dir(&d, "default"), d.join("instances").join("default").join("mods"));
        assert_ne!(mods_dir(&d, "default"), mods_dir(&d, "pvp"));
    }
}
