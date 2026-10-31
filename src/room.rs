// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025 LightFoxLauncher contributors
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Room codes — the "join by six characters instead of an IPv6 address" path.
//!
//! A host asks a registry to map a short code to `addr:port` for ten minutes;
//! a friend looks the code up and gets the address back. The registry never
//! sees game traffic and stores no identity — it is a meeting point, not a
//! directory.
//!
//! The alphabet excludes `I`, `O`, `0` and `1` because the code is read aloud
//! or typed from a screenshot, where those four are the characters people get
//! wrong. 32 symbols × 6 positions is ~10⁹ codes, far beyond what a ten-minute
//! TTL needs to avoid collisions.
//!
//! OWNER: lead.

use crate::json::Json;
use crate::util::Res;

/// Default registry. Overridable through `Settings.room_server`, so a user who
/// runs their own registry is not tied to this one.
pub const DEFAULT_SERVER: &str = "https://lightfoxlauncher.pages.dev";

/// The 32 symbols a code may contain: A–Z minus I and O, digits 2–9.
const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

/// Length of a room code.
pub const CODE_LEN: usize = 6;

/// A validated room code. Construction goes through [`RoomCode::parse`] or
/// [`RoomCode::generate`], so an instance is always well-formed — the registry
/// relies on that when it builds a KV key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomCode(String);

impl RoomCode {
    /// Parse user input. Case-insensitive; surrounding whitespace is ignored.
    pub fn parse(s: &str) -> Res<RoomCode> {
        let up = s.trim().to_ascii_uppercase();
        if up.is_empty() {
            return Err("房号不能为空".to_string());
        }
        if up.chars().count() != CODE_LEN {
            return Err(format!(
                "房号必须是 {CODE_LEN} 个字符，你输入了 {} 个",
                up.chars().count()
            ));
        }
        for c in up.chars() {
            if !ALPHABET.contains(&(c as u8)) {
                // Name the offending character: a user who typed 0 instead of O
                // needs to know which one is wrong, not just that something is.
                let hint = match c {
                    '0' => "（房号里没有数字 0，可能是字母 O 的位置——但 O 也不在字母表里）",
                    '1' => "（房号里没有数字 1，也没有字母 I）",
                    'I' => "（房号里没有字母 I）",
                    'O' => "（房号里没有字母 O）",
                    _ => "",
                };
                return Err(format!("房号包含非法字符 '{c}'{hint}"));
            }
        }
        Ok(RoomCode(up))
    }

    /// Generate a code from the clock and the process id.
    ///
    /// No `rand` crate: a small xorshift over those two values is plenty for
    /// this. The code is not a secret — it is shouted across a voice chat — so
    /// it only has to be unlikely to collide within its ten-minute life, which
    /// 32⁶ already guarantees.
    pub fn generate() -> RoomCode {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let mut state = nanos ^ ((std::process::id() as u64) << 32) ^ 0x9E37_79B9_7F4A_7C15;
        let mut out = String::with_capacity(CODE_LEN);
        for _ in 0..CODE_LEN {
            // xorshift64*: cheap, deterministic, no dependency.
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            let v = state.wrapping_mul(0x2545_F491_4F6C_DD1D);
            out.push(ALPHABET[(v % ALPHABET.len() as u64) as usize] as char);
        }
        RoomCode(out)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl core::fmt::Display for RoomCode {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Trim a server base URL and refuse anything that is not http(s).
///
/// The registry is contacted only when the user presses 建房号 / 用房间号连接,
/// but the URL still comes from a settings file a user can hand-edit, so it is
/// validated like every other URL in the launcher.
fn base_url(server: &str) -> Res<String> {
    let s = server.trim().trim_end_matches('/');
    if s.is_empty() {
        return Err("房间服务器地址为空".to_string());
    }
    let lower = s.to_ascii_lowercase();
    if !lower.starts_with("https://") && !lower.starts_with("http://") {
        return Err(format!("房间服务器地址必须以 https:// 或 http:// 开头：{s}"));
    }
    Ok(s.to_string())
}

/// Map an error enum from the registry to something a player can act on.
fn explain(error: &str) -> String {
    match error {
        "bad_code" => "房号格式不正确".to_string(),
        "bad_request" => "请求参数不合法（房号服务器拒绝了这次请求）".to_string(),
        "not_found" => "房号不存在或已过期（房间有效期 10 分钟）".to_string(),
        "rate_limited" => "请求太频繁，请稍等一分钟再试".to_string(),
        "server_error" => "房间服务器暂时不可用，请稍后再试".to_string(),
        other => format!("房间服务器返回错误：{other}"),
    }
}

/// Read `ok`/`error` out of a registry response.
fn parse_reply(body: &[u8], what: &str) -> Res<Json> {
    let text = String::from_utf8_lossy(body);
    let json = Json::parse(&text).map_err(|e| format!("{what}：服务器响应不是合法 JSON（{e}）"))?;
    if json.as_obj().is_none() {
        return Err(format!("{what}：服务器响应格式错误"));
    }
    Ok(json)
}

/// Register (or renew) a room: `code → addr:port` for ten minutes.
///
/// Returns the lifetime in seconds as the server reported it. `addr` is the
/// bare address — IPv6 without brackets — because the server stores what it is
/// given and the join side adds brackets when it formats a connect string.
pub fn register(server: &str, code: &RoomCode, addr: &str, port: u16) -> Res<u64> {
    let base = base_url(server)?;
    if addr.trim().is_empty() {
        return Err("房间地址为空".to_string());
    }
    if port == 0 {
        return Err("端口不能为 0".to_string());
    }
    let body = format!(
        "{{\"code\":\"{}\",\"addr\":\"{}\",\"port\":{port}}}",
        code.as_str(),
        addr.trim()
    );
    let url = format!("{base}/api/room");
    let (status, raw) = crate::net::post_json(&url, &body)?;
    let json = parse_reply(&raw, "注册房号")?;
    if json.str_at("ok") == Some("true") || json.path(&["ok"]).and_then(|v| v.as_bool()) == Some(true) {
        return Ok(json
            .path(&["expires"])
            .and_then(|v| v.as_u64())
            .unwrap_or(600));
    }
    let err = json.str_at("error").unwrap_or("server_error");
    Err(format!(
        "注册房号失败（HTTP {status}）：{}",
        explain(err)
    ))
}

/// Look a room up and return the host's `(addr, port)`.
pub fn lookup(server: &str, code: &RoomCode) -> Res<(String, u16)> {
    let base = base_url(server)?;
    let url = format!("{base}/api/room/{}", code.as_str());
    let raw = match crate::net::get_max(&url, crate::net::POST_MAX_BODY) {
        Ok(b) => b,
        Err(e) => {
            // 404 is the ordinary "expired" case and deserves its own wording;
            // everything else keeps the transport error.
            if e.contains("HTTP 404") {
                return Err(explain("not_found"));
            }
            if e.contains("HTTP 429") {
                return Err(explain("rate_limited"));
            }
            return Err(format!("查询房号失败：{e}"));
        }
    };
    let json = parse_reply(&raw, "查询房号")?;
    let ok = json.str_at("ok") == Some("true")
        || json.path(&["ok"]).and_then(|v| v.as_bool()) == Some(true);
    if !ok {
        let err = json.str_at("error").unwrap_or("server_error");
        return Err(explain(err));
    }
    let addr = json
        .str_at("addr")
        .ok_or_else(|| "查询房号：响应缺少地址".to_string())?
        .to_string();
    let port = json
        .path(&["port"])
        .and_then(|v| v.as_u64())
        .ok_or_else(|| "查询房号：响应缺少端口".to_string())?;
    if !(1..=65535).contains(&port) {
        return Err("查询房号：响应中的端口无效".to_string());
    }
    Ok((addr, port as u16))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_accepts_valid_codes_case_insensitively() {
        for good in ["ABC234", "abc234", "  abc234  ", "XYZ789"] {
            let c = RoomCode::parse(good).unwrap_or_else(|e| panic!("{good:?} rejected: {e}"));
            assert_eq!(c.as_str().len(), CODE_LEN);
            assert_eq!(c.as_str(), c.as_str().to_ascii_uppercase());
        }
        // The full alphabet must be accepted, position by position.
        let all: String = String::from_utf8_lossy(ALPHABET).to_string();
        for chunk in all.as_bytes().chunks(CODE_LEN) {
            if chunk.len() == CODE_LEN {
                let s = std::str::from_utf8(chunk).expect("alphabet is ASCII");
                assert!(RoomCode::parse(s).is_ok(), "alphabet subset {s:?} rejected");
            }
        }
    }

    #[test]
    fn parse_rejects_lookalikes_and_bad_shapes() {
        // I, O, 0, 1 are the four characters the alphabet deliberately omits.
        for bad in ["ABI234", "ABO234", "AB0234", "AB1234"] {
            let e = RoomCode::parse(bad).unwrap_err();
            assert!(e.contains("非法字符"), "{bad:?} -> {e}");
        }
        // Length problems are named as such.
        for bad in ["", "   ", "ABC23", "ABC2345"] {
            assert!(RoomCode::parse(bad).is_err(), "{bad:?} should be rejected");
        }
        // A space inside is a character, not something to strip.
        assert!(RoomCode::parse("AB 234").is_err());
        // Symbols and punctuation are out.
        for bad in ["AB-234", "AB:234", "AB/234", "AB_234"] {
            assert!(RoomCode::parse(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn parse_error_names_the_offending_character() {
        let e = RoomCode::parse("AB0234").unwrap_err();
        assert!(e.contains('0'), "error should name the character: {e}");
    }

    #[test]
    fn generated_codes_are_valid_and_varied() {
        // Every generated code must satisfy the parser — that is the contract
        // the registry depends on.
        let mut seen = std::collections::HashSet::new();
        for _ in 0..1000 {
            let c = RoomCode::generate();
            assert_eq!(c.as_str().len(), CODE_LEN);
            assert!(
                RoomCode::parse(c.as_str()).is_ok(),
                "generated an invalid code: {}",
                c.as_str()
            );
            seen.insert(c.0);
        }
        // Rough randomness check: 1000 draws from 32⁶ should almost never
        // repeat. A broken PRNG (e.g. a constant) would fail this loudly.
        assert!(
            seen.len() > 900,
            "only {} distinct codes in 1000 draws",
            seen.len()
        );
    }

    #[test]
    fn base_url_validates_and_trims() {
        assert_eq!(base_url("https://x.example").unwrap(), "https://x.example");
        assert_eq!(base_url("https://x.example/").unwrap(), "https://x.example");
        assert_eq!(
            base_url("  https://x.example//  ").unwrap(),
            "https://x.example"
        );
        assert!(base_url("").is_err());
        assert!(base_url("   ").is_err());
        assert!(base_url("ftp://x").is_err());
        assert!(base_url("x.example").is_err());
    }

    #[test]
    fn register_rejects_bad_arguments_before_any_network() {
        let c = RoomCode::parse("ABC234").unwrap();
        // Empty address, zero port, and a non-http server all fail locally, so
        // these stay no-network tests.
        assert!(register("https://x.example", &c, "", 25565).is_err());
        assert!(register("https://x.example", &c, "   ", 25565).is_err());
        assert!(register("https://x.example", &c, "1.2.3.4", 0).is_err());
        assert!(register("ftp://x", &c, "1.2.3.4", 25565).is_err());
    }

    #[test]
    fn lookup_rejects_bad_server_before_any_network() {
        let c = RoomCode::parse("ABC234").unwrap();
        assert!(lookup("", &c).is_err());
        assert!(lookup("not a url", &c).is_err());
        assert!(lookup("file:///etc/passwd", &c).is_err());
    }

    #[test]
    fn error_enum_maps_to_actionable_text() {
        // Each registry error becomes a sentence a player can act on, and none
        // of them leaks the raw enum value alone.
        for (code, needle) in [
            ("bad_code", "格式"),
            ("bad_request", "不合法"),
            ("not_found", "过期"),
            ("rate_limited", "频繁"),
            ("server_error", "不可用"),
        ] {
            let s = explain(code);
            assert!(s.contains(needle), "{code} -> {s}");
        }
        // An unknown code is still reported, not swallowed.
        assert!(explain("weird").contains("weird"));
    }

    #[test]
    fn reply_parsing_rejects_non_json() {
        assert!(parse_reply(b"not json", "test").is_err());
        assert!(parse_reply(b"[]", "test").is_err());
        assert!(parse_reply(b"{\"ok\":true}", "test").is_ok());
    }

    /// Live round trip. Requires the registry to be deployed, hence `ignore`.
    /// Run with: `cargo test --release -- --ignored live_room_round_trip`.
    #[test]
    #[ignore]
    fn live_room_round_trip() {
        let code = RoomCode::generate();
        let ttl = register(DEFAULT_SERVER, &code, "127.0.0.1", 25565).expect("register");
        assert!(ttl >= 60, "ttl looks wrong: {ttl}");
        let (addr, port) = lookup(DEFAULT_SERVER, &code).expect("lookup");
        assert_eq!(addr, "127.0.0.1");
        assert_eq!(port, 25565);
    }
}
