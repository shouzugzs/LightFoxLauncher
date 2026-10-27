// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025 LightFoxLauncher contributors
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Persisted launcher settings (the 设置 page).
//!
//! Stored as flat `key=value` lines in `<data dir>/lfl-settings.ini`. A tiny
//! hand-rolled format keeps this dependency-free and human-editable; the file is
//! written atomically so a crash mid-save cannot corrupt it.
//!
//! OWNER: lead.

use std::path::{Path, PathBuf};

use crate::util::Res;

/// User-tunable options that outlive a single run.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    /// Concurrent file downloads (0 = automatic).
    pub workers: usize,
    /// Explicit Java path, when the user wants a specific runtime.
    pub java: Option<String>,
    /// Default heap size for new launches.
    pub memory: String,
    /// Default player name.
    pub user: String,
    /// Default instance name.
    pub instance: String,
    /// Keep partial downloads for resume (off means `clean` them each run).
    pub resume: bool,
    /// Show the aggregated download progress line.
    pub progress: bool,
    /// Announcement / update-check endpoint. `None` disables the feature
    /// entirely; there is deliberately no default, because a hard-coded URL
    /// would leak a request to a third party on every start.
    pub feed_url: Option<String>,
    /// Check the feed automatically on start.
    pub check_feed: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            workers: crate::net::default_workers(),
            java: None,
            memory: "2G".to_string(),
            user: "Steve".to_string(),
            instance: "default".to_string(),
            resume: true,
            progress: true,
            feed_url: None,
            // Off until a URL is configured: no URL, nothing to check.
            check_feed: true,
        }
    }
}

/// `key=value` lines; unknown keys are preserved-in-spirit (ignored on load,
/// dropped on save) and malformed lines are skipped rather than fatal.
impl Settings {
    pub fn file(dir: &Path) -> PathBuf {
        dir.join("lfl-settings.ini")
    }

    pub fn load(dir: &Path) -> Settings {
        let mut out = Settings::default();
        let text = match std::fs::read_to_string(Settings::file(dir)) {
            Ok(t) => t,
            Err(_) => return out,
        };
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
                "workers" => out.workers = v.parse().unwrap_or(out.workers),
                "java" if !v.is_empty() => out.java = Some(v.to_string()),
                "memory" if !v.is_empty() => out.memory = v.to_string(),
                "user" if !v.is_empty() => out.user = v.to_string(),
                "instance" if !v.is_empty() => out.instance = v.to_string(),
                "resume" => out.resume = v != "0",
                "progress" => out.progress = v != "0",
                "feed_url" if !v.is_empty() => out.feed_url = Some(v.to_string()),
                "check_feed" => out.check_feed = v != "0",
                _ => {}
            }
        }
        out
    }

    pub fn save(&self, dir: &Path) -> Res<()> {
        crate::util::mkdirs(dir)?;
        let body = format!(
            "# LFL settings — edit by hand or via the 设置 page\n\
             workers={}\n\
             java={}\n\
             memory={}\n\
             user={}\n\
             instance={}\n\
             resume={}\n\
             progress={}\n\
             feed_url={}\n\
             check_feed={}\n",
            self.workers,
            self.java.clone().unwrap_or_default(),
            self.memory,
            self.user,
            self.instance,
            self.resume as u8,
            self.progress as u8,
            self.feed_url.clone().unwrap_or_default(),
            self.check_feed as u8,
        );
        let target = Settings::file(dir);
        let tmp = dir.join("lfl-settings.ini.tmp");
        std::fs::write(&tmp, body)
            .map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
        // Atomic replace: a half-written settings file would be worse than none.
        if target.exists() {
            let _ = std::fs::remove_file(&target);
        }
        std::fs::rename(&tmp, &target)
            .map_err(|e| format!("cannot replace {}: {e}", target.display()))
    }

    /// `load` + `apply` in one call.
    pub fn load_and_apply(dir: &Path) -> Settings {
        let s = Settings::load(dir);
        s.apply();
        s
    }

    /// Cross-check the values a settings form can produce.
    pub fn validate(&self) -> Result<(), String> {
        crate::launcher::validate_memory(&self.memory)?;
        crate::launcher::validate_instance_name(&self.instance)?;
        if self.user.is_empty() || self.user.len() > 16 {
            return Err("player name must be 1..=16 characters".to_string());
        }
        if self.workers > crate::net::MAX_WORKERS_LIMIT {
            return Err(format!(
                "worker count must be at most {}",
                crate::net::MAX_WORKERS_LIMIT
            ));
        }
        if let Some(u) = &self.feed_url {
            let lower = u.trim().to_ascii_lowercase();
            if !lower.is_empty()
                && !lower.starts_with("https://")
                && !lower.starts_with("http://")
            {
                return Err("通知地址必须以 https:// 或 http:// 开头".to_string());
            }
        }
        Ok(())
    }

    /// Apply the settings that the download/launch layer can take globally.
    pub fn apply(&self) {
        crate::net::set_workers(self.workers);
        crate::net::set_progress(self.progress);
    }
}

/// `load` → `apply`, the normal startup path.
pub fn load_and_apply(dir: &Path) -> Settings {
    let s = Settings::load(dir);
    s.apply();
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "lfl-settings-{}-{}",
            tag,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn round_trips() {
        let dir = temp("round");
        let s = Settings {
            workers: 6,
            java: Some("C:\\jdk\\bin\\javaw.exe".to_string()),
            memory: "4G".to_string(),
            user: "Notch".to_string(),
            instance: "pvp".to_string(),
            resume: false,
            progress: true,
            feed_url: Some("https://example.invalid/feed.json".to_string()),
            check_feed: false,
        };
        s.save(&dir).unwrap();
        assert_eq!(Settings::load(&dir), s);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_yields_defaults() {
        let dir = temp("missing");
        let s = Settings::load(&dir);
        assert_eq!(s, Settings::default());
        assert!(s.resume);
        assert!(s.progress);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tolerates_garbage() {
        let dir = temp("garbage");
        std::fs::write(
            Settings::file(&dir),
            "# comment\nnot-a-pair\nworkers=abc\nuser=\nmemory=8G\n",
        )
        .unwrap();
        let s = Settings::load(&dir);
        // Bad number falls back, empty value ignored.
        assert_eq!(s.memory, "8G");
        assert_eq!(s.workers, Settings::default().workers);
        assert_eq!(s.user, Settings::default().user);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_java_means_automatic() {
        let dir = temp("nojava");
        std::fs::write(Settings::file(&dir), "java=\n").unwrap();
        assert_eq!(Settings::load(&dir).java, None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
