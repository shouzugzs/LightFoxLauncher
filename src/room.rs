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

/// Longest address accepted from the registry.
///
/// Well above the longest legitimate IPv6 literal (45 characters) but short
/// enough that a hostile record cannot push a paragraph into a status line.
const ADDR_MAX_CHARS: usize = 64;

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
            // Compare as `char`, never `c as u8`: that cast truncates, so
            // `'Ł'` (U+0141) would become `b'A'` and a code that is not on the
            // alphabet would be accepted — then stored and looked up as a
            // different, valid-looking code.
            if !c.is_ascii() || !ALPHABET.contains(&(c as u8)) {
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

/// Build the POST body for [`register`].
///
/// Hand-built rather than via a serialiser (the crate has no `serde`), so
/// `addr` **must** go through [`crate::json::escape`]: it arrives from
/// `hostnet` and can be an IPv6 literal with a zone id, and without escaping a
/// `"` or `\` in it either breaks the document or, for
/// `x","port":1,"z":"`, injects extra fields the registry would act on.
fn register_body(code: &RoomCode, addr: &str, port: u16) -> String {
    format!(
        "{{\"code\":\"{}\",\"addr\":\"{}\",\"port\":{port}}}",
        code.as_str(),
        crate::json::escape(addr)
    )
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
    let body = register_body(code, addr.trim(), port);
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


/// A host address that passed [`checked_addr`].
///
/// This is a newtype rather than a bare `String` so the validation cannot be
/// bypassed by accident: [`lookup`] returns `RoomAddr`, and the only way to
/// build one is [`checked_addr`] (the field is private to this module). A
/// future edit that tries to hand back an unvalidated string — the one
/// mutation the tests alone would not catch — fails to compile instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomAddr(String);

impl RoomAddr {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl core::fmt::Display for RoomAddr {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Validate a host address from the registry.
///
/// The registry's own check (`site/functions/api/room.js`, `ADDR_RE`) is an
/// allowlist of `[0-9A-Fa-f:.[]]`, but a client must not depend on a
/// deployment behaving: this repeats the same allowlist, so nothing
/// legitimate is lost and a compromised registry gains nothing.
fn checked_addr(addr: &str) -> Res<RoomAddr> {
    if addr.is_empty() {
        return Err("查询房号：响应中的地址为空".to_string());
    }
    if addr.len() > ADDR_MAX_CHARS {
        return Err(format!(
            "查询房号：响应中的地址过长（{} 个字符）",
            addr.len()
        ));
    }
    if let Some(c) = addr
        .chars()
        .find(|c| !matches!(c, '0'..='9' | 'A'..='F' | 'a'..='f' | ':' | '.' | '[' | ']'))
    {
        return Err(format!("查询房号：响应中的地址包含非法字符 {c:?}"));
    }
    Ok(RoomAddr(addr.to_string()))
}

/// Turn a registry lookup response into `(addr, port)`.
///
/// Split out of [`lookup`] so the whole response path — `ok` handling, the
/// error enum, the port range and the address allowlist — is testable without
/// a network, the same way `notify::parse` is.
fn parse_lookup(body: &[u8]) -> Res<(RoomAddr, u16)> {
    let json = parse_reply(body, "查询房号")?;
    let ok = json.str_at("ok") == Some("true")
        || json.path(&["ok"]).and_then(|v| v.as_bool()) == Some(true);
    if !ok {
        let err = json.str_at("error").unwrap_or("server_error");
        return Err(explain(err));
    }
    let addr = json
        .str_at("addr")
        .ok_or_else(|| "查询房号：响应缺少地址".to_string())?;
    let port = json
        .path(&["port"])
        .and_then(|v| v.as_u64())
        .ok_or_else(|| "查询房号：响应缺少端口".to_string())?;
    if !(1..=65535).contains(&port) {
        return Err("查询房号：响应中的端口无效".to_string());
    }
    Ok((checked_addr(addr)?, port as u16))
}

/// The URL a lookup is sent to. Split out so the *shape* of the route — the
/// `code` query parameter, not a path segment — is pinned by a test rather
/// than only by a live round trip.
fn lookup_url(base: &str, code: &RoomCode) -> String {
    format!("{base}/api/room?code={}", code.as_str())
}

/// Map `(status, body)` from the registry to the lookup result.
///
/// Kept separate from [`lookup`] so every branch is reachable in a test: the
/// network only supplies the two values.
fn parse_lookup_status(status: u32, body: &[u8]) -> Res<(RoomAddr, u16)> {
    match status {
        // The body decides among `ok`, `bad_code`, `bad_request` and
        // `server_error`; a 200 is the only status whose body may carry an
        // address, and `parse_lookup` validates it before it becomes a
        // `RoomAddr`.
        200 => parse_lookup(body),
        // The registry answers 404 with `{"ok":false,"error":"not_found"}` for
        // an expired or unknown room. The status is the contract; the body is
        // not parsed, so a broken 404 page still reads as "not found".
        404 => Err(explain("not_found")),
        429 => Err(explain("rate_limited")),
        // Everything else is the registry (or something in front of it)
        // failing. The status is named so the report is diagnosable, but the
        // body is not quoted: it can be any length and any language.
        other => Err(format!("查询房号失败（HTTP {other}）")),
    }
}

/// Look a room up and return the host's `(addr, port)`.
///
/// The code travels as the `code` **query parameter**, not as a path segment:
/// the registry is a Cloudflare Pages Function, and Pages is a file router, so
/// `site/functions/api/room.js` maps to `/api/room` only. `/api/room/ABC234`
/// was answered by the static-asset 404 before any Function ran, which is the
/// bug this shape fixes.
///
/// Failure handling goes through the **status**, never through the error
/// string. `get_max` reports a non-2xx as `HTTP 404 for <url>` with the body
/// discarded, and matching that text is how a CDN's HTML 404 page — or any
/// other host that happens to answer 404 — gets mistranslated into "房号已过期".
/// [`crate::net::get_with_status`] keeps `(status, body)`, so each status maps
/// to one outcome and nothing is guessed from a message.
pub fn lookup(server: &str, code: &RoomCode) -> Res<(RoomAddr, u16)> {
    let base = base_url(server)?;
    let url = lookup_url(&base, code);
    let (status, raw) = crate::net::get_with_status(&url, crate::net::POST_MAX_BODY)
        .map_err(|e| format!("查询房号失败：{e}"))?;
    parse_lookup_status(status, &raw)
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
    fn parse_rejects_non_ascii_that_truncates_onto_the_alphabet() {
        // `c as u8` truncates: 'Ł' is U+0141, whose low byte is 0x41 = 'A'.
        // With a byte cast in the membership test this input was accepted and
        // silently became a *different* valid code ("ABC234") — the user would
        // then register a room nobody else could reach. Every non-ASCII
        // character must be rejected, not folded onto the alphabet.
        for bad in ["ŁBC234", "ABC23Ł", "ＡBC234", "ABC234\u{00AD}"] {
            assert!(
                RoomCode::parse(bad).is_err(),
                "{bad:?} must be rejected, got {:?}",
                RoomCode::parse(bad)
            );
        }
        // The legitimate ASCII neighbours still work, so the check is not
        // simply rejecting everything.
        assert_eq!(RoomCode::parse("abc234").unwrap().as_str(), "ABC234");
    }

    #[test]
    fn register_body_is_valid_json_for_any_address() {
        // The body is built by hand, so an address containing a quote or a
        // backslash used to produce invalid JSON — or, for
        // `x","port":1,"z":"`, a *valid* document with extra fields. Escaping
        // must neutralise both, and the address must round-trip unchanged.
        let code = RoomCode::parse("ABC234").unwrap();
        for addr in ["1.2.3.4", "::1", "fe80::1%eth0", "a\"b", "c\\d", "x\",\"port\":1,\"z\":\""] {
            let body = register_body(&code, addr, 25565);
            let parsed = Json::parse(&body)
                .unwrap_or_else(|e| panic!("body for {addr:?} is not JSON: {e} ({body})"));
            assert_eq!(parsed.str_at("addr"), Some(addr), "addr round trip: {body}");
            assert_eq!(parsed.str_at("code"), Some("ABC234"));
            assert_eq!(parsed.u64_at("port"), Some(25565), "port must not be overwritten");
            // No injected key may survive.
            assert!(parsed.get("z").is_none(), "injected field in {body}");
        }
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
    fn lookup_url_puts_the_code_in_the_query_string() {
        // The registry is a Pages Function, and Pages is a file router: only
        // `/api/room` reaches `room.js`. A path segment never routes at all,
        // so the code must be a query parameter — this test is the pin that
        // stops a future edit from "tidying" it back into the path.
        let c = RoomCode::parse("ABC234").unwrap();
        let url = lookup_url("https://launcher.example", &c);
        assert_eq!(url, "https://launcher.example/api/room?code=ABC234");
        assert!(!url.contains("/api/room/"), "path-segment form is not routed: {url}");
        // The code is always a validated uppercase 6-char string, so it needs
        // no escaping and cannot inject a second parameter.
        for good in ["ABC234", "XYZ789", "ZZZZZZ"] {
            let c = RoomCode::parse(good).unwrap();
            let url = lookup_url("https://h", &c);
            assert_eq!(url.matches('?').count(), 1, "one query: {url}");
            assert_eq!(url.matches('&').count(), 0, "one parameter: {url}");
            assert!(url.ends_with(&format!("?code={good}")), "{url}");
        }
    }

    #[test]
    fn lookup_status_maps_without_string_matching() {
        let ok_body = br#"{"ok":true,"addr":"1.2.3.4","port":25565}"#;
        // 200 is the only status that yields an address.
        let got = parse_lookup_status(200, ok_body).expect("200 ok");
        assert_eq!(got.0.as_str(), "1.2.3.4");
        assert_eq!(got.1, 25565);

        // 404 means "expired or unknown" — and it means that because of the
        // number, not because the body looked like JSON. An HTML error page
        // from a CDN in front of the registry must not become a panic or a
        // bogus address.
        let e = parse_lookup_status(404, b"<html>Not Found</html>").unwrap_err();
        assert!(e.contains("过期"), "{e}");
        // 429 is the rate limit, again by status alone.
        let e = parse_lookup_status(429, b"").unwrap_err();
        assert!(e.contains("频繁"), "{e}");
        // Anything else is a plain failure that names the status.
        for status in [500u32, 502, 503, 301, 403] {
            let e = parse_lookup_status(status, b"").unwrap_err();
            assert!(e.contains(&format!("HTTP {status}")), "{status} -> {e}");
            // The raw body is never quoted back at the user.
            assert!(!e.contains("<html>"), "{status} -> {e}");
        }
        // A 200 whose body is the error enum still reports the enum, so a
        // registry that answers 200 for a bad code is handled too.
        let e = parse_lookup_status(200, br#"{"ok":false,"error":"bad_code"}"#).unwrap_err();
        assert!(e.contains("格式"), "{e}");
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
    fn room_addr_can_only_be_built_through_the_check() {
        // The regression this guards is not a behaviour but a **type**: the
        // one way to get an unvalidated address into the GUI was for a future
        // edit to hand back a bare String from lookup(). RoomAddr has a
        // private field, so that edit does not compile. What is testable here
        // is the invariant the type carries: every RoomAddr that exists has
        // been through the allowlist.
        let ok = checked_addr("2001:db8::1").expect("valid address accepted");
        assert_eq!(ok.as_str(), "2001:db8::1");
        assert_eq!(format!("{ok}"), "2001:db8::1");
        // And the constructor refuses what the GUI must never render.
        for bad in ["1.2.3.4\n[已上线]", "1.2.3.4 x", "a\"b", ""] {
            assert!(
                checked_addr(bad).is_err(),
                "{bad:?} must not become a RoomAddr"
            );
        }
    }

    #[test]
    fn lookup_response_is_parsed_through_the_address_check() {
        // The real response path, so removing the `checked_addr` call from
        // `lookup` fails here rather than silently shipping.
        let good = parse_lookup(br#"{"ok":true,"addr":"2001:db8::1","port":25565}"#).unwrap();
        assert_eq!(good.0.as_str(), "2001:db8::1");
        assert_eq!(good.1, 25565);

        // A hostile record reaches the same path a real one does. The newline
        // must arrive as a JSON **escape**: `json.rs` rejects a raw control
        // character, and `JSON.stringify` on the server side emits `\n` too,
        // so this is the shape the attack actually takes.
        let hostile = "{\"ok\":true,\"addr\":\"1.2.3.4\\n[已上线] x\",\"port\":25565}";
        let err = parse_lookup(hostile.as_bytes()).unwrap_err();
        assert!(err.contains("非法字符"), "{err}");

        // The registry's closed error enum maps to actionable text.
        for (body, needle) in [
            (r#"{"ok":false,"error":"not_found"}"#, "过期"),
            (r#"{"ok":false,"error":"rate_limited"}"#, "频繁"),
            (r#"{"ok":false,"error":"server_error"}"#, "不可用"),
            (r#"{"ok":false}"#, "不可用"),
        ] {
            let err = parse_lookup(body.as_bytes()).unwrap_err();
            assert!(err.contains(needle), "{body} -> {err}");
        }

        // A missing or out-of-range port is refused, not truncated to u16.
        for body in [
            r#"{"ok":true,"addr":"1.2.3.4"}"#,
            r#"{"ok":true,"addr":"1.2.3.4","port":0}"#,
            r#"{"ok":true,"addr":"1.2.3.4","port":70000}"#,
            r#"{"ok":true,"addr":"1.2.3.4","port":"25565"}"#,
        ] {
            assert!(parse_lookup(body.as_bytes()).is_err(), "{body}");
        }
    }

    #[test]
    fn reply_parsing_rejects_non_json() {
        assert!(parse_reply(b"not json", "test").is_err());
        assert!(parse_reply(b"[]", "test").is_err());
        assert!(parse_reply(b"{\"ok\":true}", "test").is_ok());
    }

    #[test]
    fn a_hostile_address_cannot_forge_a_status_line() {
        // The GUI formats the join result as `房号 X → {shown}` and splits the
        // report into lines, so a newline in the address from the registry adds
        // a status line the launcher never wrote. Reproduced shape:
        // `"1.2.3.4\n[已上线] …"` renders as two lines.
        for bad in [
            "1.2.3.4\n[已上线] 你被入侵了",
            "1.2.3.4\r\nx",
            "1.2.3.4 5.6.7.8",
            "1.2.3.4\"",
            "",
            "::1%eth0",              // IPv6 zone id: percent is not allowed
            &"1".repeat(ADDR_MAX_CHARS + 1),
        ] {
            assert!(checked_addr(bad).is_err(), "{bad:?} must be refused");
        }
        // Everything the registry can legitimately return must survive
        // unchanged, including the IPv6 forms the join path re-brackets.
        for good in [
            "1.2.3.4",
            "::1",
            "fe80::1",
            "2001:db8::8a2e:370:7334",
            "[::1]",
            "192.168.1.10",
        ] {
            assert_eq!(checked_addr(good).unwrap().as_str(), good, "{good:?} must be kept");
        }
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
        assert_eq!(addr.as_str(), "127.0.0.1");
        assert_eq!(port, 25565);
    }
}
