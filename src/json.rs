// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025 LightFoxLauncher contributors
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Minimal JSON parser (RFC 8259) + value accessors.
//! OWNER: net-core. API frozen by docs/CONTRACT.md.
//!
//! Recursive descent over the UTF-8 bytes of the document: only ASCII bytes are
//! structural, everything else is copied through verbatim (a byte range always
//! starts and ends on a character boundary because the structural bytes are all
//! single-byte ASCII). No intermediate token vector is built.

use crate::util::Res;
use std::path::Path;

/// Maximum nesting depth accepted by [`Json::parse`]. Deeper documents are
/// rejected with a clean `Err` instead of blowing the stack.
pub const MAX_DEPTH: usize = 200;

#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    /// Strict RFC 8259 parse. Errors carry a byte offset into `text`.
    pub fn parse(text: &str) -> Res<Json> {
        let mut p = Parser {
            b: text.as_bytes(),
            s: text,
            i: 0,
        };
        p.ws();
        let v = p.value(0)?;
        p.ws();
        if p.i != p.b.len() {
            return Err(p.unexpected(p.i));
        }
        Ok(v)
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Json::Null)
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Json::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Json::Num(n) => Some(*n),
            _ => None,
        }
    }

    /// Integral, non-negative and exactly representable as `u64`.
    pub fn as_u64(&self) -> Option<u64> {
        let n = self.as_f64()?;
        if n >= 0.0 && n.fract() == 0.0 && n <= 9_007_199_254_740_992.0 {
            Some(n as u64)
        } else {
            None
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        let n = self.as_f64()?;
        if n.fract() == 0.0 && n.abs() <= 9_007_199_254_740_992.0 {
            Some(n as i64)
        } else {
            None
        }
    }

    pub fn as_arr(&self) -> Option<&[Json]> {
        match self {
            Json::Arr(a) => Some(a),
            _ => None,
        }
    }

    pub fn as_obj(&self) -> Option<&[(String, Json)]> {
        match self {
            Json::Obj(o) => Some(o),
            _ => None,
        }
    }

    /// Object lookup. Duplicate keys: the last occurrence wins.
    pub fn get(&self, key: &str) -> Option<&Json> {
        self.as_obj()?
            .iter()
            .rev()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v)
    }

    pub fn at(&self, index: usize) -> Option<&Json> {
        self.as_arr()?.get(index)
    }

    pub fn path(&self, keys: &[&str]) -> Option<&Json> {
        let mut cur = self;
        for k in keys {
            cur = cur.get(k)?;
        }
        Some(cur)
    }

    pub fn str_at(&self, key: &str) -> Option<&str> {
        self.get(key)?.as_str()
    }

    pub fn u64_at(&self, key: &str) -> Option<u64> {
        self.get(key)?.as_u64()
    }

    pub fn arr_at(&self, key: &str) -> Option<&[Json]> {
        self.get(key)?.as_arr()
    }
}

/// Read + parse a file. Error names the path; parse errors keep their offset.
pub fn parse_file(path: &Path) -> Res<Json> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    Json::parse(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// Escaped JSON string *content* (no surrounding quotes).
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

struct Parser<'a> {
    b: &'a [u8],
    s: &'a str,
    i: usize,
}

impl<'a> Parser<'a> {
    #[inline]
    fn ws(&mut self) {
        while let Some(&c) = self.b.get(self.i) {
            match c {
                b' ' | b'\t' | b'\n' | b'\r' => self.i += 1,
                _ => break,
            }
        }
    }

    /// `json: unexpected '}' at byte 42`
    fn unexpected(&self, at: usize) -> String {
        match self.b.get(at) {
            Some(&c) => format!("json: unexpected {} at byte {at}", show_byte(c)),
            None => format!("json: unexpected end of input at byte {at}"),
        }
    }

    fn err(&self, msg: &str, at: usize) -> String {
        format!("json: {msg} at byte {at}")
    }

    fn value(&mut self, depth: usize) -> Res<Json> {
        match self.b.get(self.i) {
            Some(&b'{') => {
                if depth >= MAX_DEPTH {
                    return Err(self.err(
                        &format!("nesting deeper than {MAX_DEPTH} levels"),
                        self.i,
                    ));
                }
                self.object(depth + 1)
            }
            Some(&b'[') => {
                if depth >= MAX_DEPTH {
                    return Err(self.err(
                        &format!("nesting deeper than {MAX_DEPTH} levels"),
                        self.i,
                    ));
                }
                self.array(depth + 1)
            }
            Some(&b'"') => Ok(Json::Str(self.string()?)),
            Some(&b't') => self.literal("true", Json::Bool(true)),
            Some(&b'f') => self.literal("false", Json::Bool(false)),
            Some(&b'n') => self.literal("null", Json::Null),
            Some(&c) if c == b'-' || c.is_ascii_digit() => self.number(),
            _ => Err(self.unexpected(self.i)),
        }
    }

    fn literal(&mut self, word: &str, v: Json) -> Res<Json> {
        if self.b[self.i..].starts_with(word.as_bytes()) {
            self.i += word.len();
            Ok(v)
        } else {
            Err(self.unexpected(self.i))
        }
    }

    fn object(&mut self, depth: usize) -> Res<Json> {
        self.i += 1; // '{'
        let mut out: Vec<(String, Json)> = Vec::new();
        self.ws();
        if self.b.get(self.i) == Some(&b'}') {
            self.i += 1;
            return Ok(Json::Obj(out));
        }
        loop {
            self.ws();
            if self.b.get(self.i) != Some(&b'"') {
                return Err(self.unexpected(self.i));
            }
            let key = self.string()?;
            self.ws();
            if self.b.get(self.i) != Some(&b':') {
                return Err(self.unexpected(self.i));
            }
            self.i += 1;
            self.ws();
            let val = self.value(depth)?;
            out.push((key, val));
            self.ws();
            match self.b.get(self.i) {
                Some(&b',') => self.i += 1,
                Some(&b'}') => {
                    self.i += 1;
                    return Ok(Json::Obj(out));
                }
                _ => return Err(self.unexpected(self.i)),
            }
        }
    }

    fn array(&mut self, depth: usize) -> Res<Json> {
        self.i += 1; // '['
        let mut out: Vec<Json> = Vec::new();
        self.ws();
        if self.b.get(self.i) == Some(&b']') {
            self.i += 1;
            return Ok(Json::Arr(out));
        }
        loop {
            self.ws();
            out.push(self.value(depth)?);
            self.ws();
            match self.b.get(self.i) {
                Some(&b',') => self.i += 1,
                Some(&b']') => {
                    self.i += 1;
                    return Ok(Json::Arr(out));
                }
                _ => return Err(self.unexpected(self.i)),
            }
        }
    }

    fn string(&mut self) -> Res<String> {
        self.i += 1; // opening quote
        let mut start = self.i;
        let mut out: Option<String> = None;
        loop {
            let c = match self.b.get(self.i) {
                Some(&c) => c,
                None => return Err(self.err("unterminated string", start)),
            };
            match c {
                b'"' => {
                    let end = self.i;
                    self.i += 1;
                    return Ok(match out {
                        Some(mut s) => {
                            s.push_str(&self.s[start..end]);
                            s
                        }
                        None => self.s[start..end].to_string(),
                    });
                }
                b'\\' => {
                    let mut s = out
                        .take()
                        .unwrap_or_else(|| String::with_capacity(self.i - start + 8));
                    s.push_str(&self.s[start..self.i]);
                    self.i += 1;
                    let e = match self.b.get(self.i) {
                        Some(&e) => e,
                        None => return Err(self.err("unterminated escape", self.i)),
                    };
                    self.i += 1;
                    match e {
                        b'"' => s.push('"'),
                        b'\\' => s.push('\\'),
                        b'/' => s.push('/'),
                        b'b' => s.push('\u{08}'),
                        b'f' => s.push('\u{0c}'),
                        b'n' => s.push('\n'),
                        b'r' => s.push('\r'),
                        b't' => s.push('\t'),
                        b'u' => {
                            let hi = self.hex4()?;
                            if (0xD800..0xDC00).contains(&hi) {
                                // Try to pair with a following low surrogate.
                                if self.b.get(self.i) == Some(&b'\\')
                                    && self.b.get(self.i + 1) == Some(&b'u')
                                {
                                    let save = self.i;
                                    self.i += 2;
                                    match self.hex4() {
                                        Ok(lo) if (0xDC00..0xE000).contains(&lo) => {
                                            let cp = 0x10000
                                                + ((hi as u32 - 0xD800) << 10)
                                                + (lo as u32 - 0xDC00);
                                            s.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
                                        }
                                        _ => {
                                            // Not a low surrogate: lone high surrogate.
                                            self.i = save;
                                            s.push('\u{FFFD}');
                                        }
                                    }
                                } else {
                                    s.push('\u{FFFD}');
                                }
                            } else if (0xDC00..0xE000).contains(&hi) {
                                s.push('\u{FFFD}');
                            } else {
                                s.push(char::from_u32(hi as u32).unwrap_or('\u{FFFD}'));
                            }
                        }
                        _ => return Err(self.err("invalid escape", self.i - 1)),
                    }
                    start = self.i;
                    out = Some(s);
                }
                c if c < 0x20 => {
                    return Err(self.err(
                        &format!("control character 0x{c:02x} in string"),
                        self.i,
                    ))
                }
                _ => self.i += 1,
            }
        }
    }

    fn hex4(&mut self) -> Res<u16> {
        let mut v: u16 = 0;
        for k in 0..4 {
            let c = match self.b.get(self.i + k) {
                Some(&c) => c,
                None => return Err(self.err("truncated \\u escape", self.i)),
            };
            let d = match c {
                b'0'..=b'9' => c - b'0',
                b'a'..=b'f' => c - b'a' + 10,
                b'A'..=b'F' => c - b'A' + 10,
                _ => return Err(self.err("bad hex digit in \\u escape", self.i + k)),
            };
            v = (v << 4) | d as u16;
        }
        self.i += 4;
        Ok(v)
    }

    fn number(&mut self) -> Res<Json> {
        let start = self.i;
        let b = self.b;
        let mut i = self.i;
        if b.get(i) == Some(&b'-') {
            i += 1;
        }
        match b.get(i) {
            Some(&b'0') => i += 1,
            Some(&c) if c.is_ascii_digit() => {
                i += 1;
                while matches!(b.get(i), Some(&d) if d.is_ascii_digit()) {
                    i += 1;
                }
            }
            _ => return Err(self.unexpected(i)),
        }
        if b.get(i) == Some(&b'.') {
            i += 1;
            if !matches!(b.get(i), Some(&d) if d.is_ascii_digit()) {
                return Err(self.err("expected digit after '.'", i));
            }
            while matches!(b.get(i), Some(&d) if d.is_ascii_digit()) {
                i += 1;
            }
        }
        if matches!(b.get(i), Some(&b'e') | Some(&b'E')) {
            i += 1;
            if matches!(b.get(i), Some(&b'+') | Some(&b'-')) {
                i += 1;
            }
            if !matches!(b.get(i), Some(&d) if d.is_ascii_digit()) {
                return Err(self.err("expected digit in exponent", i));
            }
            while matches!(b.get(i), Some(&d) if d.is_ascii_digit()) {
                i += 1;
            }
        }
        let txt = &self.s[start..i];
        self.i = i;
        match txt.parse::<f64>() {
            Ok(n) => Ok(Json::Num(n)),
            Err(_) => Err(self.err("invalid number", start)),
        }
    }
}

fn show_byte(c: u8) -> String {
    if c.is_ascii_graphic() {
        format!("'{}'", c as char)
    } else if c == b' ' {
        "' '".to_string()
    } else {
        format!("byte 0x{c:02x}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Json {
        match Json::parse(s) {
            Ok(v) => v,
            Err(e) => panic!("unexpected parse error for {s:?}: {e}"),
        }
    }

    fn perr(s: &str) -> String {
        match Json::parse(s) {
            Ok(v) => panic!("expected error for {s:?}, got {v:?}"),
            Err(e) => e,
        }
    }

    #[test]
    fn scalars() {
        assert!(p("null").is_null());
        assert_eq!(p("true").as_bool(), Some(true));
        assert_eq!(p("false").as_bool(), Some(false));
        assert_eq!(p(r#""hi""#).as_str(), Some("hi"));
        assert_eq!(p(" 1 ").as_u64(), Some(1));
        assert!(Json::parse("nul").is_err());
        assert!(Json::parse("tru").is_err());
        assert!(Json::parse("NULL").is_err());
    }

    #[test]
    fn numbers() {
        assert_eq!(p("0").as_f64(), Some(0.0));
        assert_eq!(p("-0").as_f64(), Some(-0.0));
        assert_eq!(p("-12").as_i64(), Some(-12));
        assert_eq!(p("1234567890").as_u64(), Some(1234567890));
        assert_eq!(p("1.5").as_f64(), Some(1.5));
        assert_eq!(p("-1.25e2").as_f64(), Some(-125.0));
        assert_eq!(p("2E-3").as_f64(), Some(0.002));
        assert_eq!(p("1e3").as_i64(), Some(1000));
        // u64 rejects fractional and negative values.
        assert_eq!(p("1.5").as_u64(), None);
        assert_eq!(p("-1").as_u64(), None);
        assert_eq!(p("-1").as_f64(), Some(-1.0));
        // Rejections per RFC 8259.
        for bad in ["01", "+1", ".5", "1.", "1e", "1e+", "NaN", "Infinity", "--1", "-"] {
            assert!(Json::parse(bad).is_err(), "{bad} should be rejected");
        }
        // Very large numbers still parse as f64 (infinity is the saturating result).
        assert!(p("1e400").as_f64().is_some());
        assert_eq!(p("1e400").as_u64(), None);
    }

    #[test]
    fn strings_and_escapes() {
        assert_eq!(p(r#""a\"b""#).as_str(), Some("a\"b"));
        assert_eq!(p(r#""a\\b""#).as_str(), Some("a\\b"));
        assert_eq!(p(r#""a\/b""#).as_str(), Some("a/b"));
        assert_eq!(p(r#""\b\f\n\r\t""#).as_str(), Some("\u{08}\u{0c}\n\r\t"));
        assert_eq!(p(r#""\u0041\u00e9""#).as_str(), Some("Aé"));
        // Surrogate pair -> single astral char.
        assert_eq!(p(r#""\ud83d\ude00""#).as_str(), Some("😀"));
        // Lone surrogates -> U+FFFD, and a following escape is still parsed.
        assert_eq!(p(r#""\ud83d""#).as_str(), Some("\u{FFFD}"));
        assert_eq!(p(r#""\ud83dX""#).as_str(), Some("\u{FFFD}X"));
        assert_eq!(p(r#""\ud83d\u0041""#).as_str(), Some("\u{FFFD}A"));
        assert_eq!(p(r#""\udc00""#).as_str(), Some("\u{FFFD}"));
        // Raw UTF-8 is copied verbatim.
        assert_eq!(p("\"héllo → ✓\"").as_str(), Some("héllo → ✓"));
        // Errors.
        assert!(Json::parse("\"abc").is_err());
        assert!(Json::parse("\"a\\qb\"").is_err());
        assert!(Json::parse("\"a\\u00g1\"").is_err());
        assert!(Json::parse("\"\u{0001}\"").is_err());
    }

    #[test]
    fn escape_roundtrip() {
        let raw = "a\"b\\c\n\t\r\u{08}\u{0c}\u{1f}é";
        let doc = format!("\"{}\"", escape(raw));
        assert_eq!(p(&doc).as_str(), Some(raw));
        assert_eq!(escape("plain"), "plain");
        assert_eq!(escape("\u{1f}"), "\\u001f");
        assert_eq!(escape("/"), "/");
    }

    #[test]
    fn objects_and_duplicates() {
        let v = p(r#"{"b":1,"a":2,"b":3}"#);
        // get() returns the last duplicate ...
        assert_eq!(v.get("b").and_then(Json::as_u64), Some(3));
        // ... but document order is preserved.
        let o = v.as_obj().expect("object");
        assert_eq!(o.len(), 3);
        assert_eq!(o[0].0, "b");
        assert_eq!(o[1].0, "a");
        assert_eq!(o[2].0, "b");
        // Nested accessors.
        let v = p(r#"{"d":{"client":{"url":"u","size":12}},"arr":[{"x":1},2]}"#);
        assert_eq!(v.path(&["d", "client", "url"]).and_then(Json::as_str), Some("u"));
        assert_eq!(v.path(&["d", "client", "size"]).and_then(Json::as_u64), Some(12));
        assert_eq!(v.path(&["d", "nope"]), None);
        assert_eq!(v.path(&["arr"]).and_then(|a| a.at(1)).and_then(Json::as_u64), Some(2));
        assert_eq!(v.str_at("nope"), None);
        assert_eq!(v.arr_at("arr").map(|a| a.len()), Some(2));
        assert_eq!(v.u64_at("nope"), None);
        assert_eq!(v.get("d").and_then(|d| d.str_at("client")), None);
        // Arrays of scalars.
        let v = p("[1,2,3]");
        assert_eq!(v.as_arr().map(|a| a.len()), Some(3));
        assert_eq!(v.at(9), None);
    }

    #[test]
    fn trailing_garbage_rejected() {
        for bad in ["{} x", "[] []", "1 2", "\"a\"b", "{}}", "[]]"] {
            assert!(Json::parse(bad).is_err(), "{bad} should be rejected");
        }
        for bad in ["", "   ", "{", "[", "{\"a\":}", "{\"a\" 1}", "{\"a\":1,}", "[1,]",
                    "{,}", "[1 2]", "{\"a\":1 \"b\":2}", "{a:1}", "'x'"] {
            assert!(Json::parse(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn errors_carry_offsets() {
        let e = perr(r#"{"a": }"#);
        assert!(e.contains("byte 6"), "offset missing in: {e}");
        assert!(e.starts_with("json: "), "prefix missing in: {e}");
        assert!(e.contains('\'') || e.contains("end of input"), "detail missing in: {e}");
        let e = perr("{\"a\":1} x");
        assert!(e.contains("byte 8"), "offset missing in: {e}");
        // Truncated documents.
        assert!(perr("{\"a\":[1,2").contains("end of input"));
    }

    #[test]
    fn depth_limit() {
        let ok = format!("{}{}", "[".repeat(100), "]".repeat(100));
        assert!(Json::parse(&ok).is_ok());
        let ok = format!("{}{}", "[".repeat(MAX_DEPTH), "]".repeat(MAX_DEPTH));
        assert!(Json::parse(&ok).is_ok());
        let deep = format!("{}{}", "[".repeat(MAX_DEPTH + 5), "]".repeat(MAX_DEPTH + 5));
        let e = perr(&deep);
        assert!(e.contains("nesting"), "unexpected error: {e}");
        // Objects too.
        let deep = format!("{}1{}", "{\"a\":".repeat(MAX_DEPTH + 5), "}".repeat(MAX_DEPTH + 5));
        assert!(perr(&deep).contains("nesting"));
        // Adversarial unterminated deep input must not crash either.
        assert!(Json::parse(&"[".repeat(100_000)).is_err());
    }

    #[test]
    fn realistic_documents() {
        let doc = r#"{
            "id": "1.20.1",
            "assessment": {"type": "release"},
            "downloads": {
                "client": {"sha1": "0e9d0e0a0e", "size": 22376016,
                           "url": "https://piston-data.mojang.com/v1/objects/x/client.jar"},
                "server": {}
            },
            "rules": [{"action": "allow", "os": {"name": "windows", "arch": "x64"}},
                      {"action": "disallow", "os": {"name": "osx"}}],
            "natives": {"windows": "natives-windows-${arch}"},
            "libraries": [{"name": "org.lwjgl:lwjgl:3.3.1", "downloads": {"artifact": null}}],
            "arguments": {"game": ["--username", "${auth_player_name}", "--width", 854]},
            "assetIndex": {"id": "5", "totalSize": 617233386, "size": 318864},
            "logging": {"client": {"argument": "-Dlog4j.configurationFile=${path}",
                                   "file": {"id": "client-1.12.xml", "size": 888, "url": "https://x/y"}}}
        }"#;
        let v = p(doc);
        assert_eq!(v.str_at("id"), Some("1.20.1"));
        assert_eq!(v.path(&["downloads", "client", "size"]).and_then(Json::as_u64), Some(22376016));
        assert_eq!(v.path(&["downloads", "server"]).and_then(Json::as_obj).map(|o| o.len()), Some(0));
        assert_eq!(v.path(&["rules"]).and_then(|r| r.at(1)).and_then(|r| r.str_at("action")), Some("disallow"));
        assert_eq!(v.path(&["assetIndex", "totalSize"]).and_then(Json::as_u64), Some(617233386));
        assert_eq!(v.path(&["libraries"]).and_then(|l| l.at(0)).and_then(|l| l.str_at("name")),
                   Some("org.lwjgl:lwjgl:3.3.1"));
        assert_eq!(v.path(&["arguments", "game"]).and_then(|a| a.at(3)).and_then(Json::as_u64), Some(854));
        assert!(v.path(&["logging", "client", "file", "url"]).and_then(Json::as_str).is_some());
    }

    /// The contract requires a 620 KB asset index in < 50 ms (release).
    #[test]
    fn large_document_perf() {
        let mut doc = String::with_capacity(700 * 1024);
        doc.push_str("{\"objects\":{");
        for i in 0..6000 {
            if i > 0 {
                doc.push(',');
            }
            doc.push_str(&format!(
                "\"{:040x}\":{{\"hash\":\"{:040x}\",\"size\":{}}}",
                i,
                i * 7919,
                1024 + i
            ));
        }
        doc.push_str("}}");
        assert!(doc.len() > 600 * 1024, "synthetic doc too small: {}", doc.len());
        let t0 = std::time::Instant::now();
        let v = p(&doc);
        let dt = t0.elapsed();
        assert_eq!(v.path(&["objects"]).and_then(Json::as_obj).map(|o| o.len()), Some(6000));
        if !cfg!(debug_assertions) {
            assert!(dt.as_millis() < 50, "parse of {} bytes took {dt:?}", doc.len());
        }
    }
}
