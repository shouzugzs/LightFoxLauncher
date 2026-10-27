// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025 LightFoxLauncher contributors
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Per-version launch profiles ("多个同版本的管理").
//!
//! One Minecraft version on disk is often launched several ways: a 4G profile for
//! single-player, a 1G one for a small server, a demo profile, or two player
//! names against the same jars. Storing that in one global settings file means
//! editing it every time you switch.
//!
//! A profile is a small `.ini` beside the version, in
//! `versions/<id>/lfl-profiles/<name>.ini`, holding only the launch-time choices
//! (heap, player, window, instance flags). Files, mods and saves are untouched and
//! stay shared by the version, which is what keeps this cheap.
//!
//! OWNER: lead.

use std::path::{Path, PathBuf};

use crate::util::{self, Res};
use crate::version;

/// Name used when a version has no explicit profile of its own.
pub const DEFAULT_PROFILE: &str = "默认";

/// Launch-time choices that vary between profiles of the same version.
#[derive(Debug, Clone, PartialEq)]
pub struct Profile {
    pub name: String,
    /// `-Xmx` value, e.g. `2G`.
    pub memory: String,
    pub user: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// Version isolation, per profile so a "server" profile can share a game dir
    /// while the default one stays isolated.
    pub isolate: bool,
    /// Free-form note shown in the UI.
    pub note: String,
}

impl Default for Profile {
    fn default() -> Self {
        Profile {
            name: DEFAULT_PROFILE.to_string(),
            memory: "2G".to_string(),
            user: "Steve".to_string(),
            width: None,
            height: None,
            isolate: true,
            note: String::new(),
        }
    }
}

impl Profile {
    /// `默认 · 2G · Steve` — one line in a list.
    pub fn label(&self) -> String {
        format!("{} · {} · {}", self.name, self.memory, self.user)
    }

    pub fn validate(&self) -> Res<()> {
        crate::launcher::validate_memory(&self.memory)?;
        if self.user.is_empty() || self.user.len() > 16 {
            return Err("玩家名需为 1..=16 个字符".to_string());
        }
        validate_profile_name(&self.name)?;
        if self.width.is_some() != self.height.is_some() {
            return Err("窗口宽高必须同时设置".to_string());
        }
        Ok(())
    }
}

/// Profile names become file names, so they must be a single safe component.
pub fn validate_profile_name(name: &str) -> Res<()> {
    let n = name.trim();
    if n.is_empty() {
        return Err("配置名不能为空".to_string());
    }
    if n.chars().count() > 32 {
        return Err("配置名最长 32 个字符".to_string());
    }
    if n == "." || n == ".." {
        return Err("非法配置名".to_string());
    }
    let mut comps = Path::new(n).components();
    if !matches!(
        (comps.next(), comps.next()),
        (Some(std::path::Component::Normal(_)), None)
    ) {
        return Err("配置名不能包含路径分隔符".to_string());
    }
    const BAD: [char; 7] = ['<', '>', ':', '"', '|', '?', '*'];
    if n.chars().any(|c| c.is_control() || BAD.contains(&c)) {
        return Err("配置名不能包含 < > : \" | ? *".to_string());
    }
    // Windows reserves these device names with any extension, so `con.ini` is
    // just as invalid as `con`.
    let stem = n.split('.').next().unwrap_or(n).to_ascii_uppercase();
    const RESERVED: [&str; 22] = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
        "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    if RESERVED.contains(&stem.as_str()) {
        return Err(format!("配置名 '{n}' 是 Windows 保留名"));
    }
    Ok(())
}

/// `<data>/versions/<id>/lfl-profiles`
pub fn profiles_dir(data: &Path, version_id: &str) -> PathBuf {
    version::versions_dir(data).join(version_id).join("lfl-profiles")
}

/// Path of one profile file.
pub fn profile_file(data: &Path, version_id: &str, name: &str) -> PathBuf {
    profiles_dir(data, version_id).join(format!("{}.ini", name))
}

/// All profiles of a version, `默认` first and the rest alphabetically.
///
/// A version with no stored profile still yields the default, so the UI always
/// has something to show and the caller never needs a "no profiles" branch.
pub fn list(data: &Path, version_id: &str) -> Vec<Profile> {
    let dir = profiles_dir(data, version_id);
    let mut out: Vec<Profile> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("ini") {
                continue;
            }
            let stem = match p.file_stem().and_then(|s| s.to_str()) {
                Some(s) => s.to_string(),
                None => continue,
            };
            let mut prof = load_file(&p).unwrap_or_default();
            prof.name = stem;
            out.push(prof);
        }
    }
    out.sort_by(|a, b| {
        // `默认` always first, then by name.
        let rank = |n: &str| if n == DEFAULT_PROFILE { 0 } else { 1 };
        rank(&a.name).cmp(&rank(&b.name)).then_with(|| a.name.cmp(&b.name))
    });
    // Every version implicitly has the default profile even before anything was
    // saved, so the UI always offers one selectable entry instead of an empty box.
    if !out.iter().any(|p| p.name == DEFAULT_PROFILE) {
        out.insert(0, Profile::default());
    }
    out
}

/// Read one profile by name, falling back to defaults.
pub fn get(data: &Path, version_id: &str, name: &str) -> Profile {
    let p = profile_file(data, version_id, name);
    let mut prof = load_file(&p).unwrap_or_default();
    prof.name = name.to_string();
    prof
}

fn load_file(path: &Path) -> Option<Profile> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut prof = Profile::default();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (k, v) = match line.split_once('=') {
            Some(kv) => kv,
            None => continue,
        };
        let v = v.trim();
        match k.trim() {
            "memory" if !v.is_empty() => prof.memory = v.to_string(),
            "user" if !v.is_empty() => prof.user = v.to_string(),
            "isolate" => prof.isolate = v != "0",
            "width" => prof.width = v.parse().ok(),
            "height" => prof.height = v.parse().ok(),
            "note" => prof.note = v.to_string(),
            _ => {}
        }
    }
    Some(prof)
}

/// Write a profile, creating the profiles directory.
pub fn save(data: &Path, version_id: &str, prof: &Profile) -> Res<()> {
    prof.validate()?;
    let dir = profiles_dir(data, version_id);
    util::mkdirs(&dir)?;
    let mut body = String::from("# LFL 启动配置\n");
    body.push_str(&format!("memory={}\n", prof.memory));
    body.push_str(&format!("user={}\n", prof.user));
    body.push_str(&format!("isolate={}\n", prof.isolate as u8));
    if let (Some(w), Some(h)) = (prof.width, prof.height) {
        body.push_str(&format!("width={w}\nheight={h}\n"));
    }
    if !prof.note.is_empty() {
        body.push_str(&format!("note={}\n", prof.note));
    }
    let target = profile_file(data, version_id, &prof.name);
    let tmp = target.with_extension("ini.tmp");
    std::fs::write(&tmp, body).map_err(|e| format!("写入 {} 失败: {e}", tmp.display()))?;
    if target.exists() {
        let _ = std::fs::remove_file(&target);
    }
    std::fs::rename(&tmp, &target)
        .map_err(|e| format!("替换 {} 失败: {e}", target.display()))
}

/// Delete a profile file. The default profile cannot be deleted.
pub fn delete(data: &Path, version_id: &str, name: &str) -> Res<()> {
    if name == DEFAULT_PROFILE {
        return Err("默认配置不能删除".to_string());
    }
    let p = profile_file(data, version_id, name);
    std::fs::remove_file(&p).map_err(|e| format!("删除 {} 失败: {e}", p.display()))
}

/// Copy `from` to `to`, so a tweaked variant can be derived from a working one.
pub fn duplicate(data: &Path, version_id: &str, from: &str, to: &str) -> Res<Profile> {
    validate_profile_name(to)?;
    if profile_file(data, version_id, to).exists() {
        return Err(format!("配置 {to} 已存在"));
    }
    let mut prof = get(data, version_id, from);
    prof.name = to.to_string();
    save(data, version_id, &prof)?;
    Ok(prof)
}

/// Count how many profiles were explicitly saved (the implicit default excluded).
pub fn count(data: &Path, version_id: &str) -> usize {
    let dir = profiles_dir(data, version_id);
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("ini"))
                .count()
        })
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("lfl-prof-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(p.join("versions").join("1.20.1")).unwrap();
        p
    }

    #[test]
    fn empty_version_yields_only_the_default() {
        let d = temp("empty");
        let list = list(&d, "1.20.1");
        // No files on disk, yet the implicit default must still be offered.
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, DEFAULT_PROFILE);
        let def = get(&d, "1.20.1", DEFAULT_PROFILE);
        assert_eq!(def.name, DEFAULT_PROFILE);
        assert_eq!(def.memory, "2G");
        assert!(def.isolate);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn round_trips_a_profile() {
        let d = temp("round");
        let p = Profile {
            name: "服务器".into(),
            memory: "1G".into(),
            user: "Srv".into(),
            width: Some(1280),
            height: Some(720),
            isolate: false,
            note: "小内存".into(),
        };
        save(&d, "1.20.1", &p).unwrap();
        let got = get(&d, "1.20.1", "服务器");
        assert_eq!(got.memory, "1G");
        assert_eq!(got.user, "Srv");
        assert_eq!(got.width, Some(1280));
        assert!(!got.isolate);
        assert_eq!(got.note, "小内存");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn default_sorts_first() {
        let d = temp("sort");
        for name in [DEFAULT_PROFILE, "b", "a"] {
            let p = Profile {
                name: name.into(),
                ..Profile::default()
            };
            save(&d, "1.20.1", &p).unwrap();
        }
        let names: Vec<String> = list(&d, "1.20.1").into_iter().map(|p| p.name).collect();
        assert_eq!(names, vec![DEFAULT_PROFILE, "a", "b"]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn duplicate_copies_settings_but_not_the_name() {
        let d = temp("dup");
        let p = Profile {
            name: "base".into(),
            memory: "4G".into(),
            ..Profile::default()
        };
        save(&d, "1.20.1", &p).unwrap();
        let copy = duplicate(&d, "1.20.1", "base", "copy").unwrap();
        assert_eq!(copy.name, "copy");
        assert_eq!(copy.memory, "4G");
        assert!(duplicate(&d, "1.20.1", "base", "copy").is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn default_profile_cannot_be_deleted() {
        let d = temp("deldef");
        assert!(delete(&d, "1.20.1", DEFAULT_PROFILE).is_err());
        let p = Profile {
            name: "x".into(),
            ..Profile::default()
        };
        save(&d, "1.20.1", &p).unwrap();
        assert!(delete(&d, "1.20.1", "x").is_ok());
        assert!(!profile_file(&d, "1.20.1", "x").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn profile_names_are_sanitized() {
        for good in ["默认", "server", "1G 小内存", "a-b_c"] {
            assert!(validate_profile_name(good).is_ok(), "{good}");
        }
        for bad in ["", "  ", ".", "..", "a/b", "a|b", "a?b", "con"] {
            // "con" is a reserved device name but a valid *component* here; the
            // file is `con.ini`, which Windows also rejects, so it must fail.
            let ok = validate_profile_name(bad).is_ok();
            assert!(!ok, "should reject {bad:?}");
        }
    }

    #[test]
    fn validates_memory_and_user() {
        let mut p = Profile::default();
        assert!(p.validate().is_ok());
        p.memory = "nonsense".into();
        assert!(p.validate().is_err());
        p.memory = "2G".into();
        p.user = String::new();
        assert!(p.validate().is_err());
        p.user = "ok".into();
        p.width = Some(800);
        assert!(p.validate().is_err(), "width without height");
        let _ = (p, &temp("noop"));
    }

    #[test]
    fn count_reports_only_ini_files() {
        let d = temp("count");
        assert_eq!(count(&d, "1.20.1"), 0);
        save(
            &d,
            "1.20.1",
            &Profile {
                name: "a".into(),
                ..Profile::default()
            },
        )
        .unwrap();
        assert_eq!(count(&d, "1.20.1"), 1);
        // A stray non-ini file must not be counted.
        std::fs::write(profiles_dir(&d, "1.20.1").join("junk.txt"), b"x").unwrap();
        assert_eq!(count(&d, "1.20.1"), 1);
        let _ = std::fs::remove_dir_all(&d);
    }
}
