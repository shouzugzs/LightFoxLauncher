// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025 LightFoxLauncher contributors
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Asset index parsing and object downloads.
//! OWNER: meta. Public API frozen by `docs/CONTRACT.md`.
//!
//! Layout: index at `<dir>/assets/indexes/<id>.json`, objects at
//! `<dir>/assets/objects/<h[0..2]>/<h>`, served from
//! `https://resources.download.minecraft.net/<h[0..2]>/<h>`.

use std::path::{Path, PathBuf};

use crate::json::Json;
use crate::net::{self, Download};
use crate::util::{file_size, info, mkdirs, Res};
use crate::version::AssetIndex;

const RESOURCE_BASE: &str = "https://resources.download.minecraft.net/";
/// Cap on a single `net::download_many` call (bounds peak memory per batch).
const BATCH: usize = 500;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetObject {
    pub hash: String,
    pub size: u64,
}

/// `<dir>/assets`
pub fn assets_dir(dir: &Path) -> PathBuf {
    dir.join("assets")
}

/// `<dir>/assets/indexes`
pub fn indexes_dir(dir: &Path) -> PathBuf {
    assets_dir(dir).join("indexes")
}

/// `<dir>/assets/objects`
pub fn objects_dir(dir: &Path) -> PathBuf {
    assets_dir(dir).join("objects")
}

/// `<dir>/assets/objects/<h[0..2]>/<h>`; `None` for a malformed hash.
///
/// The hash comes out of the (remotely fetched) asset index, so it is validated
/// as a hex digest before being used as two path components. Accepting any
/// two-character string let `..` through and escaped `objects/`.
pub fn object_path(dir: &Path, hash: &str) -> Option<PathBuf> {
    let h = hash.to_ascii_lowercase();
    if h.len() < 2 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some(objects_dir(dir).join(&h[0..2]).join(&h))
}

/// `<dir>/assets/indexes/<id>.json`
///
/// `id` comes from the version JSON, so it must be a single safe component; it
/// used to be interpolated straight into a path.
pub fn index_path(dir: &Path, id: &str) -> PathBuf {
    match crate::util::safe_component(id) {
        Ok(()) => indexes_dir(dir).join(format!("{id}.json")),
        Err(_) => {
            // A name that can neither collide with a real index nor escape the
            // directory; the sha1 check then fails loudly instead of the write
            // landing somewhere unexpected.
            crate::util::info(&format!("assets: refusing unsafe index id {id:?}"));
            indexes_dir(dir).join("invalid-index.json")
        }
    }
}

/// Download URL of one object. The hash is already lowercase in real indexes;
/// lowercasing here keeps the URL and the on-disk name in sync.
///
/// The slice is byte-indexed, so it must be guarded: a non-hex hash from a
/// tampered index (e.g. `aé`, where byte 2 lands inside a multi-byte character)
/// used to panic here — and with `panic = "abort"` that kills the process.
pub fn object_url(hash: &str) -> String {
    let h = hash.to_ascii_lowercase();
    if !h.bytes().all(|b| b.is_ascii_hexdigit()) {
        return String::new();
    }
    let prefix = if h.len() >= 2 { &h[..2] } else { &h[..] };
    format!("{RESOURCE_BASE}{prefix}/{h}")
}

fn parse_objects(text: &str) -> Res<Vec<AssetObject>> {
    let root = Json::parse(text).map_err(|e| format!("asset index: {e}"))?;
    let objs = root
        .get("objects")
        .and_then(|o| o.as_obj())
        .ok_or_else(|| "asset index: missing \"objects\" object".to_string())?;
    let mut out = Vec::with_capacity(objs.len());
    for (key, v) in objs {
        // The map key *is* the hash; prefer it over the duplicated "hash" field
        // so a truncated entry still yields a usable object.
        let hash = match v.str_at("hash") {
            Some(h) if !h.is_empty() => h.to_ascii_lowercase(),
            _ => key.to_ascii_lowercase(),
        };
        if hash.len() < 2 {
            continue;
        }
        out.push(AssetObject {
            hash,
            size: v.u64_at("size").unwrap_or(0),
        });
    }
    Ok(out)
}

/// Load (downloading if needed) the index and return its objects.
pub fn load_index(dir: &Path, index: &AssetIndex) -> Res<Vec<AssetObject>> {
    if index.id.is_empty() {
        return Err("asset index: version JSON has no asset index id".to_string());
    }
    let dest = index_path(dir, &index.id);
    if let Some(parent) = dest.parent() {
        mkdirs(parent)?;
    }
    // Re-download when the file is missing, has the wrong size, or its hash no
    // longer matches the manifest. `net::download` already skips a matching file.
    let need = !dest.exists() || (index.size > 0 && file_size(&dest) != index.size);
    if need {
        if index.url.is_empty() {
            return Err(format!(
                "asset index '{}': missing {} and the version JSON has no url",
                index.id,
                dest.display()
            ));
        }
        net::download(&index.url, &dest, &index.sha1, index.size)
            .map_err(|e| format!("asset index '{}': {e}", index.id))?;
    }
    let text = std::fs::read_to_string(&dest)
        .map_err(|e| format!("cannot read {}: {e}", dest.display()))?;
    parse_objects(&text).map_err(|e| format!("{}: {e}", dest.display()))
}

/// Download every object that is missing or has the wrong size.
/// Returns the number of objects present afterwards.
pub fn ensure(dir: &Path, index: &AssetIndex) -> Res<usize> {
    let objects = load_index(dir, index)?;
    if objects.is_empty() {
        return Ok(0);
    }
    mkdirs(&objects_dir(dir))?;

    let mut items: Vec<Download> = Vec::new();
    let mut present = 0usize;
    for o in &objects {
        let dest = match object_path(dir, &o.hash) {
            Some(p) => p,
            None => continue,
        };
        // Trust a non-empty file of the right size; a size-0 index entry still
        // requires existence. `net::download` verifies the sha1 when it runs.
        let on_disk = file_size(&dest);
        if on_disk > 0 && (o.size == 0 || on_disk == o.size) {
            present += 1;
            continue;
        }
        // `object_path` already rejected non-hex hashes, so this slice is on an
        // ASCII string; keep the bound explicit rather than relying on that.
        let label = &o.hash[..o.hash.len().min(8)];
        let mut d = Download::new(&object_url(&o.hash), dest, &o.hash, o.size);
        d.label = label.to_string();
        items.push(d);
    }

    let total = items.len();
    if total > 0 {
        info(&format!(
            "       assets: {present} present, {} to download",
            total
        ));
        for chunk in items.chunks(BATCH) {
            net::download_many(chunk).map_err(|e| format!("downloading assets: {e}"))?;
        }
        present += total;
    }
    Ok(present)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// A miniature index with the same shape as `assets/indexes/5.json`.
    const INDEX: &str = r#"{
      "objects": {
        "000b97ba1e3cf11e7a7b993e9e1a25ada80d2b42": {"hash": "000b97ba1e3cf11e7a7b993e9e1a25ada80d2b42", "size": 4571},
        "0011a4f7e39b0ea6a0b6b1b0d6f7a1a7d6b0b0c1": {"hash": "0011a4f7e39b0ea6a0b6b1b0d6f7a1a7d6b0b0c1", "size": 12},
        "abcdef0123456789abcdef0123456789abcdef01": {"hash": "abcdef0123456789abcdef0123456789abcdef01", "size": 0}
      }
    }"#;

    fn index_fixture() -> AssetIndex {
        AssetIndex {
            id: "5".into(),
            sha1: "8a45c6be38ac9486b04944bffcb96f8aae1b0649".into(),
            size: 413067,
            total_size: 651728646,
            url: "https://piston-meta.mojang.com/v1/packages/8a45/5.json".into(),
        }
    }

    fn test_dir(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "lfl-meta-assets-{tag}-{}-{}",
            std::process::id(),
            crate::util::now_ms()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn parses_objects_list() {
        let objs = parse_objects(INDEX).unwrap();
        assert_eq!(objs.len(), 3);
        assert_eq!(objs[0].hash, "000b97ba1e3cf11e7a7b993e9e1a25ada80d2b42");
        assert_eq!(objs[0].size, 4571);
        assert_eq!(objs[2].size, 0);
        assert!(objs.iter().all(|o| o.hash.len() == 40));
    }

    #[test]
    fn object_paths_and_urls() {
        let dir = Path::new("D:/LFL");
        let h = "000b97ba1e3cf11e7a7b993e9e1a25ada80d2b42";
        assert_eq!(
            object_path(dir, h).unwrap(),
            Path::new("D:/LFL/assets/objects/00/000b97ba1e3cf11e7a7b993e9e1a25ada80d2b42")
        );
        assert_eq!(
            object_url(h),
            "https://resources.download.minecraft.net/00/000b97ba1e3cf11e7a7b993e9e1a25ada80d2b42"
        );
        // uppercase hashes are normalized so URL == on-disk name
        let up = h.to_ascii_uppercase();
        assert_eq!(object_url(&up), object_url(h));
        assert_eq!(object_path(dir, &up).unwrap().file_name().unwrap(), h);
        assert!(object_path(dir, "a").is_none());
        assert!(object_path(dir, "").is_none());
    }

    #[test]
    fn object_url_never_panics_on_non_ascii() {
        // A tampered index can supply any string as a hash. `&h[..2]` is a byte
        // slice, so a multi-byte character straddling byte 2 used to panic —
        // fatal under `panic = "abort"`.
        for bad in ["aé", "é", "日本", "..", "zz"] {
            let u = object_url(bad);
            assert!(u.is_empty(), "should reject {bad:?}, got {u:?}");
        }
        // A genuine hash still produces the split-directory URL.
        assert!(object_url("00abcdef").ends_with("/00/00abcdef"));
    }

    #[test]
    fn object_path_rejects_non_hex() {
        // The hash comes from the remote asset index. `..` used to pass the old
        // `len >= 2 && is_ascii` check and produced
        // `<dir>/assets/objects/../..`, which resolves above the data dir.
        let dir = Path::new("D:/LFL");
        for bad in ["..", "../..", "zz", "a/", "a\\b", "./", "0g", "..x"] {
            assert!(object_path(dir, bad).is_none(), "should reject {bad:?}");
        }
    }

    #[test]
    fn index_path_rejects_escapes() {
        // `index.id` comes from the version JSON.
        let dir = Path::new("D:/LFL");
        for bad in ["../../evil", "C:/Windows/evil", "a/b", ".."] {
            let p = index_path(dir, bad);
            let expected_parent = indexes_dir(dir);
            assert_eq!(
                p.parent(),
                Some(expected_parent.as_path()),
                "{bad:?} escaped the indexes directory: {p:?}"
            );
        }
        assert_eq!(index_path(dir, "17").file_name().unwrap(), "17.json");
    }

    #[test]
    fn index_path_and_dirs() {
        let dir = Path::new("D:/LFL");
        assert_eq!(index_path(dir, "5"), Path::new("D:/LFL/assets/indexes/5.json"));
        assert_eq!(indexes_dir(dir), Path::new("D:/LFL/assets/indexes"));
        assert_eq!(objects_dir(dir), Path::new("D:/LFL/assets/objects"));
        assert_eq!(assets_dir(dir), Path::new("D:/LFL/assets"));
    }

    #[test]
    fn load_index_reads_file_from_disk_when_size_matches() {
        let dir = test_dir("load");
        let dest = index_path(&dir, "5");
        mkdirs(dest.parent().unwrap()).unwrap();
        std::fs::write(&dest, INDEX).unwrap();
        let mut idx = index_fixture();
        idx.size = std::fs::metadata(&dest).unwrap().len();
        let objs = load_index(&dir, &idx).unwrap();
        assert_eq!(objs.len(), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_index_errors_cleanly_without_url() {
        let dir = test_dir("nourl");
        let mut idx = index_fixture();
        idx.url = String::new(); // offline, nothing on disk
        let e = load_index(&dir, &idx).unwrap_err();
        assert!(e.contains("asset index '5'"), "{e}");
        assert!(e.contains("no url"), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_index_rejects_empty_id() {
        let dir = test_dir("noid");
        let e = load_index(&dir, &AssetIndex::default()).unwrap_err();
        assert!(e.contains("no asset index id"), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ensure_counts_existing_objects() {
        let dir = test_dir("ensure");
        let dest = index_path(&dir, "5");
        mkdirs(dest.parent().unwrap()).unwrap();
        std::fs::write(&dest, INDEX).unwrap();
        let mut idx = index_fixture();
        idx.size = std::fs::metadata(&dest).unwrap().len();

        // pre-create two of the three objects with the right sizes
        for (h, size) in [
            ("000b97ba1e3cf11e7a7b993e9e1a25ada80d2b42", 4571usize),
            ("0011a4f7e39b0ea6a0b6b1b0d6f7a1a7d6b0b0c1", 12),
        ] {
            let p = object_path(&dir, h).unwrap();
            mkdirs(p.parent().unwrap()).unwrap();
            std::fs::write(&p, vec![0u8; size]).unwrap();
        }
        // the third one (size 0 in the index) must be treated as missing
        let missing = object_path(&dir, "abcdef0123456789abcdef0123456789abcdef01").unwrap();
        assert!(!missing.exists());

        // one object is genuinely missing => network needed; the stub returns
        // Err, so this must surface as a clean error, not a panic.
        let r = ensure(&dir, &idx);
        match r {
            Ok(n) => assert_eq!(n, 3),
            Err(e) => assert!(e.contains("downloading assets"), "{e}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ensure_all_present_is_offline_ok() {
        let dir = test_dir("allpresent");
        let dest = index_path(&dir, "5");
        mkdirs(dest.parent().unwrap()).unwrap();
        let two = r#"{"objects":{
            "000b97ba1e3cf11e7a7b993e9e1a25ada80d2b42":{"hash":"000b97ba1e3cf11e7a7b993e9e1a25ada80d2b42","size":4571},
            "0011a4f7e39b0ea6a0b6b1b0d6f7a1a7d6b0b0c1":{"hash":"0011a4f7e39b0ea6a0b6b1b0d6f7a1a7d6b0b0c1","size":12}}}"#;
        std::fs::write(&dest, two).unwrap();
        let mut idx = index_fixture();
        idx.size = std::fs::metadata(&dest).unwrap().len();
        for (h, size) in [
            ("000b97ba1e3cf11e7a7b993e9e1a25ada80d2b42", 4571usize),
            ("0011a4f7e39b0ea6a0b6b1b0d6f7a1a7d6b0b0c1", 12),
        ] {
            let p = object_path(&dir, h).unwrap();
            mkdirs(p.parent().unwrap()).unwrap();
            std::fs::write(&p, vec![0u8; size]).unwrap();
        }
        assert_eq!(ensure(&dir, &idx).unwrap(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_objects_map_is_zero() {
        let dir = test_dir("empty");
        let dest = index_path(&dir, "5");
        mkdirs(dest.parent().unwrap()).unwrap();
        std::fs::write(&dest, r#"{"objects":{}}"#).unwrap();
        let mut idx = index_fixture();
        idx.size = std::fs::metadata(&dest).unwrap().len();
        assert_eq!(ensure(&dir, &idx).unwrap(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn malformed_index_is_clean_error() {
        let dir = test_dir("bad");
        let dest = index_path(&dir, "5");
        mkdirs(dest.parent().unwrap()).unwrap();
        std::fs::write(&dest, r#"{"objects": 42}"#).unwrap();
        let mut idx = index_fixture();
        idx.size = std::fs::metadata(&dest).unwrap().len();
        let e = load_index(&dir, &idx).unwrap_err();
        assert!(e.contains("objects"), "{e}");
        std::fs::write(&dest, "{not json").unwrap();
        idx.size = std::fs::metadata(&dest).unwrap().len();
        let e = load_index(&dir, &idx).unwrap_err();
        assert!(e.contains("5.json"), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
