// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025 LightFoxLauncher contributors
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! UPnP IGD port mapping — the "UPnP 尝试" feature.
//!
//! Tries to map an external port on the home router to this machine so
//! friends on IPv4 can join. Deliberately named an *attempt*: a router can
//! refuse (UPnP disabled, CGNAT upstream, firmware quirks), and the result
//! type says exactly which happened so the UI can tell the user what to do
//! instead of leaving a generic "failed".
//!
//! Flow: SSDP M-SEARCH discovery → fetch the device description XML →
//! find the WANIPConnection/WANPPPConnection controlURL → SOAP AddPortMapping.
//! All raw sockets and hand-built XML; the zero-dependency rule applies here
//! like everywhere else.
//!
//! OWNER: lead.

use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::time::Duration;

use crate::hostnet;
use crate::util::Res;

/// How long to listen for SSDP responses per M-SEARCH.
const SSDP_WAIT: Duration = Duration::from_secs(3);

/// Default external port the UI offers (Minecraft Java).
pub const DEFAULT_PORT: u16 = 25565;

/// Mapping lifetime to request, in seconds. 4 hours: long enough for an
/// evening, short enough that a crashed launcher does not leave a permanent
/// hole in the router.
const LEASE_SECONDS: u32 = 4 * 3600;

/// Why an attempt did not produce a working mapping. The UI shows the
/// detail string verbatim; the enum is for logic ("is it worth retrying").
#[derive(Debug, Clone, PartialEq)]
pub enum UpnpErr {
    /// No router answered SSDP at all.
    NoGateway,
    /// A router answered but exposes no IGD control URL we can use.
    NoIgd,
    /// The router refused the mapping (code + its meaning).
    Refused { code: u32, detail: String },
    /// Network error talking to the router.
    Net(String),
}

impl core::fmt::Display for UpnpErr {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            UpnpErr::NoGateway => write!(f, "未发现支持 UPnP 的路由器（SSDP 无响应）。路由器可能关闭了 UPnP，或本机处于运营商内网（CGNAT）。"),
            UpnpErr::NoIgd => write!(f, "路由器响应了 SSDP 但未提供 UPnP 端口映射服务（InternetGatewayDevice）。"),
            UpnpErr::Refused { code, detail } => {
                let hint = match code {
                    402 => "参数无效",
                    501 => "路由器执行动作失败",
                    606 => "路由器不允许此操作（UPnP 可能被设置为只读）",
                    718 => "端口映射冲突（外部端口可能已被占用）",
                    725 => "路由器仅支持永久租约",
                    _ => "路由器拒绝",
                };
                write!(f, "{hint}（UPnP 错误码 {code}：{detail}）")
            }
            UpnpErr::Net(e) => write!(f, "与路由器通信失败：{e}"),
        }
    }
}

// ---------------------------------------------------------------- SSDP

/// One M-SEARCH discovery result worth acting on.
struct SsdpHit {
    /// Base URL of the device description, e.g. `http://192.168.1.1:5000/igd.xml`.
    location: String,
}

/// The ST values worth searching for, in priority order. `ssdp:all` floods
/// the network with every device's response; the targeted ones keep the
/// 3-second window useful.
const SEARCH_TARGETS: [&str; 3] = [
    "urn:schemas-upnp-org:device:InternetGatewayDevice:1",
    "urn:schemas-upnp-org:service:WANIPConnection:1",
    "urn:schemas-upnp-org:service:WANPPPConnection:1",
];

fn msearch(st: &str) -> String {
    // Header order follows the miniupnpc reference implementation (ST before
    // MAN); MAN's value must be quoted. CRLF line endings are mandatory.
    format!(
        "M-SEARCH * HTTP/1.1\r\n\
         HOST: 239.255.255.250:1900\r\n\
         ST: {st}\r\n\
         MAN: \"ssdp:discover\"\r\n\
         MX: 2\r\n\
         \r\n"
    )
}

/// Send M-SEARCH on each target and collect distinct LOCATION values.
fn ssdp_discover() -> Vec<SsdpHit> {
    let Ok(sock) = UdpSocket::bind(("0.0.0.0", 0)) else {
        return Vec::new();
    };
    let _ = sock.set_read_timeout(Some(Duration::from_millis(400)));
    let multicast: SocketAddrV4 = "239.255.255.250:1900"
        .parse()
        .expect("constant parses");

    let mut seen: Vec<String> = Vec::new();
    for st in SEARCH_TARGETS {
        let packet = msearch(st);
        if sock.send_to(packet.as_bytes(), multicast).is_err() {
            continue;
        }
        // Drain responses for the remainder of the window.
        let deadline = std::time::Instant::now() + SSDP_WAIT;
        let mut buf = [0u8; 2048];
        loop {
            let now = std::time::Instant::now();
            if now >= deadline {
                break;
            }
            let _ = sock.set_read_timeout(Some(deadline - now));
            match sock.recv_from(&mut buf) {
                Ok((n, _from)) => {
                    let text = String::from_utf8_lossy(&buf[..n]).to_ascii_lowercase();
                    // Case-insensitive header scan; LOCATION value keeps its case.
                    for line in String::from_utf8_lossy(&buf[..n]).lines() {
                        if let Some(v) = line.trim().strip_prefix("location:").map(str::trim) {
                            if !v.is_empty() && !seen.iter().any(|s| s.eq_ignore_ascii_case(v)) {
                                seen.push(v.to_string());
                            }
                        }
                    }
                    let _ = text; // scanned above
                }
                Err(_) => break, // timeout for this ST
            }
        }
    }
    seen.into_iter().map(|location| SsdpHit { location }).collect()
}

// ---------------------------------------------------------------- HTTP over TCP

/// Minimal HTTP GET for device-description XML (router LAN, plain HTTP).
fn http_get(url: &str) -> Res<String> {
    // Parse scheme://host[:port]/path — routers never redirect, never TLS here.
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| format!("UPnP 描述地址不是 http：{url}"))?;
    let (hostport, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) => (h, p.parse::<u16>().map_err(|_| format!("UPnP 端口无效：{hostport}"))?),
        None => (hostport, 80),
    };
    let addr = format!("{host}:{port}");
    let mut conn = std::net::TcpStream::connect(&addr)
        .map_err(|e| format!("无法连接路由器 {addr}：{e}"))?;
    conn.set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| e.to_string())?;
    conn.set_write_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| e.to_string())?;
    use std::io::{Read, Write};
    let req = format!(
        "GET {path} HTTP/1.1\r\nHost: {hostport}\r\nConnection: close\r\nUser-Agent: LightFoxLauncher/{}\r\n\r\n",
        crate::VERSION
    );
    conn.write_all(req.as_bytes())
        .map_err(|e| format!("发送 HTTP 请求失败：{e}"))?;
    let mut raw = Vec::new();
    conn.read_to_end(&mut raw)
        .map_err(|e| format!("读取路由器响应失败：{e}"))?;
    let text = String::from_utf8_lossy(&raw);
    // Split headers/body without a full HTTP parser: body starts after \r\n\r\n.
    let body = text
        .split_once("\r\n\r\n")
        .map(|(_, b)| b)
        .unwrap_or(&text)
        .to_string();
    Ok(body)
}

/// Minimal HTTP POST with extra headers; returns (status, body).
fn http_post(url: &str, soap_action: &str, body: &str) -> Res<(u32, String)> {
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| format!("UPnP 控制地址不是 http：{url}"))?;
    let (hostport, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) => (h, p.parse::<u16>().map_err(|_| format!("UPnP 端口无效：{hostport}"))?),
        None => (hostport, 80),
    };
    let addr = format!("{host}:{port}");
    let mut conn = std::net::TcpStream::connect(&addr)
        .map_err(|e| format!("无法连接路由器 {addr}：{e}"))?;
    conn.set_read_timeout(Some(Duration::from_secs(8)))
        .map_err(|e| e.to_string())?;
    conn.set_write_timeout(Some(Duration::from_secs(8)))
        .map_err(|e| e.to_string())?;
    use std::io::{Read, Write};
    let req = format!(
        "POST {path} HTTP/1.1\r\nHost: {hostport}\r\nContent-Type: text/xml; charset=\"utf-8\"\r\nSOAPACTION: \"{soap_action}\"\r\nContent-Length: {}\r\nConnection: close\r\nUser-Agent: LightFoxLauncher/{}\r\n\r\n{}",
        body.len(),
        crate::VERSION,
        body
    );
    conn.write_all(req.as_bytes())
        .map_err(|e| format!("发送 SOAP 请求失败：{e}"))?;
    let mut raw = Vec::new();
    conn.read_to_end(&mut raw)
        .map_err(|e| format!("读取 SOAP 响应失败：{e}"))?;
    let text = String::from_utf8_lossy(&raw);
    let status: u32 = text
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let body = text
        .split_once("\r\n\r\n")
        .map(|(_, b)| b)
        .unwrap_or(&text)
        .to_string();
    Ok((status, body))
}

// ---------------------------------------------------------------- XML helpers

/// Extract inner text of the first `<tag>…</tag>`, matching the tag name
/// exactly at element boundaries: the opening tag may carry attributes and a
/// namespace prefix (`<x:tag …>` matches `tag`), and a longer element whose
/// name merely starts with `tag` must not match.
fn xml_text(xml: &str, tag: &str) -> Option<String> {
    let lower = xml.to_ascii_lowercase();
    let want = tag.to_ascii_lowercase();
    // A namespace prefix (`x:tag`) may precede the tag name; search for the
    // bare name and verify the character before it is '<' or ':'.
    let mut search = 0usize;
    loop {
        let i = lower[search..].find(&want)?;
        let start = search + i;
        let before = if start == 0 { b'\0' } else { lower.as_bytes()[start - 1] };
        let prefix_ok = before == b'<' || before == b':';
        let after = start + want.len();
        let name_end = lower.as_bytes().get(after).copied().unwrap_or(b'\0');
        let exact = name_end == b'>' || name_end == b' ' || name_end == b'\t' || name_end == b'\r' || name_end == b'\n' || name_end == b'/';
        if prefix_ok && exact {
            let Some(gt_rel) = lower[after..].find('>') else { return None };
            let gt = after + gt_rel;
            // Closing tag is `</tag>` or `</prefix:tag>`. Find the first '</'
            // whose name part equals our tag (with optional prefix).
            let from = gt + 1;
            let bytes = lower.as_bytes();
            let mut close_rel: Option<usize> = None;
            let mut scan = from;
            while let Some(i) = lower[scan..].find("</") {
                let open = scan + i;
                let name_start = open + 2;
                // name part: up to '>' — may be 'tag' or 'prefix:tag'
                if let Some(end) = lower[name_start..].find('>') {
                    let name = &lower[name_start..name_start + end];
                    let bare = name.rsplit(':').next().unwrap_or(name);
                    if bare == want {
                        close_rel = Some(name_start - from);
                        break;
                    }
                    scan = name_start + end;
                } else {
                    break;
                }
            }
            let Some(rel) = close_rel else {
                search = after;
                continue;
            };
            // Inner text runs from after the opening '>' to the '<' of the
            // closing tag.
            let inner_end = from + rel - 2; // '</' is two chars before name_start
            if inner_end < from {
                search = after;
                continue;
            }
            let _ = bytes;
            return Some(xml[from..inner_end].trim().to_string());
        }
        search = start + want.len();
    }
}

/// First `<tag>…</tag>` (for device descriptions where the first hit is the
/// one we want).
#[allow(dead_code)]
fn xml_text_first(xml: &str, tag: &str) -> Option<String> {
    xml_text(xml, tag)
}

/// Append a relative controlURL to the description URL base.
fn join_url(base: &str, rel: &str) -> String {
    if rel.starts_with("http://") || rel.starts_with("https://") {
        return rel.to_string();
    }
    // If the document declared <URLBase>, it has been substituted by the caller.
    if rel.starts_with('/') {
        // scheme://host[:port] + rel
        if let Some(i) = base.find("://") {
            let after = &base[i + 3..];
            let host_end = after.find('/').map(|x| i + 3 + x).unwrap_or(base.len());
            return format!("{}{}", &base[..host_end], rel);
        }
    }
    // Relative without a leading slash: replace the document filename.
    match base.rfind('/') {
        Some(i) if base[i..].contains('.') && i > base.find("://").map(|x| x + 2).unwrap_or(0) => {
            format!("{}{}", &base[..i + 1], rel)
        }
        _ => format!("{}/{}", base.trim_end_matches('/'), rel),
    }
}

// ---------------------------------------------------------------- IGD flow

/// A control endpoint we can send SOAP to.
struct IgdControl {
    control_url: String,
    service_type: String,
}

/// Find a usable control URL in a device description document.
fn find_control(doc_url: &str, doc: &str) -> Option<IgdControl> {
    // URLBase overrides the fetch URL when present (IGD v1).
    let base = xml_text(doc, "URLBase").unwrap_or_else(|| doc_url.to_string());
    // Walk service blocks: crude split on <service> — each holds serviceType
    // and controlURL. Prefer WANIPConnection, fall back to WANPPPConnection.
    let mut fallback: Option<IgdControl> = None;
    let lower = doc.to_ascii_lowercase();
    let mut pos = 0usize;
    while let Some(i) = lower[pos..].find("<service>") {
        let start = pos + i;
        let Some(end_rel) = lower[start..].find("</service>") else { break };
        let block = &doc[start..start + end_rel];
        let stype = xml_text(block, "serviceType").unwrap_or_default();
        let curl = xml_text(block, "controlURL").unwrap_or_default();
        if !stype.is_empty() && !curl.is_empty() {
            let ctl = IgdControl {
                control_url: join_url(&base, &curl),
                service_type: stype.clone(),
            };
            if stype.contains("WANIPConnection") {
                return Some(ctl);
            }
            if stype.contains("WANPPPConnection") && fallback.is_none() {
                fallback = Some(ctl);
            }
        }
        pos = start + end_rel;
    }
    fallback
}

/// Discover a control endpoint: SSDP, then description fetch, then parse.
fn discover_control() -> Result<IgdControl, UpnpErr> {
    let hits = ssdp_discover();
    if hits.is_empty() {
        return Err(UpnpErr::NoGateway);
    }
    for h in &hits {
        if let Ok(doc) = http_get(&h.location) {
            if let Some(ctl) = find_control(&h.location, &doc) {
                return Ok(ctl);
            }
        }
    }
    Err(UpnpErr::NoIgd)
}

fn soap_envelope(service_type: &str, action: &str, args: &str) -> String {
    // Written as one literal line: Rust's `\` line-continuation swallows the
    // newline but keeps the next line's leading spaces, which would corrupt
    // the XML (a space inside `</u:AddPortMapping >` is tolerated by some
    // routers and rejected by others — no reason to risk either).
    format!(
        "<?xml version=\"1.0\"?><s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\"><s:Body><u:{action} xmlns:u=\"{service_type}\">{args}</u:{action}></s:Body></s:Envelope>"
    )
}

/// Pull the UPnP errorCode/ errorDescription out of a fault response.
fn parse_fault(body: &str) -> Option<(u32, String)> {
    let code = xml_text(body, "errorCode")?.parse().ok()?;
    let detail = xml_text(body, "errorDescription").unwrap_or_default();
    Some((code, detail))
}

/// The public outcome of a mapping attempt.
#[derive(Debug, Clone, PartialEq)]
pub struct Mapping {
    pub external_ip: String,
    pub external_port: u16,
    /// Seconds the mapping lives; 0 = router said permanent.
    pub lease: u32,
}

/// Try to map `external_port → internal_port` on this machine for `lease`
/// seconds, and return the external address friends should connect to.
pub fn add_mapping(external_port: u16, internal_port: u16, lease: u32) -> Result<Mapping, UpnpErr> {
    let ctl = discover_control()?;

    // Internal client: the local IPv4 the router knows us by. Pick the first
    // non-link-local address; the gateway list comes from the same adapter
    // set, so on a normal home LAN this is the right interface.
    let internal = hostnet::detect()
        .v4_local
        .first()
        .and_then(|s| s.parse::<Ipv4Addr>().ok())
        .ok_or_else(|| UpnpErr::Net("本机没有可用的 IPv4 地址".to_string()))?;

    let action = "AddPortMapping";
    let args = format!(
        "<NewRemoteHost></NewRemoteHost>\
         <NewExternalPort>{external_port}</NewExternalPort>\
         <NewProtocol>TCP</NewProtocol>\
         <NewInternalPort>{internal_port}</NewInternalPort>\
         <NewInternalClient>{internal}</NewInternalClient>\
         <NewEnabled>1</NewEnabled>\
         <NewPortMappingDescription>LightFoxLauncher</NewPortMappingDescription>\
         <NewLeaseDuration>{lease}</NewLeaseDuration>"
    );
    let envelope = soap_envelope(&ctl.service_type, action, &args);
    let soap_action = format!("{}#{action}", ctl.service_type);
    let (status, body) = http_post(&ctl.control_url, &soap_action, &envelope)
        .map_err(UpnpErr::Net)?;

    if !(200..300).contains(&status) {
        let (code, detail) = parse_fault(&body).unwrap_or((status, body.chars().take(120).collect()));
        // 725 = only permanent leases supported: retry once with lease 0.
        if code == 725 {
            return add_mapping_inner(&ctl, external_port, internal_port, 0, internal);
        }
        return Err(UpnpErr::Refused { code, detail });
    }

    let external_ip = get_external_ip(&ctl).unwrap_or_default();
    Ok(Mapping {
        external_ip,
        external_port,
        lease,
    })
}

/// Shared tail so the 725 retry does not recurse through discovery again.
fn add_mapping_inner(
    ctl: &IgdControl,
    external_port: u16,
    internal_port: u16,
    lease: u32,
    internal: Ipv4Addr,
) -> Result<Mapping, UpnpErr> {
    let action = "AddPortMapping";
    let args = format!(
        "<NewRemoteHost></NewRemoteHost>\
         <NewExternalPort>{external_port}</NewExternalPort>\
         <NewProtocol>TCP</NewProtocol>\
         <NewInternalPort>{internal_port}</NewInternalPort>\
         <NewInternalClient>{internal}</NewInternalClient>\
         <NewEnabled>1</NewEnabled>\
         <NewPortMappingDescription>LightFoxLauncher</NewPortMappingDescription>\
         <NewLeaseDuration>{lease}</NewLeaseDuration>"
    );
    let envelope = soap_envelope(&ctl.service_type, action, &args);
    let soap_action = format!("{}#{action}", ctl.service_type);
    let (status, body) = http_post(&ctl.control_url, &soap_action, &envelope)
        .map_err(UpnpErr::Net)?;
    if !(200..300).contains(&status) {
        let (code, detail) = parse_fault(&body).unwrap_or((status, body.chars().take(120).collect()));
        return Err(UpnpErr::Refused { code, detail });
    }
    let external_ip = get_external_ip(ctl).unwrap_or_default();
    Ok(Mapping {
        external_ip,
        external_port,
        lease,
    })
}

/// GetExternalIPAddress; failures degrade to empty (the mapping still works).
fn get_external_ip(ctl: &IgdControl) -> Res<String> {
    let action = "GetExternalIPAddress";
    let envelope = soap_envelope(&ctl.service_type, action, "");
    let soap_action = format!("{}#{action}", ctl.service_type);
    let (status, body) = http_post(&ctl.control_url, &soap_action, &envelope)?;
    if (200..300).contains(&status) {
        if let Some(ip) = xml_text(&body, "NewExternalIPAddress") {
            return Ok(ip);
        }
    }
    Ok(String::new())
}

/// Best-effort removal of a mapping we created. Errors are returned for the
/// UI to log, not to act on: a stale mapping expires with its lease anyway.
pub fn delete_mapping(external_port: u16) -> Res<()> {
    let ctl = discover_control().map_err(|e| e.to_string())?;
    let action = "DeletePortMapping";
    let args = format!(
        "<NewRemoteHost></NewRemoteHost>\
         <NewExternalPort>{external_port}</NewExternalPort>\
         <NewProtocol>TCP</NewProtocol>"
    );
    let envelope = soap_envelope(&ctl.service_type, action, &args);
    let soap_action = format!("{}#{action}", ctl.service_type);
    let _ = http_post(&ctl.control_url, &soap_action, &envelope)
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xml_text_handles_namespaces_and_attributes() {
        let doc = r#"<a xmlns:x="urn:x"><x:NewExternalPort>25565</x:NewExternalPort></a>"#;
        assert_eq!(xml_text(doc, "NewExternalPort").as_deref(), Some("25565"));
        let doc2 = r#"<NewLeaseDuration xmlns="urn:u">14400</NewLeaseDuration>"#;
        assert_eq!(xml_text(doc2, "NewLeaseDuration").as_deref(), Some("14400"));
        let doc3 = r#"<root><errorCode>718</errorCode><errorDescription>Conflict</errorDescription></root>"#;
        assert_eq!(xml_text(doc3, "errorCode").as_deref(), Some("718"));
        assert_eq!(xml_text(doc3, "errorDescription").as_deref(), Some("Conflict"));
        // No match -> None, never a panic.
        assert_eq!(xml_text("<a>1</a>", "b"), None);
        assert_eq!(xml_text("", "a"), None);
    }

    #[test]
    fn xml_text_prefix_is_exact_not_prefix_of_longer_tag() {
        // <NewPortMappingNotes> must NOT be read as <NewPortMappingNote>.
        // Our matcher walks to the closing </NewPortMappingNote> which does not
        // exist here, so the result is None.
        let doc = "<NewPortMappingNotes>xx</NewPortMappingNotes>";
        assert_eq!(xml_text(doc, "NewPortMappingNote"), None);
    }

    #[test]
    fn join_url_variants() {
        assert_eq!(
            join_url("http://192.168.1.1/igd.xml", "/ctl/IPConn"),
            "http://192.168.1.1/ctl/IPConn"
        );
        assert_eq!(
            join_url("http://192.168.1.1:5000/igd.xml", "ctl/IPConn"),
            "http://192.168.1.1:5000/ctl/IPConn"
        );
        assert_eq!(
            join_url("http://192.168.1.1/igd.xml", "http://other/x"),
            "http://other/x"
        );
        // URLBase honoured by caller substitution:
        assert_eq!(
            join_url("http://192.168.1.1/base/", "/ctl/IPConn"),
            "http://192.168.1.1/ctl/IPConn"
        );
        // Trailing-slash base + relative name (no dot in last segment).
        assert_eq!(
            join_url("http://192.168.1.1:5000/ctrl/", "IPConn.xml"),
            "http://192.168.1.1:5000/ctrl/IPConn.xml"
        );
    }

    #[test]
    fn fault_parsing() {
        let body = r#"<?xml version="1.0"?>
        <s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/">
        <s:Body><s:Fault>
        <detail><UPnPError>
        <errorCode>718</errorCode>
        <errorDescription>ConflictInMappingEntry</errorDescription>
        </UPnPError></detail>
        </s:Fault></s:Body></s:Envelope>"#;
        let (code, detail) = parse_fault(body).expect("fault parses");
        assert_eq!(code, 718);
        assert_eq!(detail, "ConflictInMappingEntry");
    }

    #[test]
    fn msearch_packets_look_right() {
        let p = msearch("urn:schemas-upnp-org:device:InternetGatewayDevice:1");
        assert!(p.starts_with("M-SEARCH * HTTP/1.1\r\n"));
        assert!(p.contains("HOST: 239.255.255.250:1900\r\n"));
        // ST precedes MAN, per the miniupnpc reference order.
        let st = p.find("ST: ").expect("ST present");
        let man = p.find("MAN: ").expect("MAN present");
        assert!(st < man, "ST must come before MAN");
        assert!(p.contains("MAN: \"ssdp:discover\"\r\n"));
        assert!(p.contains("MX: 2\r\n"));
        assert!(p.ends_with("\r\n\r\n"));
    }

    #[test]
    fn envelope_carries_service_type_and_action() {
        let e = soap_envelope("urn:schemas-upnp-org:service:WANIPConnection:1", "AddPortMapping", "<A>1</A>");
        assert!(e.contains("xmlns:u=\"urn:schemas-upnp-org:service:WANIPConnection:1\""));
        // The action element opens with attributes following, so match the
        // opening tag shape rather than a literal `>` after the name.
        assert!(e.contains("<u:AddPortMapping "));
        assert!(e.contains("<A>1</A>"));
        assert!(e.contains("</u:AddPortMapping></s:Body></s:Envelope>"));
        // The whole document must be a single line: the format! continuation
        // bug this pins down used to inject a space before '</s:Body>'.
        assert!(!e.contains("</s:Body> </s:Envelope>"));
        assert_eq!(e.lines().count(), 1);
    }

    #[test]
    fn find_control_prefers_wanip_over_ppp() {
        let doc = r#"<root>
        <serviceList>
        <service><serviceType>urn:schemas-upnp-org:service:WANPPPConnection:1</serviceType>
        <controlURL>/ctl/ppp</controlURL></service>
        <service><serviceType>urn:schemas-upnp-org:service:WANIPConnection:1</serviceType>
        <controlURL>/ctl/ip</controlURL></service>
        </serviceList></root>"#;
        let base = "http://192.168.1.1/igd.xml";
        let c = find_control(base, doc).expect("control found");
        assert_eq!(c.control_url, "http://192.168.1.1/ctl/ip");
        assert!(c.service_type.contains("WANIPConnection"));
    }

    #[test]
    fn find_control_falls_back_to_ppp() {
        let doc = r#"<serviceList>
        <service><serviceType>urn:schemas-upnp-org:service:WANPPPConnection:1</serviceType>
        <controlURL>/ppp</controlURL></service></serviceList>"#;
        let c = find_control("http://192.168.1.1/igd.xml", doc).expect("ppp fallback");
        assert_eq!(c.control_url, "http://192.168.1.1/ppp");
    }

    #[test]
    fn find_control_rejects_unrelated_device() {
        let doc = "<serviceList><service><serviceType>urn:schemas-upnp-org:service:Print:1</serviceType>
        <controlURL>/p</controlURL></service></serviceList>";
        assert!(find_control("http://x/igd.xml", doc).is_none());
    }

    #[test]
    fn upnp_error_messages_are_actionable() {
        let e = UpnpErr::Refused { code: 718, detail: "x".into() };
        let s = e.to_string();
        assert!(s.contains("冲突") && s.contains("718"));
        let e = UpnpErr::NoGateway;
        assert!(e.to_string().contains("UPnP") && e.to_string().contains("CGNAT"));
    }
}
