// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025 LightFoxLauncher contributors
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Mod dependency resolution.
//!
//! Modrinth reports dependencies per *version*, in two shapes:
//!
//! * `version_id` set — an exact file was recorded, so it can be downloaded
//!   directly.
//! * only `project_id` set — the project is known but the exact build was not,
//!   so the newest build matching the target Minecraft version and loader must
//!   be resolved.
//!
//! Each dependency also carries a type: `required` (must be installed),
//! `optional` (nice to have) or `incompatible` (must NOT be installed). This
//! module only *reports*; the UI decides what to download.
//!
//! OWNER: lead.

use std::path::Path;

use crate::json::Json;
use crate::modrinth::{self, ModFile};

/// How strongly a dependency is needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Need {
    Required,
    Optional,
    Incompatible,
}

impl Need {
    pub fn parse(s: &str) -> Need {
        match s {
            "optional" => Need::Optional,
            "incompatible" => Need::Incompatible,
            // Modrinth uses "required" today; treat anything else as required so
            // an unknown future value never silently drops a dependency.
            _ => Need::Required,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Need::Required => "必需",
            Need::Optional => "可选",
            Need::Incompatible => "冲突",
        }
    }
}

/// One resolved (or unresolvable) dependency.
#[derive(Debug, Clone, PartialEq)]
pub struct Dep {
    /// Project id from Modrinth (`AANobbMI`).
    pub project_id: String,
    /// Human name, when it could be resolved.
    pub title: String,
    pub need: Need,
    /// A concrete file to install, when resolvable.
    pub file: Option<ModFile>,
    /// Why `file` is `None`, when it is.
    pub unresolved: Option<String>,
    /// Already present in the mods directory (by file name).
    pub installed: bool,
}

impl Dep {
    /// `Sodium (必需) — 已安装` / `Sodium (必需) — 待下载`
    pub fn summary(&self) -> String {
        let state = if self.installed {
            "已安装".to_string()
        } else if self.file.is_some() {
            "待下载".to_string()
        } else {
            format!(
                "无法解析{}",
                self.unresolved
                    .as_ref()
                    .map(|e| format!("：{e}"))
                    .unwrap_or_default()
            )
        };
        let name = if self.title.is_empty() {
            &self.project_id
        } else {
            &self.title
        };
        format!("{} ({}) — {}", name, self.need.label(), state)
    }

    /// Dependencies worth downloading automatically.
    pub fn should_download(&self) -> bool {
        self.need == Need::Required && !self.installed && self.file.is_some()
    }
}

/// A dependency as Modrinth reported it, before resolution.
#[derive(Debug, Clone, PartialEq)]
pub struct RawDep {
    pub version_id: Option<String>,
    pub project_id: Option<String>,
    pub need: Need,
}

/// Parse the `dependencies` array of one Modrinth version object.
pub fn parse_dependencies(version: &Json) -> Vec<RawDep> {
    let arr = match version.arr_at("dependencies") {
        Some(a) => a,
        None => return Vec::new(),
    };
    arr.iter()
        .filter_map(|d| {
            let need = Need::parse(d.str_at("dependency_type").unwrap_or("required"));
            let version_id = d.str_at("version_id").filter(|s| !s.is_empty()).map(str::to_string);
            let project_id = d.str_at("project_id").filter(|s| !s.is_empty()).map(str::to_string);
            if version_id.is_none() && project_id.is_none() {
                return None;
            }
            Some(RawDep {
                version_id,
                project_id,
                need,
            })
        })
        .collect()
}

/// Resolve one dependency into a concrete file.
///
/// `version_id` wins when present (it is the exact build the author tested).
/// Otherwise the newest build of `project_id` matching `mc`/`loader` is chosen.
/// `project_id` alone is also enough to fetch the display name, which is what
/// makes the report readable when a build cannot be found.
pub fn resolve(dep: &RawDep, mc: &str, loader: &str, mods: &Path) -> Dep {
    let mut out = Dep {
        project_id: dep.project_id.clone().unwrap_or_default(),
        title: String::new(),
        need: dep.need,
        file: None,
        unresolved: None,
        installed: false,
    };

    // Exact version: fetch it directly.
    if let Some(vid) = &dep.version_id {
        match modrinth::version_by_id(vid) {
            Ok(v) => {
                out.project_id = v.project_id.clone();
                out.title = v.version_name.clone();
                out.installed = mods.join(&v.filename).exists();
                out.file = Some(v);
                fill_title(&mut out);
                return out;
            }
            Err(e) => out.unresolved = Some(e),
        }
    }

    // Project id only: pick the newest compatible build.
    if let Some(pid) = &dep.project_id {
        match modrinth::versions(pid, mc, loader) {
            Ok(vs) => {
                if let Some(v) = vs.into_iter().next() {
                    out.installed = mods.join(&v.filename).exists();
                    out.file = Some(v);
                } else {
                    out.unresolved = Some(format!("没有适用于 {mc}/{loader} 的版本"));
                }
            }
            Err(e) => out.unresolved = Some(e),
        }
        let _ = fill_title(&mut out);
    }
    out
}

/// Look up a project's human name for the report; best effort, never fatal.
fn fill_title(dep: &mut Dep) -> Option<()> {
    if dep.title.is_empty() && !dep.project_id.is_empty() {
        if let Ok(t) = modrinth::project_title(&dep.project_id) {
            dep.title = t;
        }
    }
    Some(())
}

/// Resolve all dependencies of a version, required first.
pub fn resolve_all(
    version: &Json,
    mc: &str,
    loader: &str,
    mods: &Path,
) -> Vec<Dep> {
    let raw = parse_dependencies(version);
    let mut out: Vec<Dep> = raw.iter().map(|d| resolve(d, mc, loader, mods)).collect();
    // Required before optional before incompatible, then by name for stability.
    out.sort_by(|a, b| {
        let rank = |n: Need| match n {
            Need::Required => 0,
            Need::Optional => 1,
            Need::Incompatible => 2,
        };
        rank(a.need)
            .cmp(&rank(b.need))
            .then_with(|| a.title.cmp(&b.title))
    });
    out
}

/// Mod ids that are already installed (by jar file name), for a quick "present?" check.
pub fn installed_files(mods: &Path) -> Vec<String> {
    crate::loaders::list_mods(mods)
        .into_iter()
        .filter_map(|m| m.file.file_name().map(|f| f.to_string_lossy().to_string()))
        .collect()
}

/// Download every required, not-yet-installed dependency.
///
/// Returns one line per attempt so the caller can log exactly what happened.
/// Failures do not abort the batch: a missing optional dependency or a network
/// hiccup on one mod should not stop the others.
pub fn install_missing(deps: &[Dep], mods: &Path) -> Vec<String> {
    let mut log = Vec::new();
    for d in deps {
        if d.need == Need::Incompatible {
            if d.installed {
                log.push(format!(
                    "{} 与本模组冲突，建议从 mods 目录移除",
                    if d.title.is_empty() { &d.project_id } else { &d.title }
                ));
            }
            continue;
        }
        if d.installed {
            continue;
        }
        match &d.file {
            Some(f) => match modrinth::download_file(mods, f) {
                Ok(p) => log.push(format!("已安装依赖 {}", p.display())),
                Err(e) => log.push(format!("依赖下载失败 {}：{e}", f.filename)),
            },
            None => {
                if d.need == Need::Required {
                    log.push(format!(
                        "必需依赖 {} 无法自动获取{}",
                        if d.title.is_empty() { &d.project_id } else { &d.title },
                        d.unresolved
                            .as_ref()
                            .map(|e| format!("（{e}）"))
                            .unwrap_or_default()
                    ));
                }
            }
        }
    }
    log
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(s: &str) -> Json {
        Json::parse(s).unwrap()
    }

    #[test]
    fn parses_both_dependency_shapes() {
        let v = doc(
            r#"{"dependencies":[
                {"version_id":"ryOMVRuG","project_id":"AANobbMI","dependency_type":"required"},
                {"version_id":null,"project_id":"1eAoo2KR","dependency_type":"required"},
                {"version_id":null,"project_id":"mOgUt4GM","dependency_type":"optional"}
            ]}"#,
        );
        let deps = parse_dependencies(&v);
        assert_eq!(deps.len(), 3);
        assert_eq!(deps[0].version_id.as_deref(), Some("ryOMVRuG"));
        assert_eq!(deps[0].need, Need::Required);
        assert_eq!(deps[1].version_id, None);
        assert_eq!(deps[1].project_id.as_deref(), Some("1eAoo2KR"));
        assert_eq!(deps[2].need, Need::Optional);
    }

    #[test]
    fn unknown_dependency_type_is_treated_as_required() {
        let v = doc(r#"{"dependencies":[{"project_id":"x","dependency_type":"weird"}]}"#);
        assert_eq!(parse_dependencies(&v)[0].need, Need::Required);
    }

    #[test]
    fn entries_without_ids_are_dropped() {
        let v = doc(r#"{"dependencies":[{"dependency_type":"required"}]}"#);
        assert!(parse_dependencies(&v).is_empty());
    }

    #[test]
    fn missing_dependencies_key_is_empty() {
        assert!(parse_dependencies(&doc(r#"{"id":"1"}"#)).is_empty());
        assert!(parse_dependencies(&doc(r#"{"dependencies":null}"#)).is_empty());
    }

    #[test]
    fn summaries_name_the_state() {
        let mut d = Dep {
            project_id: "AANobbMI".into(),
            title: "Sodium".into(),
            need: Need::Required,
            file: None,
            unresolved: None,
            installed: false,
        };
        // No file resolved yet: that reads as "cannot resolve", not "to download".
        assert!(d.summary().contains("无法解析"), "{}", d.summary());
        d.file = Some(ModFile {
            project_id: "AANobbMI".into(),
            version_name: "v".into(),
            version_number: "1".into(),
            filename: "sodium.jar".into(),
            url: "https://example.invalid/s.jar".into(),
            sha1: String::new(),
            size: 1,
            loaders: vec![],
            game_versions: vec![],
        });
        assert!(d.summary().contains("待下载"), "{}", d.summary());
        d.installed = true;
        assert!(d.summary().contains("已安装"), "{}", d.summary());
        d.installed = false;
        d.file = None;
        d.unresolved = Some("没有适用于 1.20.1/fabric 的版本".into());
        assert!(d.summary().contains("无法解析"), "{}", d.summary());
    }

    #[test]
    fn only_required_missing_files_are_downloaded() {
        let mk = |need: Need, installed: bool, file: Option<ModFile>| Dep {
            project_id: "p".into(),
            title: "t".into(),
            need,
            file,
            unresolved: None,
            installed,
        };
        let f = ModFile {
            project_id: "p".into(),
            version_name: "v".into(),
            version_number: "1".into(),
            filename: "a.jar".into(),
            url: "https://example.invalid/a.jar".into(),
            sha1: String::new(),
            size: 1,
            loaders: vec![],
            game_versions: vec![],
        };
        assert!(mk(Need::Required, false, Some(f.clone())).should_download());
        assert!(!mk(Need::Required, true, Some(f.clone())).should_download());
        assert!(!mk(Need::Optional, false, Some(f.clone())).should_download());
        assert!(!mk(Need::Required, false, None).should_download());
        assert!(!mk(Need::Incompatible, false, Some(f)).should_download());
    }

    #[test]
    fn incompatible_that_is_installed_is_reported() {
        let d = Dep {
            project_id: "x".into(),
            title: "BadMod".into(),
            need: Need::Incompatible,
            file: None,
            unresolved: None,
            installed: true,
        };
        let dir = std::env::temp_dir().join(format!("lfl-dep-{}", std::process::id()));
        let log = install_missing(&[d], &dir);
        assert_eq!(log.len(), 1);
        assert!(log[0].contains("冲突"), "{}", log[0]);
    }
}
