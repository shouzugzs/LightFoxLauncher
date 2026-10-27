// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025 LightFoxLauncher contributors
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Local Minecraft version management: inspect, delete, clone and rename the
//! version trees under `<data>/versions/`.
//!
//! "Manage" means acting on what is already on disk, so every operation here is
//! local-only and never touches the network. Deleting a version also cleans the
//! native jar extraction it owns, but never removes shared `libraries/`,
//! `assets/` or `runtime/` — those are used by every other version.
//!
//! OWNER: lead.

use std::path::{Path, PathBuf};

use crate::util::{self, Res};
use crate::version;

/// A version directory with its measured disk usage.
#[derive(Debug, Clone, PartialEq)]
pub struct LocalVersion {
    pub id: String,
    /// `versions/<id>`
    pub dir: PathBuf,
    /// Bytes used by the whole version directory.
    pub size: u64,
    /// The client jar exists and is non-empty.
    pub has_jar: bool,
    /// The version JSON exists and parses.
    pub has_json: bool,
    /// Count of `.jar` files under `libraries/` referenced by this version is
    /// not measured (shared); this is just the per-version byte count.
    pub natives: usize,
}

impl LocalVersion {
    /// `1.20.1 — 42.3 MB` for display.
    pub fn label(&self) -> String {
        format!("{} — {}", self.id, util::human_size(self.size))
    }

    /// A row is usable when both the JSON and the jar are present.
    pub fn launchable(&self) -> bool {
        self.has_json && self.has_jar
    }
}

/// Everything under `<data>/versions/` that looks like a version directory.
pub fn list(dir: &Path) -> Vec<LocalVersion> {
    let root = version::versions_dir(dir);
    let mut out = Vec::new();
    let rd = match std::fs::read_dir(&root) {
        Ok(r) => r,
        Err(_) => return out,
    };
    for e in rd.flatten() {
        if !e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let id = match e.file_name().to_str() {
            Some(s) => s.to_string(),
            None => continue,
        };
        let vdir = e.path();
        let jar = vdir.join(format!("{id}.jar"));
        let json = vdir.join(format!("{id}.json"));
        let has_jar = util::file_size(&jar) > 0;
        let has_json = json.is_file();
        let size = crate::launcher::dir_size(&vdir);
        let natives = vdir.join("natives").is_dir();
        out.push(LocalVersion {
            id,
            dir: vdir,
            size,
            has_jar,
            has_json,
            natives: if natives { 1 } else { 0 },
        });
    }
    out.sort_by(|a, b| b.id.cmp(&a.id));
    out
}

/// Total bytes used by all versions.
pub fn total_size(list: &[LocalVersion]) -> u64 {
    list.iter().map(|v| v.size).sum()
}

/// Delete a version directory.
///
/// Refuses to delete `default`-looking shared data: only the directory
/// `versions/<id>` and nothing outside it is removed. Returns the bytes freed.
pub fn delete(dir: &Path, id: &str) -> Res<u64> {
    // Reject the id *before* touching the filesystem: `canonicalize` only works
    // on paths that exist, so validating by canonical form made legitimate
    // deletes fail whenever the two sides resolved differently.
    validate_existing_id(id)?;
    let root = version::versions_dir(dir);
    let target = root.join(id);
    if !target.is_dir() {
        return Err(format!("版本 {id} 不存在"));
    }
    // Belt and braces: the joined path must still sit directly under `versions/`.
    let root_norm = normalize_lexically(&root);
    let target_norm = normalize_lexically(&target);
    match target_norm.parent() {
        Some(p) if p == root_norm => {}
        _ => return Err(format!("拒绝删除 {id}：路径不在 versions/ 内")),
    }
    let freed = crate::launcher::dir_size(&target);
    std::fs::remove_dir_all(&target)
        .map_err(|e| format!("删除 {} 失败: {e}", target.display()))?;
    Ok(freed)
}

/// An id that can name an existing version: a single, safe path component.
pub fn validate_existing_id(id: &str) -> Res<()> {
    let id = id.trim();
    if id.is_empty() {
        return Err("版本名不能为空".to_string());
    }
    if id == "." || id == ".." {
        return Err(format!("非法版本名 '{id}'"));
    }
    let mut comps = std::path::Path::new(id).components();
    if !matches!(
        (comps.next(), comps.next()),
        (Some(std::path::Component::Normal(_)), None)
    ) {
        return Err(format!("拒绝 '{id}'：路径不在 versions/ 内"));
    }
    Ok(())
}

/// Resolve `.`/`..` and repeated separators without touching the filesystem.
///
/// `canonicalize` would additionally expand 8.3 short names and add a `\\?\`
/// verbatim prefix, which makes prefix comparison unreliable on Windows; for a
/// containment check between a directory and its own child, lexical
/// normalisation is both sufficient and stable.
fn normalize_lexically(p: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Copy `from` to a new version id `to`, rewriting the id inside the JSON.
///
/// The jar is renamed too, so the copy is a self-contained version directory —
/// which is what lets a user keep a modded snapshot of a vanilla install.
pub fn clone_version(dir: &Path, from: &str, to: &str) -> Res<PathBuf> {
    validate_new_id(to)?;
    let root = version::versions_dir(dir);
    let src = root.join(from);
    if !src.is_dir() {
        return Err(format!("源版本 {from} 不存在"));
    }
    let dst = root.join(to);
    if dst.exists() {
        return Err(format!("目标版本 {to} 已存在"));
    }
    copy_tree(&src, &dst)?;

    // Rename the jar and JSON to the new id, and patch the JSON's `id`.
    let old_jar = dst.join(format!("{from}.jar"));
    let new_jar = dst.join(format!("{to}.jar"));
    if old_jar.is_file() {
        std::fs::rename(&old_jar, &new_jar)
            .map_err(|e| format!("重命名 jar 失败: {e}"))?;
    }
    let old_json = dst.join(format!("{from}.json"));
    let new_json = dst.join(format!("{to}.json"));
    if old_json.is_file() {
        let text = std::fs::read_to_string(&old_json)
            .map_err(|e| format!("读取 {} 失败: {e}", old_json.display()))?;
        let patched = patch_json_id(&text, from, to);
        std::fs::write(&new_json, patched)
            .map_err(|e| format!("写入 {} 失败: {e}", new_json.display()))?;
        let _ = std::fs::remove_file(&old_json);
    }
    // The natives marker embeds the jar list, not the id; drop it so the copy
    // re-extracts into its own natives dir on first launch.
    let _ = std::fs::remove_file(dst.join("natives").join(".lfl-natives-ok"));
    Ok(dst)
}

/// Replace the top-level `"id"` value, leaving every other byte alone.
fn patch_json_id(text: &str, from: &str, to: &str) -> String {
    // A full re-serialisation would reorder and reformat the document; the id
    // is a single well-known key, so a targeted replacement is safer here.
    let needle = format!("\"id\"");
    let mut out = String::with_capacity(text.len() + 16);
    let mut rest = text;
    while let Some(pos) = rest.find(&needle) {
        out.push_str(&rest[..pos + needle.len()]);
        rest = &rest[pos + needle.len()..];
        // Skip whitespace and the colon, then the quoted value.
        let trimmed = rest.trim_start();
        let ws_len = rest.len() - trimmed.len();
        if let Some(after_colon) = trimmed.strip_prefix(':') {
            let v = after_colon.trim_start();
            let v_ws = after_colon.len() - v.len();
            if let Some(stripped) = v.strip_prefix('"') {
                if let Some(end) = stripped.find('"') {
                    let current = &stripped[..end];
                    if current.eq_ignore_ascii_case(from) {
                        out.push_str(&rest[..ws_len]);
                        out.push(':');
                        out.push_str(&after_colon[..v_ws]);
                        out.push('"');
                        out.push_str(to);
                        out.push('"');
                        rest = &stripped[end + 1..];
                        continue;
                    }
                }
            }
        }
        // Not the key we wanted: emit the whitespace/colon untouched.
        out.push_str(&rest[..ws_len]);
        rest = &rest[ws_len..];
    }
    out.push_str(rest);
    out
}

/// Validation for a user-supplied new version id.
pub fn validate_new_id(id: &str) -> Res<()> {
    let id = id.trim();
    if id.is_empty() {
        return Err("版本名不能为空".to_string());
    }
    if id.len() > 64 {
        return Err("版本名最长 64 个字符".to_string());
    }
    crate::launcher::validate_instance_name(id)
        .map_err(|e| e.replace("instance name", "version name"))
}

/// Recursively copy a directory tree.
fn copy_tree(src: &Path, dst: &Path) -> Res<()> {
    util::mkdirs(dst)?;
    let rd = std::fs::read_dir(src).map_err(|e| format!("读取 {} 失败: {e}", src.display()))?;
    for e in rd.flatten() {
        let p = e.path();
        let target = dst.join(e.file_name());
        if p.is_dir() {
            copy_tree(&p, &target)?;
        } else {
            std::fs::copy(&p, &target)
                .map_err(|e| format!("复制 {} 失败: {e}", p.display()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("lfl-vm-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn make_version(root: &Path, id: &str, json: &str) {
        let d = root.join("versions").join(id);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join(format!("{id}.json")), json).unwrap();
        std::fs::write(d.join(format!("{id}.jar")), vec![0u8; 128]).unwrap();
    }

    #[test]
    fn lists_and_measures_versions() {
        let root = temp("list");
        make_version(&root, "1.20.1", r#"{"id":"1.20.1"}"#);
        make_version(&root, "1.8.9", r#"{"id":"1.8.9"}"#);
        // A directory without a jar is still listed, but not launchable.
        std::fs::create_dir_all(root.join("versions").join("broken")).unwrap();
        let list = list(&root);
        assert_eq!(list.len(), 3);
        let v = list.iter().find(|x| x.id == "1.20.1").unwrap();
        assert!(v.launchable());
        assert!(v.size > 0);
        let b = list.iter().find(|x| x.id == "broken").unwrap();
        assert!(!b.launchable());
        assert!(total_size(&list) >= 256);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn delete_removes_only_that_version() {
        let root = temp("del");
        make_version(&root, "1.20.1", r#"{"id":"1.20.1"}"#);
        make_version(&root, "keep", r#"{"id":"keep"}"#);
        let freed = delete(&root, "1.20.1").unwrap();
        assert!(freed > 0);
        assert!(!root.join("versions").join("1.20.1").exists());
        assert!(root.join("versions").join("keep").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn delete_rejects_escaping_ids() {
        let root = temp("escape");
        make_version(&root, "1.20.1", r#"{"id":"1.20.1"}"#);
        for bad in ["../1.20.1", "..", "", "a/b"] {
            assert!(delete(&root, bad).is_err(), "should reject {bad:?}");
        }
        // The legit version must still be there.
        assert!(root.join("versions").join("1.20.1").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn clone_rewrites_id_and_renames_files() {
        let root = temp("clone");
        make_version(&root, "1.20.1", r#"{"id":"1.20.1","name":"1.20.1"}"#);
        let dst = clone_version(&root, "1.20.1", "1.20.1-modded").unwrap();
        assert!(dst.join("1.20.1-modded.jar").is_file());
        assert!(!dst.join("1.20.1.jar").exists());
        let text = std::fs::read_to_string(dst.join("1.20.1-modded.json")).unwrap();
        assert!(text.contains(r#""id":"1.20.1-modded""#), "{text}");
        // Other fields are untouched.
        assert!(text.contains(r#""name":"1.20.1""#), "{text}");
        // Source is intact.
        assert!(root.join("versions").join("1.20.1").join("1.20.1.jar").is_file());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn clone_refuses_existing_target() {
        let root = temp("clonedup");
        make_version(&root, "a", r#"{"id":"a"}"#);
        make_version(&root, "b", r#"{"id":"b"}"#);
        assert!(clone_version(&root, "a", "b").is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn new_id_validation() {
        for good in ["1.20.1-modded", "test", "我的版本"] {
            assert!(validate_new_id(good).is_ok(), "should accept {good:?}");
        }
        for bad in ["", "   ", "a/b", "..", "con"] {
            assert!(validate_new_id(bad).is_err(), "should reject {bad:?}");
        }
    }

    #[test]
    fn patch_leaves_other_keys_alone() {
        let src = r#"{"id":"old","inheritsFrom":"old","name":"old keeps"}"#;
        let out = patch_json_id(src, "old", "new");
        assert!(out.contains(r#""id":"new""#), "{out}");
        assert!(out.contains(r#""inheritsFrom":"old""#), "{out}");
        assert!(out.contains(r#""name":"old keeps""#), "{out}");
    }
}
