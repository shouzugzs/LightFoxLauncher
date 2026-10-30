// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025 LightFoxLauncher contributors
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Announcements and launcher-update checks.
//!
//! Both features read one JSON document from a URL the user configures. There is
//! no default endpoint: the launcher ships with the feature **off**, because a
//! hard-coded third-party URL would be a privacy leak and would break the moment
//! that host moved.
//!
//! ## The document
//!
//! ```json
//! {
//!   "latest": "0.3.0",
//!   "url": "https://example.invalid/lfl/releases",
//!   "notes": [
//!     { "id": "2025-11-a", "date": "2025-11-02", "title": "1.21.9 support",
//!       "body": "Adds the 1.21.9 version JSON shape.", "level": "info" }
//!   ]
//! }
//! ```
//!
//! Every field is optional and unknown fields are ignored, so a publisher can add
//! to the format without breaking old clients. `notes` may be omitted entirely.
//!
//! ## Safety
//!
//! This document is fully remote, so nothing in it is trusted:
//!
//! * `latest` is parsed as three numeric parts and compared numerically — never
//!   string-compared, so `0.10.0` is newer than `0.9.0`.
//! * `url` is only ever *shown*; the launcher does not open it. Opening a remote
//!   URL from a remote document is how an announcement turns into a phishing
//!   vector, so the user copies it if they want it.
//! * Every string is length-capped and stripped of control characters before it
//!   reaches a list box.
//! * The body is capped, the note count is capped, and the whole document is
//!   capped — a hostile endpoint can waste one request, not the machine.
//!
//! OWNER: lead.

use std::path::{Path, PathBuf};

use crate::json::Json;
use crate::util::Res;

/// Largest document accepted, in bytes. Announcements are a few KB at most.
const MAX_BODY: usize = 256 * 1024;
/// Most notes accepted from one document.
const MAX_NOTES: usize = 50;
/// Longest single field (title, body, url) kept.
const MAX_FIELD: usize = 2000;

/// How loud a note is. Unknown levels degrade to `Info`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Warn,
    /// The launcher itself is out of date.
    Update,
}

impl Level {
    pub fn parse(s: &str) -> Level {
        match s.trim().to_ascii_lowercase().as_str() {
            "warn" | "warning" => Level::Warn,
            "update" => Level::Update,
            _ => Level::Info,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Level::Info => "公告",
            Level::Warn => "注意",
            Level::Update => "更新",
        }
    }
}

/// One announcement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    /// Stable identity used to remember what has been read. Falls back to the
    /// title when the publisher omits it.
    pub id: String,
    pub date: String,
    pub title: String,
    pub body: String,
    pub level: Level,
}

impl Note {
    /// One-line form for a list box.
    pub fn list_line(&self) -> String {
        let tag = self.level.label();
        if self.date.is_empty() {
            format!("[{tag}] {}", self.title)
        } else {
            format!("[{tag}] {}  {}", self.date, self.title)
        }
    }
}

/// A parsed announcement document.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Feed {
    /// Newest published launcher version, if the document mentions one.
    pub latest: Option<String>,
    /// Where a human should go for the release. Displayed, never opened.
    pub url: Option<String>,
    pub notes: Vec<Note>,
}

/// Compare two dotted version strings numerically.
///
/// Returns `true` when `candidate` is strictly newer than `current`. Anything
/// that does not parse as at least two numeric parts is treated as "not newer",
/// so a malformed field can never trigger an update prompt.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    fn parts(v: &str) -> Option<Vec<u64>> {
        let v = v.trim().trim_start_matches('v').trim_start_matches('V');
        let mut out = Vec::new();
        for piece in v.split('.') {
            // Ignore a pre-release suffix on the last component: 0.3.0-beta -> 0.
            let digits: String = piece.chars().take_while(|c| c.is_ascii_digit()).collect();
            if digits.is_empty() {
                return None;
            }
            out.push(digits.parse().ok()?);
        }
        if out.len() < 2 {
            return None;
        }
        Some(out)
    }
    let (Some(a), Some(b)) = (parts(candidate), parts(current)) else {
        return false;
    };
    // Pad the shorter with zeros so 0.3 == 0.3.0.
    let n = a.len().max(b.len());
    for i in 0..n {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        if x != y {
            return x > y;
        }
    }
    false
}

/// `true` for characters that are invisible but still reorder or hide text.
///
/// `char::is_control` only covers category **Cc** (U+0000–U+001F, U+007F–U+009F).
/// Bidirectional overrides and zero-width characters are category **Cf** and slip
/// straight through it, which matters because the announcement list is the one
/// place a hostile feed gets to draw text:
///
/// * `U+202E` RIGHT-TO-LEFT OVERRIDE renders a title reversed, so
///   `"0.3.1 released"` can be displayed as something else entirely;
/// * `U+200B`/`U+2060` hide characters inside a title, so a note that looks
///   identical to another can carry a different `id` — and therefore never be
///   marked read, or be marked read when it was not;
/// * `U+FEFF` and the tag block are classic spoofing filler.
///
/// None of these is ever legitimate in a launcher announcement, so they are
/// dropped rather than escaped.
fn is_invisible_format(c: char) -> bool {
    matches!(c as u32,
        0x00AD                  // soft hyphen
        | 0x061C                // Arabic letter mark
        | 0x115F | 0x1160       // Hangul fillers
        | 0x17B4 | 0x17B5
        | 0x180B..=0x180E
        | 0x200B..=0x200F       // zero-width space/joiners, LRM, RLM
        | 0x202A..=0x202E       // bidi embeddings and overrides
        | 0x2060..=0x2064
        | 0x2066..=0x206F       // bidi isolates
        | 0x3164 | 0xFFA0
        | 0xFEFF                // BOM
        | 0xFFF0..=0xFFF8
        | 0xE0000..=0xE007F     // tag characters
    )
}

/// Trim, drop control characters, and cap length.
///
/// Keeps `\n` because a note **body** is legitimately multi-line and is only ever
/// shown in an edit control.
fn clean(s: &str, max: usize) -> String {
    let s: String = s
        .chars()
        .filter(|c| (!c.is_control() && !is_invisible_format(*c)) || *c == '\n')
        .collect();
    let s = s.trim().to_string();
    if s.chars().count() <= max {
        return s;
    }
    s.chars().take(max).collect()
}

/// Clean a field that must occupy exactly **one line**.
///
/// `id`, `latest`, `url`, `date` and `title` are written to `lfl-seen.ini` as
/// `key=value` lines. A `\n` in any of them injects a new line into that file:
/// a feed publishing `"latest": "0.3.0\nreported_update=9.9.9"` was observed to
/// add a second `reported_update` line, and a note `id` containing
/// `"\nseen=forged"` added a forged `seen=` entry. Both were reproduced against
/// the previous build.
///
/// Everything that can reach the state file goes through this, which also drops
/// `\r` and `\t` so a value can never span or split a line.
fn clean_line(s: &str, max: usize) -> String {
    let s: String = s
        .chars()
        .filter(|c| !c.is_control() && !is_invisible_format(*c))
        .collect();
    let s = s.trim().to_string();
    if s.chars().count() <= max {
        return s;
    }
    s.chars().take(max).collect()
}

/// Collapse anything that could break a `key=value` line.
///
/// Used by [`Seen::save`] as a last line of defence, so no value — however it
/// reached the struct — can inject a second line into the state file.
fn one_line(v: &str) -> String {
    v.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .trim()
        .to_string()
}

/// `true` for a string shaped like a version: `1.2`, `1.2.3`, `v1.2.3-beta.1`.
///
/// Deliberately strict. `latest` is compared with [`is_newer`], shown in the UI,
/// and written to `lfl-seen.ini`, so it must not carry arbitrary text. Anything
/// else — including a version with injected content appended — is rejected
/// rather than sanitised, because a partially-clean version is still wrong.
pub fn is_version_shaped(v: &str) -> bool {
    let v = v.trim();
    if v.is_empty() || v.len() > 32 {
        return false;
    }
    // Split off an optional pre-release suffix at the first '-' or '+'.
    let core = v.split(['-', '+']).next().unwrap_or(v);
    let core = core.trim_start_matches(['v', 'V']);
    let mut parts = 0usize;
    for piece in core.split('.') {
        if piece.is_empty() || !piece.bytes().all(|b| b.is_ascii_digit()) {
            return false;
        }
        parts += 1;
    }
    if parts < 2 {
        return false;
    }
    // The suffix, when present, may only contain identifier characters.
    if let Some(idx) = v.find(['-', '+']) {
        let suffix = &v[idx + 1..];
        if suffix.is_empty()
            || !suffix
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-' || b == b'+')
        {
            return false;
        }
    }
    true
}

/// Parse an announcement document.
///
/// A document with no `notes` and no `latest` is valid and yields an empty feed:
/// a publisher removing every announcement must not look like an error.
pub fn parse(text: &str) -> Res<Feed> {
    let root = Json::parse(text).map_err(|e| format!("announcement feed: {e}"))?;
    if root.as_obj().is_none() {
        return Err("announcement feed: expected a JSON object".to_string());
    }
    let mut feed = Feed::default();

    if let Some(v) = root.str_at("latest") {
        let v = clean_line(v, 32);
        // Only a real version string is accepted. Checking "starts with a digit"
        // was not enough: `"0.3.0\nreported_update=9.9.9"` had its newline
        // stripped and became `0.3.0reported_update=9.9.9`, which still passed.
        // A version must be digits, dots, and an optional pre-release suffix —
        // nothing else can reach the state file or the update prompt.
        if is_version_shaped(&v) {
            feed.latest = Some(v);
        }
    }
    if let Some(u) = root.str_at("url") {
        let u = clean_line(u, MAX_FIELD);
        // Display-only, but still refuse anything that is not http(s): a
        // `file://` or `javascript:` URL has no business in a hint.
        let lower = u.to_ascii_lowercase();
        if lower.starts_with("https://") || lower.starts_with("http://") {
            feed.url = Some(u);
        }
    }

    if let Some(arr) = root.arr_at("notes") {
        for item in arr.iter().take(MAX_NOTES) {
            if item.as_obj().is_none() {
                continue;
            }
            let title = clean_line(item.str_at("title").unwrap_or(""), 200);
            let body = clean(item.str_at("body").unwrap_or(""), MAX_FIELD);
            if title.is_empty() && body.is_empty() {
                continue;
            }
            let date = clean_line(item.str_at("date").unwrap_or(""), 32);
            let id = {
                let explicit = clean_line(item.str_at("id").unwrap_or(""), 128);
                if explicit.is_empty() {
                    // Stable across fetches as long as the publisher keeps the
                    // date and title, which is the best available default.
                    format!("{date}|{title}")
                } else {
                    explicit
                }
            };
            feed.notes.push(Note {
                id,
                date,
                title,
                body,
                level: Level::parse(item.str_at("level").unwrap_or("")),
            });
        }
    }
    Ok(feed)
}

/// Which notes the user has already seen.
///
/// Kept in its own file rather than in `lfl-settings.ini`: it is machine state,
/// not a preference, and it grows as notes arrive.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Seen {
    /// Ids of notes already read. Capped at [`MAX_SEEN`] with oldest dropped.
    pub ids: Vec<String>,
    /// Unix seconds of the last successful fetch.
    pub last_check: u64,
    /// The launcher version already reported as available, so the same update is
    /// not announced on every start.
    pub reported_update: Option<String>,
}

/// Cap on remembered ids, so the file cannot grow without bound.
const MAX_SEEN: usize = 200;

impl Seen {
    pub fn file(dir: &Path) -> PathBuf {
        dir.join("lfl-seen.ini")
    }

    pub fn load(dir: &Path) -> Seen {
        let mut out = Seen::default();
        let text = match std::fs::read_to_string(Seen::file(dir)) {
            Ok(t) => t,
            Err(_) => return out,
        };
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            match k.trim() {
                "last_check" => out.last_check = v.trim().parse().unwrap_or(0),
                "reported_update" if !v.trim().is_empty() => {
                    out.reported_update = Some(v.trim().to_string())
                }
                "seen" if !v.trim().is_empty() => {
                    if out.ids.len() < MAX_SEEN {
                        out.ids.push(v.trim().to_string());
                    }
                }
                _ => {}
            }
        }
        out
    }

    pub fn save(&self, dir: &Path) -> Res<()> {
        crate::util::mkdirs(dir)?;
        let mut body = String::from("# LFL announcement state — machine-written, safe to delete\n");
        body.push_str(&format!("last_check={}\n", self.last_check));
        // Defence in depth: `parse` already strips newlines from every field it
        // keeps, but `Seen` is public and a caller could set `reported_update`
        // directly. Writing a value with a newline here would inject a line into
        // this file, so sanitise again at the point of writing.
        if let Some(v) = &self.reported_update {
            body.push_str(&format!("reported_update={}\n", one_line(v)));
        }
        // Written oldest-first so a reload restores the same order the
        // in-memory list has; `mark` already trims from the front.
        let skip = self.ids.len().saturating_sub(MAX_SEEN);
        for id in self.ids.iter().skip(skip) {
            body.push_str(&format!("seen={}\n", one_line(id)));
        }
        let target = Seen::file(dir);
        let tmp = dir.join("lfl-seen.ini.tmp");
        std::fs::write(&tmp, body).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
        if target.exists() {
            let _ = std::fs::remove_file(&target);
        }
        std::fs::rename(&tmp, &target)
            .map_err(|e| format!("cannot replace {}: {e}", target.display()))
    }

    pub fn is_seen(&self, id: &str) -> bool {
        self.ids.iter().any(|s| s == id)
    }

    /// Remember an id, keeping the list bounded and duplicate-free.
    pub fn mark(&mut self, id: &str) {
        if self.is_seen(id) {
            return;
        }
        self.ids.push(id.to_string());
        if self.ids.len() > MAX_SEEN {
            let drop = self.ids.len() - MAX_SEEN;
            self.ids.drain(0..drop);
        }
    }

    pub fn mark_all(&mut self, feed: &Feed) {
        for n in &feed.notes {
            self.mark(&n.id);
        }
    }

    /// Notes in `feed` the user has not read yet, newest first.
    pub fn unseen<'a>(&self, feed: &'a Feed) -> Vec<&'a Note> {
        feed.notes.iter().filter(|n| !self.is_seen(&n.id)).collect()
    }
}

/// Unix seconds now, or 0 if the clock is before the epoch.
pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Fetch and parse the feed at `url`.
///
/// One request, no retry: this runs on a worker thread while the user waits, and
/// a broken announcement endpoint must never make the launcher feel slow. A
/// failure is reported as an error string, and the caller keeps the previous
/// state.
pub fn fetch(url: &str) -> Res<Feed> {
    let url = url.trim();
    if url.is_empty() {
        return Err("未配置通知地址".to_string());
    }
    let lower = url.to_ascii_lowercase();
    if !lower.starts_with("https://") && !lower.starts_with("http://") {
        return Err(format!("通知地址必须以 https:// 或 http:// 开头：{url}"));
    }
    // A plaintext feed is not refused — a LAN mirror or a self-hosted host
    // without TLS is a legitimate setup — but the user is told once, because
    // over `http://` anyone on the path can rewrite the announcements, including
    // the "new version available" hint. This is the same advisory-not-block
    // policy the unverified-download warning follows: inform, do not obstruct.
    if lower.starts_with("http://") {
        static WARNED: std::sync::OnceLock<std::sync::atomic::AtomicBool> =
            std::sync::OnceLock::new();
        let flag = WARNED.get_or_init(|| std::sync::atomic::AtomicBool::new(false));
        if !flag.swap(true, std::sync::atomic::Ordering::Relaxed) {
            crate::util::warn(
                "通知地址使用明文 http://，公告内容可被网络中间人篡改（包括“发现新版本”的提示）。\
                 如果该地址支持 HTTPS，建议改用 https://。",
            );
        }
    }
    // The cap is passed *into* the fetch so an oversized body is abandoned
    // mid-stream. Checking `body.len()` afterwards, as this did before, only
    // refused the document after the launcher had already buffered all of it —
    // a hostile endpoint could push a gigabyte into memory first.
    let body = crate::net::get_max(url, MAX_BODY).map_err(|e| format!("拉取通知失败：{e}"))?;
    let text = String::from_utf8_lossy(&body);
    parse(&text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("lfl-notify-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn version_comparison_is_numeric() {
        // The whole point: 0.10.0 is newer than 0.9.0, which string comparison
        // gets wrong.
        assert!(is_newer("0.10.0", "0.9.0"));
        assert!(is_newer("0.3.0", "0.2.1"));
        assert!(is_newer("1.0.0", "0.99.99"));
        assert!(!is_newer("0.2.1", "0.2.1"));
        assert!(!is_newer("0.2.0", "0.2.1"));
        // Missing components count as zero.
        assert!(!is_newer("0.3", "0.3.0"));
        assert!(is_newer("0.3.1", "0.3"));
        // A leading v is tolerated.
        assert!(is_newer("v0.3.0", "0.2.1"));
        // Pre-release suffixes compare on the numeric part only.
        assert!(is_newer("0.3.0-beta", "0.2.1"));
    }

    #[test]
    fn garbage_versions_never_trigger_an_update() {
        for bad in ["", "abc", "1", "x.y", "-1.0", ".."] {
            assert!(!is_newer(bad, "0.2.1"), "{bad:?} must not be newer");
            assert!(!is_newer("9.9.9", bad), "{bad:?} as current must not count");
        }
    }

    #[test]
    fn parses_a_full_feed() {
        let text = r#"{
            "latest": "0.3.0",
            "url": "https://example.invalid/rel",
            "notes": [
                {"id":"n1","date":"2025-11-02","title":"标题","body":"正文","level":"warn"},
                {"date":"2025-11-01","title":"无 id","level":"weird"}
            ]
        }"#;
        let f = parse(text).unwrap();
        assert_eq!(f.latest.as_deref(), Some("0.3.0"));
        assert_eq!(f.url.as_deref(), Some("https://example.invalid/rel"));
        assert_eq!(f.notes.len(), 2);
        assert_eq!(f.notes[0].id, "n1");
        assert_eq!(f.notes[0].level, Level::Warn);
        // Missing id falls back to date|title, and an unknown level is Info.
        assert_eq!(f.notes[1].id, "2025-11-01|无 id");
        assert_eq!(f.notes[1].level, Level::Info);
    }

    #[test]
    fn empty_feed_is_not_an_error() {
        // A publisher clearing every announcement must not look like a failure.
        for text in ["{}", r#"{"notes":[]}"#, r#"{"latest":"0.3.0"}"#] {
            assert!(parse(text).is_ok(), "{text}");
        }
        assert!(parse("{}").unwrap().notes.is_empty());
    }

    #[test]
    fn rejects_non_http_url_and_bad_version() {
        let f = parse(r#"{"latest":"nonsense","url":"javascript:alert(1)"}"#).unwrap();
        assert_eq!(f.latest, None, "a non-version must not be shown as an update");
        assert_eq!(f.url, None, "only http(s) is displayed");
        let f2 = parse(r#"{"url":"file:///etc/passwd"}"#).unwrap();
        assert_eq!(f2.url, None);
    }

    #[test]
    fn strips_control_characters_and_caps_length() {
        let long = "x".repeat(MAX_FIELD + 500);
        let text = format!(r#"{{"notes":[{{"title":"a\u0000b\u0007c","body":"{long}"}}]}}"#);
        let f = parse(&text).unwrap();
        assert_eq!(f.notes[0].title, "abc");
        assert_eq!(f.notes[0].body.chars().count(), MAX_FIELD);
    }

    #[test]
    fn caps_note_count() {
        let notes: Vec<String> = (0..MAX_NOTES + 25)
            .map(|i| format!(r#"{{"id":"n{i}","title":"t{i}"}}"#))
            .collect();
        let text = format!(r#"{{"notes":[{}]}}"#, notes.join(","));
        assert_eq!(parse(&text).unwrap().notes.len(), MAX_NOTES);
    }

    #[test]
    fn malformed_documents_are_errors_not_panics() {
        for bad in ["", "{", "[]", "null", r#"{"notes":"not an array"}"#, r#"{"notes":[1,2]}"#] {
            let _ = parse(bad);
        }
        // A top-level array is rejected explicitly.
        assert!(parse("[]").is_err());
        // Non-object note entries are skipped rather than fatal.
        assert_eq!(parse(r#"{"notes":[1,{"title":"ok"}]}"#).unwrap().notes.len(), 1);
    }

    #[test]
    fn feed_cannot_inject_lines_into_the_state_file() {
        // Reproduced against the previous build: `clean` kept a literal newline,
        // so a `latest` or note `id` containing one added a forged line to
        // lfl-seen.ini, and `reported_update` was overwritten on reload.
        let dir = temp("inject");
        let doc = "{\"latest\":\"0.3.0\\nreported_update=9.9.9\",\
                   \"notes\":[{\"id\":\"a\\nseen=forged\",\"title\":\"t\\nseen=x\",\
                   \"date\":\"d\\nseen=y\"}]}";
        let feed = parse(doc).expect("parse");
        // `latest` is rejected outright: it is not version-shaped, so it never
        // becomes an update prompt and never reaches the state file.
        assert_eq!(feed.latest, None, "non-version latest must be dropped");
        assert!(!feed.notes[0].id.contains('\n'));
        assert!(!feed.notes[0].title.contains('\n'));
        assert!(!feed.notes[0].date.contains('\n'));

        let mut seen = Seen::default();
        seen.reported_update = feed.latest.clone();
        seen.mark_all(&feed);
        seen.save(&dir).unwrap();
        let body = std::fs::read_to_string(Seen::file(&dir)).unwrap();
        // The injected text is collapsed onto one line, so no forged key can
        // appear at the start of a line.
        let count_key = |k: &str| {
            body.lines().filter(|l| l.starts_with(&format!("{k}="))).count()
        };
        assert_eq!(count_key("reported_update"), 0, "{body}");
        assert_eq!(count_key("seen"), 1, "{body}");
        assert_eq!(count_key("last_check"), 1, "{body}");
        let back = Seen::load(&dir);
        assert_eq!(back.reported_update, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_sanitises_even_a_directly_constructed_value() {
        // `Seen` is public, so a caller can set these fields without going
        // through `parse`. Save must still refuse to write a second line.
        let dir = temp("inject2");
        let mut seen = Seen::default();
        seen.reported_update = Some("1.0.0\nlast_check=999".to_string());
        seen.ids.push("id\nseen=forged".to_string());
        seen.save(&dir).unwrap();
        let body = std::fs::read_to_string(Seen::file(&dir)).unwrap();
        // Count *lines that start with* the key: the injected text is collapsed
        // onto the same line, so a plain substring count would still see it.
        let count_key = |k: &str| {
            body.lines()
                .filter(|l| l.starts_with(&format!("{k}=")))
                .count()
        };
        assert_eq!(count_key("reported_update"), 1, "{body}");
        assert_eq!(count_key("last_check"), 1, "{body}");
        assert_eq!(count_key("seen"), 1, "{body}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn body_may_still_be_multiline() {
        // The body is only ever shown in an edit control, so newlines are fine
        // there and must not be stripped.
        let f = parse("{\"notes\":[{\"id\":\"a\",\"title\":\"t\",\"body\":\"line1\\nline2\"}]}")
            .unwrap();
        assert!(f.notes[0].body.contains('\n'));
    }

    #[test]
    fn version_shape_is_strict() {
        for good in ["0.2", "0.2.1", "v1.2.3", "1.2.3-beta", "1.2.3+build", "1.2.3-rc.1"] {
            assert!(is_version_shaped(good), "should accept {good:?}");
        }
        for bad in [
            "", "  ", "abc", "1", "1.", ".1", "-1.0",
            "0.3.0reported_update=9.9.9",   // injection with the newline stripped
            "0.3.0; rm -rf /",              // arbitrary text
            "0.3.0 beta",                   // space
            "0.3.0-beta;x",                 // bad suffix char
            "1.2.3.4.5.6.7.8.9.0.1.2.3.4.5.6.7.8.9.0.1.2.3.4.5", // too long
        ] {
            assert!(!is_version_shaped(bad), "should reject {bad:?}");
        }
    }

    #[test]
    fn raw_control_bytes_are_rejected_by_the_json_parser() {
        // Defence in depth: a literal newline inside a JSON string is invalid
        // JSON, so json.rs rejects it before clean_line ever runs. The only
        // route to an injected line is the escape, which clean_line strips.
        let err = parse("{\"latest\":\"1.0\n2.0\"}").unwrap_err();
        println!("raw newline -> {err}");
        assert!(err.contains("control character"), "{err}");
    }

    #[test]
    fn seen_state_round_trips() {
        let dir = temp("seen");
        let mut s = Seen::default();
        s.mark("a");
        s.mark("b");
        s.last_check = 1_700_000_000;
        s.reported_update = Some("0.3.0".to_string());
        s.save(&dir).unwrap();
        let back = Seen::load(&dir);
        assert_eq!(back, s);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn marking_is_idempotent_and_bounded() {
        let mut s = Seen::default();
        s.mark("a");
        s.mark("a");
        assert_eq!(s.ids.len(), 1);
        for i in 0..MAX_SEEN + 50 {
            s.mark(&format!("n{i}"));
        }
        assert_eq!(s.ids.len(), MAX_SEEN);
        // The oldest were dropped, the newest kept.
        assert!(s.is_seen(&format!("n{}", MAX_SEEN + 49)));
        assert!(!s.is_seen("a"));
    }

    #[test]
    fn unseen_filters_already_read_notes() {
        let feed = Feed {
            latest: None,
            url: None,
            notes: vec![
                Note {
                    id: "old".into(),
                    date: String::new(),
                    title: "旧".into(),
                    body: String::new(),
                    level: Level::Info,
                },
                Note {
                    id: "new".into(),
                    date: String::new(),
                    title: "新".into(),
                    body: String::new(),
                    level: Level::Update,
                },
            ],
        };
        let mut s = Seen::default();
        s.mark("old");
        let unseen = s.unseen(&feed);
        assert_eq!(unseen.len(), 1);
        assert_eq!(unseen[0].id, "new");
        s.mark_all(&feed);
        assert!(s.unseen(&feed).is_empty());
    }

    #[test]
    fn list_line_is_readable() {
        let n = Note {
            id: "i".into(),
            date: "2025-11-02".into(),
            title: "标题".into(),
            body: String::new(),
            level: Level::Warn,
        };
        assert_eq!(n.list_line(), "[注意] 2025-11-02  标题");
        let n2 = Note {
            date: String::new(),
            level: Level::Update,
            ..n
        };
        assert_eq!(n2.list_line(), "[更新] 标题");
    }

    #[test]
    fn fetch_rejects_bad_urls_without_network() {
        assert!(fetch("").is_err());
        assert!(fetch("   ").is_err());
        assert!(fetch("ftp://x/y").is_err());
        assert!(fetch("not a url").is_err());
        // A URL that would split the request line is refused before any socket
        // is opened, so this stays a no-network test.
        assert!(fetch("https://example.com/a\r\nX-Injected: 1").is_err());
    }

    #[test]
    fn invisible_format_characters_cannot_spoof_a_title() {
        // `char::is_control` is category Cc only, so a bidi override or a
        // zero-width joiner used to survive into the list box and the state
        // file. `\u{202E}` reverses rendering; `\u{200B}` hides characters, so
        // two notes can look identical while carrying different ids.
        let doc = "{\"notes\":[{\"id\":\"a\\u202Eb\",\"title\":\"ok\\u202Etxet\",\
                   \"body\":\"x\\u200By\",\"date\":\"d\\u2066e\"}]}";
        let f = parse(doc).expect("parse");
        assert_eq!(f.notes[0].id, "ab", "bidi override must be dropped");
        assert_eq!(f.notes[0].title, "oktxet");
        assert_eq!(f.notes[0].body, "xy", "zero-width must be dropped");
        assert_eq!(f.notes[0].date, "de", "bidi isolate must be dropped");
        // A soft hyphen is invisible too, and is a classic homograph trick.
        let f2 = parse("{\"notes\":[{\"id\":\"a\u{00AD}b\",\"title\":\"t\"}]}").expect("parse");
        assert_eq!(f2.notes[0].id, "ab");
        // Legitimate CJK and emoji must survive: this is not a "strip
        // everything non-ASCII" filter.
        let f3 = parse("{\"notes\":[{\"id\":\"公告🦊\",\"title\":\"1.21.9 支持\"}]}").expect("parse");
        assert_eq!(f3.notes[0].id, "公告🦊");
        assert_eq!(f3.notes[0].title, "1.21.9 支持");
    }

    #[test]
    fn a_huge_body_is_refused_with_a_clear_message() {
        // The cap is now passed into the fetch, so the error text comes from
        // net.rs. Assert the constant the caller matches on is present, so a
        // wording change cannot silently break the message.
        assert!(crate::net::TOO_LARGE.contains("size cap"));
        // MAX_BODY must stay far below the generic document cap, or the tighter
        // announcement limit would never be the one that fires.
        assert!(MAX_BODY < crate::net::MAX_DOC);
    }
}
