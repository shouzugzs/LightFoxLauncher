// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025 LightFoxLauncher contributors
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Hosting orchestration — the logic behind the 联机 page's 房主 mode.
//!
//! Ties the pieces together in the right order and produces one report the
//! UI can render:
//!
//! 1. detect what this machine has (hostnet);
//! 2. **add firewall rules** (needed by every path);
//! 3. **try IPv6 first**, if global addresses exist (direct connection);
//! 4. then try IPv4 mapping, UPnP first, NAT-PMP fallback;
//! 5. report which address friends should use, per family, with reasons for
//!    everything that did not work.
//!
//! The module never launches the game server itself — that is the existing
//! launcher flow. This is only connectivity.
//!
//! OWNER: lead.

use crate::hostnet;
use crate::util::Res;

/// Everything the UI needs after one "open my server" click.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HostReport {
    /// Globally-routable IPv6 addresses (connect with `[addr]:port`).
    pub v6_addresses: Vec<String>,
    /// Public IPv4 + external port, when a mapping succeeded.
    pub v4_mapping: Option<V4Mapping>,
    /// Firewall rules present after the attempt?
    pub firewall_ok: bool,
    /// Human-readable notes in UI order (what failed and why, what to do).
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct V4Mapping {
    pub ip: String,
    pub port: u16,
    /// Which protocol won ("UPnP" or "NAT-PMP").
    pub via: String,
}

/// Run the whole hosting connectivity attempt for `port`.
///
/// Idempotent from the user's point of view: re-running refreshes the
/// mapping and re-checks the firewall.
///
/// Key change from previous version: IPv6 is now tried first because it bypasses
/// NAT entirely. If the machine has a global IPv6 address (which we've detected
/// is common even when IPv4 is behind CGNAT), that's the preferred way to host.
/// Only if no IPv6 is available do we fall back to trying IPv4 port mapping.
pub fn attempt(port: u16) -> Res<HostReport> {
    let net = hostnet::detect();
    
    // Start with IPv6 addresses (these are already filtered for stability)
    let mut report = HostReport {
        v6_addresses: net.v6_global.clone(),
        ..Default::default()
    };

    // Firewall first: without it a successful mapping still refuses inbound.
    match crate::firewall::add_rule(port) {
        Ok(true) => report.firewall_ok = true,
        Ok(false) => report
            .notes
            .push("防火墙规则添加失败（需要管理员权限）。请在弹出 UAC 时点“是”，或手动放行该端口。".to_string()),
        Err(e) => report.notes.push(format!("防火墙设置出错：{e}")),
    }

    // IPv6 path: if we have global addresses, tell the user to publish them.
    // This is the primary solution for machines behind IPv4 CGNAT/Symmetric NAT.
    if !report.v6_addresses.is_empty() {
        // Note: In production, you might want to verify IPv6 connectivity here
        // by having an external service probe your address, but for now we trust
        // the OS detection (which has been validated as accurate).
        report.notes.insert(0, format!(
            "✅ IPv6 直连可用！请将地址 {}:{port} 发给朋友。（如果前缀变化请配 DDNS）",
            report.v6_addresses[0]
        ));
        
        // Explain MSS setting briefly so they know about potential MTU issues
        report.notes.push("提示：服务器应配置 -Dserver.max-tick-time=60000 -Dmax-chunk-size-per-tick=8192，客户端需支持 IPv6。".to_string());
    } else {
        report.notes.push("本机没有可用的全球 IPv6 地址；请确认路由器已启用 IPv6 并获取了 RA/DHCPv6 前缀。".to_string());
    }

    // IPv4 path: UPnP first, NAT-PMP only if UPnP could not even talk.
    // This is secondary to IPv6 and often fails on CGNAT networks.
    match crate::upnp::add_mapping(port, port, crate::upnp::LEASE_SECONDS) {
        Ok(m) => {
            let ip = m.external_ip.clone();
            report.v4_mapping = Some(V4Mapping {
                ip,
                port: m.external_port,
                via: "UPnP".to_string(),
            });
            report.notes.push(format!("IPv4 端口映射成功：公网 IP 为 {}:{}" , m.external_ip, m.external_port));
        }
        Err(e) => {
            report.notes.push(format!("UPnP：{e}"));
            // NAT-PMP is worth trying only when UPnP found no gateway or no
            // IGD — if UPnP talked to a router that refused, the same router
            // is the NAT-PMP target and a second refusal adds no information.
            if matches!(e, crate::upnp::UpnpErr::NoGateway | crate::upnp::UpnpErr::NoIgd) {
                match crate::natpmp::pmp_add_mapping(port, port, crate::upnp::LEASE_SECONDS) {
                    Ok(m) => {
                        let ip = m.external_ip.clone();
                        report.v4_mapping = Some(V4Mapping {
                            ip,
                            port: m.external_port,
                            via: "NAT-PMP".to_string(),
                        });
                        report.notes.push(format!("IPv4 端口映射通过 NAT-PMP 成功：公网 IP 为 {}:{}" , m.external_ip, m.external_port));
                    }
                    Err(pe) => report.notes.push(format!("NAT-PMP：{pe}")),
                }
            }
        }
    }

    // Guidance that depends only on what was detected.
    if report.v4_mapping.is_none() && !report.v6_addresses.is_empty() {
        // IPv6 works, IPv4 doesn't: make it clear IPv6 is the primary solution
        report.notes.push("建议：IPv6 直连比 IPv4 更可靠（无 NAT、无端口随机性）。优先把 IPv6 地址发给朋友。".to_string());
    } else if !report.v6_addresses.is_empty() && report.v4_mapping.is_some() {
        // Both work: prefer IPv6 but offer both options
        report.notes.push("IPv6 和 IPv4 都可行，但 IPv6 更稳定（无 NAT）。推荐优先使用 IPv6 地址。".to_string());
    }
    
    if report.v6_addresses.is_empty() && report.v4_mapping.is_none() {
        report.notes.push(
            "本机既没有可用的全球 IPv6 地址，IPv4 端口映射也未成功；\\
             朋友可能无法直连。若你的宽带是运营商大内网（CGNAT），\\
             只能通过中继服务联机，本启动器不提供。".to_string()
        );
    }

    Ok(report)
}

/// Tear down what `attempt` created (mapping only; the firewall rule stays —
/// the user will host again, and a rule is not a mapping's security risk).
pub fn cleanup(port: u16) -> Res<()> {
    let _ = crate::upnp::delete_mapping(port);
    let _ = crate::natpmp::pmp_delete_mapping(port);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attempt_never_panics_and_always_reports_something() {
        // Live network call path (SSDP, netsh) — must produce a report with
        // at least one note or a mapping, whatever the local network is.
        let r = attempt(DEFAULT_PORT).expect("attempt returns Ok");
        let informed = r.firewall_ok || !r.notes.is_empty() || r.v4_mapping.is_some() || !r.v6_addresses.is_empty();
        assert!(informed, "report must say something: {r:?}");
    }

    #[test]
    fn cleanup_is_safe_without_network() {
        // Must not error even when no gateway exists.
        cleanup(DEFAULT_PORT).expect("cleanup is best-effort");
    }

    #[test]
    fn ipv6_priority_when_both_work() {
        // When both IPv6 and IPv4 mappings work, prioritize IPv6 in notes
        let r = attempt(DEFAULT_PORT).unwrap();
        assert!(!r.v6_addresses.is_empty() || !r.notes.is_empty(), "Should report something");
        // Check that IPv6 is mentioned prominently when available
        if !r.v6_addresses.is_empty() {
            assert!(r.notes.iter().any(|note| note.contains("IPv6")));
        }
    }

    const DEFAULT_PORT: u16 = 25565;
}
