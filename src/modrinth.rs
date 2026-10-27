// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025 LightFoxLauncher contributors
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Mod search and download from Modrinth.
//!
//! Modrinth's v2 API is public (no key, unlike CurseForge), returns `sha1` for
//! every file, and supports facets for game version + loader — so the whole
//! feature can reuse the existing verified download path.
//!
//! OWNER: lead.

use std::path::{Path, PathBuf};

use crate::json::Json;
use crate::util::{self, Res};

const API: &str = "https://api.modrinth.com/v2";

/// A search result.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchHit {
    pub project_id: String,
    pub slug: String,
    pub title: String,
    pub description: String,
    pub downloads: u64,
    pub author: String,
    /// Loader tags among the project's categories (`fabric`, `forge`, …).
    pub loaders: Vec<String>,
}

impl SearchHit {
    /// `Sodium — A high-performance rendering engine…`
    pub fn label(&self) -> String {
        format!("{} ({})", self.title, util::human_size(self.downloads))
    }
}

/// One downloadable file of a project.
#[derive(Debug, Clone, PartialEq)]
pub struct ModFile {
    pub project_id: String,
    pub version_name: String,
    pub version_number: String,
    pub filename: String,
    pub url: String,
    pub sha1: String,
    pub size: u64,
    pub loaders: Vec<String>,
    pub game_versions: Vec<String>,
}

impl ModFile {
    pub fn label(&self) -> String {
        format!(
            "{} · {} · {}",
            self.version_name,
            self.game_versions.join("/"),
            util::human_size(self.size)
        )
    }
}

/// Percent-encode a query string component (RFC 3986 unreserved set kept).
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Build the search URL for `query` restricted to `mc` + `loader`.
///
/// Facets are Modrinth's filter syntax: an array of OR-groups that are ANDed.
/// Passing an empty `loader` or `mc` simply omits that filter.
pub fn search_url(query: &str, mc: &str, loader: &str, limit: u32) -> String {
    let mut facets: Vec<String> = vec![r#"["project_type:mod"]"#.to_string()];
    if !loader.is_empty() {
        facets.push(format!(r#"["categories:{}"]"#, encode(loader)));
    }
    if !mc.is_empty() {
        facets.push(format!(r#"["versions:{}"]"#, encode(mc)));
    }
    let facets_json = format!("[{}]", facets.join(","));
    format!(
        "{API}/search?query={}&limit={}&index=relevance&facets={}",
        encode(query),
        limit.clamp(1, 100),
        encode(&facets_json)
    )
}

fn strings(v: Option<&Json>) -> Vec<String> {
    v.and_then(|a| a.as_arr())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

fn hit_from(j: &Json) -> Option<SearchHit> {
    let project_id = j.str_at("project_id")?;
    let title = j.str_at("title")?;
    let categories = strings(j.get("categories"));
    let loaders = categories
        .iter()
        .filter(|c| {
            matches!(
                c.as_str(),
                "fabric" | "quilt" | "forge" | "neoforge"
            )
        })
        .cloned()
        .collect();
    Some(SearchHit {
        project_id: project_id.to_string(),
        slug: j.str_at("slug").unwrap_or("").to_string(),
        title: title.to_string(),
        description: j.str_at("description").unwrap_or("").to_string(),
        downloads: j.u64_at("downloads").unwrap_or(0),
        author: j.str_at("author").unwrap_or("").to_string(),
        loaders,
    })
}

/// Search Modrinth. Returns up to `limit` hits.
pub fn search(query: &str, mc: &str, loader: &str, limit: u32) -> Res<Vec<SearchHit>> {
    let url = search_url(query, mc, loader, limit);
    let body = crate::net::get(&url)?;
    let text = String::from_utf8_lossy(&body);
    let json = Json::parse(&text).map_err(|e| format!("cannot parse search results: {e}"))?;
    let hits = json
        .arr_at("hits")
        .ok_or_else(|| "search response has no 'hits'".to_string())?;
    Ok(hits.iter().filter_map(hit_from).collect())
}

/// Files of `project` matching `mc` and `loader`, newest first.
pub fn versions(project: &str, mc: &str, loader: &str) -> Res<Vec<ModFile>> {
    let mut url = format!("{API}/project/{}/version", encode(project));
    let mut filters = Vec::new();
    if !mc.is_empty() {
        filters.push(format!("game_versions={}", encode(&format!(r#"["{mc}"]"#))));
    }
    if !loader.is_empty() {
        filters.push(format!("loaders={}", encode(&format!(r#"["{loader}"]"#))));
    }
    if !filters.is_empty() {
        url.push('?');
        url.push_str(&filters.join("&"));
    }
    let body = crate::net::get(&url)?;
    let text = String::from_utf8_lossy(&body);
    let json = Json::parse(&text).map_err(|e| format!("cannot parse version list: {e}"))?;
    let arr = json
        .as_arr()
        .ok_or_else(|| "version response is not an array".to_string())?;
    Ok(arr.iter().filter_map(|v| parse_version(v, project)).collect())
}

/// One specific version by its Modrinth version id.
///
/// Used when a dependency records the exact build it was tested against, which
/// is more reliable than guessing the newest one.
pub fn version_by_id(version_id: &str) -> Res<ModFile> {
    let url = format!("{API}/version/{}", encode(version_id));
    let body = crate::net::get(&url)?;
    let text = String::from_utf8_lossy(&body);
    let json = Json::parse(&text).map_err(|e| format!("cannot parse version {version_id}: {e}"))?;
    parse_version(&json, "").ok_or_else(|| format!("version {version_id} has no downloadable file"))
}

/// Resolve a mod id / slug / file stem to a Modrinth project id.
///
/// `Ok(None)` means "not on Modrinth", which callers treat as "nothing to check"
/// rather than an error — many mods are distributed elsewhere.
pub fn find_project(id_or_slug: &str) -> Res<Option<String>> {
    let q = id_or_slug.trim();
    if q.is_empty() {
        return Ok(None);
    }
    // Search by the exact name first; the top hit is almost always right for a
    // mod id, and it avoids a 404 on `/project/<id>` for non-Modrinth mods.
    match search(q, "", "", 5) {
        Ok(hits) => {
            let exact = hits
                .iter()
                .find(|h| h.slug.eq_ignore_ascii_case(q) || h.title.eq_ignore_ascii_case(q));
            if let Some(h) = exact {
                return Ok(Some(h.project_id.clone()));
            }
            Ok(hits.first().map(|h| h.project_id.clone()))
        }
        Err(e) => Err(e),
    }
}

/// The full JSON object for one version of a project, matched by
/// `version_number`; falls back to the newest version when no exact match.
///
/// Dependency resolution needs the raw object because the `dependencies` array
/// is not carried on [`ModFile`].
pub fn version_json(project: &str, version_number: &str) -> Res<Json> {
    let url = format!("{API}/project/{}/version", encode(project));
    let body = crate::net::get(&url)?;
    let text = String::from_utf8_lossy(&body);
    let json = Json::parse(&text).map_err(|e| format!("cannot parse versions of {project}: {e}"))?;
    let arr = json
        .as_arr()
        .ok_or_else(|| format!("{project}: version list is not an array"))?;
    arr.iter()
        .find(|v| v.str_at("version_number").map(|n| n == version_number).unwrap_or(false))
        .or_else(|| arr.first())
        .cloned()
        .ok_or_else(|| format!("{project}: no versions"))
}

/// A project's display title, for dependency reports.
pub fn project_title(project_id: &str) -> Res<String> {
    // Modrinth accepts either the id or the slug here.
    let url = format!("{API}/project/{}", encode(project_id));
    let body = crate::net::get(&url)?;
    let text = String::from_utf8_lossy(&body);
    let json = Json::parse(&text).map_err(|e| format!("cannot parse project {project_id}: {e}"))?;
    json.str_at("title")
        .map(|s| s.to_string())
        .ok_or_else(|| format!("project {project_id} has no title"))
}

/// Map one Modrinth version object to a [`ModFile`].
///
/// `fallback_project` is used when the object omits `project_id` (it is present
/// in practice, but a missing field must not panic).
fn parse_version(v: &Json, fallback_project: &str) -> Option<ModFile> {
    let files = v.arr_at("files")?;
    if files.is_empty() {
        return None;
    }
    let file = files
        .iter()
        .find(|f| f.get("primary").and_then(|p| p.as_bool()).unwrap_or(false))
        .unwrap_or(&files[0]);
    let url = file.str_at("url").filter(|u| !u.is_empty())?.to_string();
    let filename = file.str_at("filename").unwrap_or("mod.jar").to_string();
    if !filename.to_ascii_lowercase().ends_with(".jar") {
        return None;
    }
    Some(ModFile {
        project_id: v
            .str_at("project_id")
            .filter(|s| !s.is_empty())
            .unwrap_or(fallback_project)
            .to_string(),
        version_name: v.str_at("name").unwrap_or("").to_string(),
        version_number: v.str_at("version_number").unwrap_or("").to_string(),
        filename,
        url,
        sha1: file
            .path(&["hashes", "sha1"])
            .and_then(|h| h.as_str())
            .unwrap_or("")
            .to_string(),
        size: file.u64_at("size").unwrap_or(0),
        loaders: strings(v.get("loaders")),
        game_versions: strings(v.get("game_versions")),
    })
}

/// Download a mod file into `mods`, verifying its sha1.
///
/// Refuses to overwrite: an existing file with the same name is reported rather
/// than replaced, matching `loaders::install_mod`.
///
/// `f.filename` is whatever the Modrinth API returned. It is checked to be a
/// single safe file name first: without that, a `filename` of `../../../x.jar`
/// (or an absolute path) would place the download outside `mods/`.
pub fn download_file(mods: &Path, f: &ModFile) -> Res<PathBuf> {
    util::safe_component(&f.filename)
        .map_err(|e| format!("Modrinth 返回了不安全的文件名：{e}"))?;
    util::mkdirs(mods)?;
    let dest = mods.join(&f.filename);
    if dest.exists() && (f.sha1.is_empty() || crate::hash::file_matches(&dest, &f.sha1)) {
        return Err(format!("已存在 {}", dest.display()));
    }
    crate::net::download(&f.url, &dest, &f.sha1, f.size)?;
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_url_encodes_query_and_facets() {
        let u = search_url("sodium extra", "1.20.1", "fabric", 20);
        assert!(u.starts_with("https://api.modrinth.com/v2/search?"), "{u}");
        assert!(u.contains("query=sodium%20extra"), "{u}");
        assert!(u.contains("limit=20"), "{u}");
        // Facets are JSON, so they must be percent-encoded, not raw.
        assert!(u.contains("facets=%5B"), "{u}");
        assert!(!u.contains("facets=["), "facets must be encoded: {u}");
    }

    #[test]
    fn search_url_omits_empty_filters() {
        let u = search_url("x", "", "", 5);
        assert!(!u.contains("versions%3A"), "{u}");
        assert!(!u.contains("categories%3A"), "{u}");
        assert!(u.contains("project_type%3Amod"), "{u}");
    }

    #[test]
    fn search_url_clamps_limit() {
        assert!(search_url("x", "", "", 0).contains("limit=1"));
        assert!(search_url("x", "", "", 9999).contains("limit=100"));
    }

    #[test]
    fn encode_is_rfc3986() {
        assert_eq!(encode("a b"), "a%20b");
        assert_eq!(encode("1.20.1"), "1.20.1");
        assert_eq!(encode("a+b"), "a%2Bb");
        assert_eq!(encode("中"), "%E4%B8%AD");
    }

    #[test]
    fn parses_a_search_hit() {
        let doc = r#"{"hits":[{
            "project_id":"AANobbMI","slug":"sodium","title":"Sodium",
            "description":"Better frames","downloads":254921362,
            "categories":["fabric","optimization","quilt"],"author":"jellysquid3"
        }]}"#;
        let json = Json::parse(doc).unwrap();
        let hit = hit_from(&json.arr_at("hits").unwrap()[0]).unwrap();
        assert_eq!(hit.title, "Sodium");
        assert_eq!(hit.slug, "sodium");
        assert_eq!(hit.loaders, vec!["fabric", "quilt"]);
        // Non-loader categories are dropped from `loaders`.
        assert!(!hit.loaders.contains(&"optimization".to_string()));
        assert!(hit.label().contains("Sodium"));
    }

    #[test]
    fn download_file_refuses_existing_verified_file() {
        let dir = std::env::temp_dir().join(format!("lfl-modrinth-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let f = ModFile {
            project_id: "p".into(),
            version_name: "v".into(),
            version_number: "1".into(),
            filename: "existing.jar".into(),
            url: "https://example.invalid/x.jar".into(),
            sha1: String::new(),
            size: 1,
            loaders: vec![],
            game_versions: vec![],
        };
        std::fs::write(dir.join("existing.jar"), b"x").unwrap();
        // The file is already there, so no network access must happen.
        let err = download_file(&dir, &f).expect_err("must refuse");
        assert!(err.contains("已存在"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
