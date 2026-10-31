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
//! 2. add firewall rules (needed by every path);
//! 3. try a port mapping, UPnP first, NAT-PMP/PCP second;
//! 4. report the address friends should use, per family, with reasons for
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
pub fn attempt(port: u16) -> Res<HostReport> {
    let net = hostnet::detect();
    let mut report = HostReport {
        v6_addresses: net.v6_global.clone(),
        ..Default::default()
    };

    // Firewall first: without it a successful mapping still refuses inbound.
    match crate::firewall::add_rule(port) {
        Ok(true) => report.firewall_ok = true,
        Ok(false) => report
            .notes
            .push("防火墙规则添加失败（需要管理员权限）。请在弹出UAC时点“是”，或手动放行该端口。".to_string()),
        Err(e) => report.notes.push(format!("防火墙设置出错：{e}")),
    }

    // IPv4 path: UPnP first, NAT-PMP only if UPnP could not even talk.
    match crate::upnp::add_mapping(port, port, crate::upnp::LEASE_SECONDS) {
        Ok(m) => {
            report.v4_mapping = Some(V4Mapping {
                ip: m.external_ip,
                port: m.external_port,
                via: "UPnP".to_string(),
            });
        }
        Err(e) => {
            report.notes.push(format!("UPnP：{e}"));
            // NAT-PMP is worth trying only when UPnP found no gateway or no
            // IGD — if UPnP talked to a router that refused, the same router
            // is the NAT-PMP target and a second refusal adds no information.
            if matches!(e, crate::upnp::UpnpErr::NoGateway | crate::upnp::UpnpErr::NoIgd) {
                match crate::natpmp::pmp_add_mapping(port, port, crate::upnp::LEASE_SECONDS) {
                    Ok(m) => {
                        report.v4_mapping = Some(V4Mapping {
                            ip: m.external_ip,
                            port: m.external_port,
                            via: "NAT-PMP".to_string(),
                        });
                    }
                    Err(pe) => report.notes.push(format!("NAT-PMP：{pe}")),
                }
            }
        }
    }

    // Guidance that depends only on what was detected.
    if report.v4_mapping.is_none() && net.v6_global.is_empty() {
        report.notes.push(
            "本机既没有可用的全球 IPv6 地址，IPv4 端口映射也未成功；\
             朋友可能无法直连。若你的宽带是运营商大内网（CGNAT），\
             只能通过中继服务联机，本启动器不提供。"
                .to_string(),
        );
    }
    if !net.v6_global.is_empty() && report.v4_mapping.is_none() {
        report.notes.push(
            "IPv4 端口映射未成功；IPv6 直连可用时建议优先把 IPv6 地址发给朋友。".to_string(),
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

    const DEFAULT_PORT: u16 = 25565;
}
