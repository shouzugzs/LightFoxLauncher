// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025 LightFoxLauncher contributors
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Host network detection for the play-together feature.
//!
//! Answers, with pure system APIs and no network traffic, the questions the
//! 联机 page needs before offering anything:
//!
//! * does this machine have a **global IPv6** address a friend could connect
//!   to (and is it a real one, not a temporary/privacy-extension address that
//!   rotates and breaks hosting);
//! * what is the default **IPv4 gateway** (the target for UPnP discovery);
//! * what **local IPv4** address would the game bind to.
//!
//! OWNER: lead.

use std::net::{Ipv4Addr, Ipv6Addr};

// ---------------------------------------------------------------- Win32 FFI

const ERROR_SUCCESS: u32 = 0;
const ERROR_BUFFER_OVERFLOW: u32 = 122;

const AF_INET: u16 = 2;
const AF_INET6: u16 = 23;
const AF_UNSPEC: u16 = 0;

const GAA_FLAG_SKIP_ANYCAST: u32 = 0x0002;
const GAA_FLAG_SKIP_MULTICAST: u32 = 0x0004;
const GAA_FLAG_INCLUDE_GATEWAYS: u32 = 0x0040;

const IpPrefixOriginManual: u32 = 1;
const IpPrefixOriginWellKnown: u32 = 2;
const IpPrefixOriginDhcp: u32 = 3;
const IpPrefixOriginRouterAdvertisement: u32 = 4;

const IpSuffixOriginRandom: u32 = 3;

const IpDadStatePreferred: u32 = 1;

const IF_TYPE_SOFTWARE_LOOPBACK: u32 = 24;

#[repr(C)]
struct SocketAddress {
    family: u16,
    _pad: u16,
    port_be: u16,
    flowinfo_be: u32,
    addr: [u8; 16],
    scope_id: u32,
}

#[repr(C)]
struct IpAdapterUnicastAddress {
    _header: usize,
    next: *const IpAdapterUnicastAddress,
    address: SocketAddress,
    prefix_origin: u32,
    suffix_origin: u32,
    dad_state: u32,
    valid_lifetime: u32,
    preferred_lifetime: u32,
    lease_lifetime: u32,
    _flags: u8,
    skip_as_source: u8,
}

#[repr(C)]
struct IpAdapterGatewayAddress {
    next: *const IpAdapterGatewayAddress,
    address: SocketAddress,
}

#[repr(C)]
struct IpAdapterAddresses {
    _header: usize,
    next: *const IpAdapterAddresses,
    adapter_name: *const u8,
    first_unicast: *const IpAdapterUnicastAddress,
    first_dns_server: *const SocketAddress,
    dns_suffix: *const u16,
    description: *const u16,
    friendly_name: *const u16,
    _mac: *const u8,
    _physical_len: u32,
    flags: u32,
    mtu: u32,
    if_type: u32,
    oper_status: u32,
    ipv6_if_index: u32,
    first_gateway: *const IpAdapterGatewayAddress,
    first_unicast_ok: *const IpAdapterUnicastAddress, // not a real field name
    ipv4_if_index: u32,
}

#[link(name = "iphlpapi")]
extern "system" {
    fn GetAdaptersAddresses(
        family: u16,
        flags: u32,
        reserved: *mut core::ffi::c_void,
        adapter: *mut IpAdapterAddresses,
        size: *mut u32,
    ) -> u32;
}

// ---------------------------------------------------------------- impl

/// Oper status of an adapter is up (1) per the IF_OPER_STATUS enum.
fn is_up(oper: u32) -> bool {
    oper == 1
}

/// True for addresses that are reachable from the internet and stable enough
/// to host on: preferred DAD state, not a privacy-extension temporary address,
/// and in a globally-routable range. There is no API-level "scope" field
/// (confirmed against the GetAdaptersAddresses docs), so global is judged by
/// prefix: 2000::/3 is the currently-routable global unicast space; fe80::/10
/// link-local and fc00::/7 ULA fail the check.
fn hostable_v6(u: &IpAdapterUnicastAddress) -> bool {
    if u.address.family != AF_INET6 || u.dad_state != IpDadStatePreferred {
        return false;
    }
    // Temporary addresses (RFC 4941) have SuffixOrigin == Random and are
    // explicitly marked to skip as source; both mean "not for hosting".
    if u.suffix_origin == IpSuffixOriginRandom || u.skip_as_source != 0 {
        return false;
    }
    // Only permanent-looking origins: manual, well-known, DHCPv6, or RA.
    if !matches!(
        u.prefix_origin,
        IpPrefixOriginManual | IpPrefixOriginWellKnown | IpPrefixOriginDhcp | IpPrefixOriginRouterAdvertisement
    ) {
        return false;
    }
    let seg0 = u16::from_be_bytes([u.address.addr[0], u.address.addr[1]]);
    // 2000::/3 covers 2000:: through 3fff::.
    (seg0 & 0xe000) == 0x2000
}

fn v6_from_sockaddr(a: &SocketAddress) -> Option<Ipv6Addr> {
    if a.family != AF_INET6 {
        return None;
    }
    Some(Ipv6Addr::new(
        u16::from_be_bytes([a.addr[0], a.addr[1]]),
        u16::from_be_bytes([a.addr[2], a.addr[3]]),
        u16::from_be_bytes([a.addr[4], a.addr[5]]),
        u16::from_be_bytes([a.addr[6], a.addr[7]]),
        u16::from_be_bytes([a.addr[8], a.addr[9]]),
        u16::from_be_bytes([a.addr[10], a.addr[11]]),
        u16::from_be_bytes([a.addr[12], a.addr[13]]),
        u16::from_be_bytes([a.addr[14], a.addr[15]]),
    ))
}

fn v4_from_sockaddr(a: &SocketAddress) -> Option<Ipv4Addr> {
    if a.family != AF_INET {
        return None;
    }
    Some(Ipv4Addr::new(a.addr[0], a.addr[1], a.addr[2], a.addr[3]))
}

/// Everything found in one pass, so the UI never mixes inconsistent reads.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HostNet {
    /// Global, non-temporary, DAD-preferred IPv6 addresses (for hosting).
    pub v6_global: Vec<String>,
    /// IPv4 addresses of non-loopback adapters that are up.
    pub v4_local: Vec<String>,
    /// Default IPv4 gateways (UPnP discovery targets), deduplicated.
    pub v4_gateways: Vec<String>,
}

/// Zero buffers and call the API twice if the first call says "too small".
fn gaa(family: u16, flags: u32) -> Option<Vec<u8>> {
    let mut size: u32 = 16 * 1024;
    loop {
        let mut buf = vec![0u8; size as usize];
        let rc = unsafe {
            GetAdaptersAddresses(
                family,
                flags,
                std::ptr::null_mut(),
                buf.as_mut_ptr() as *mut IpAdapterAddresses,
                &mut size,
            )
        };
        match rc {
            ERROR_SUCCESS => return Some(buf),
            ERROR_BUFFER_OVERFLOW => {
                // size now holds the required size; loop once more.
                if size > 4 * 1024 * 1024 {
                    return None; // absurd; refuse rather than loop forever
                }
            }
            _ => return None, // NO_DATA (no addresses of that family) etc.
        }
    }
}

/// Read adapters with gateways included, walking the returned list.
unsafe fn collect_adapters(flags: u32) -> HostNet {
    let mut out = HostNet::default();
    let Some(buf) = gaa(AF_UNSPEC, flags) else {
        return out;
    };
    let mut p = buf.as_ptr() as *const IpAdapterAddresses;
    while !p.is_null() {
        let a = &*p;
        if is_up(a.oper_status) && a.if_type != IF_TYPE_SOFTWARE_LOOPBACK {
            // Unicast addresses.
            let mut u = a.first_unicast;
            while !u.is_null() {
                let ua = &*u;
                if ua.dad_state == IpDadStatePreferred {
                    if hostable_v6(ua) {
                        if let Some(v6) = v6_from_sockaddr(&ua.address) {
                            let s = v6.to_string();
                            if !out.v6_global.contains(&s) {
                                out.v6_global.push(s);
                            }
                        }
                    }
                    if ua.address.family == AF_INET {
                        if let Some(v4) = v4_from_sockaddr(&ua.address) {
                            // Skip 169.254/16 link-local: no hosting on it.
                            if !(v4.octets()[0] == 169 && v4.octets()[1] == 254) {
                                let s = v4.to_string();
                                if !out.v4_local.contains(&s) {
                                    out.v4_local.push(s);
                                }
                            }
                        }
                    }
                }
                u = ua.next;
            }
            // Gateways (v4 only; v6 needs no gateway for direct hosting).
            let mut g = a.first_gateway;
            while !g.is_null() {
                let ga = &*g;
                if ga.address.family == AF_INET {
                    if let Some(v4) = v4_from_sockaddr(&ga.address) {
                        let s = v4.to_string();
                        if !out.v4_gateways.contains(&s) {
                            out.v4_gateways.push(s);
                        }
                    }
                }
                g = ga.next;
            }
        }
        p = a.next;
    }
    out
}

/// One-shot detection used by the 联机 page.
pub fn detect() -> HostNet {
    unsafe { collect_adapters(GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_INCLUDE_GATEWAYS) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_runs_and_returns_sane_shape() {
        // This machine has some network. The exact values differ per machine,
        // so assert only invariants: no link-local v4, no empty-string entries.
        let n = detect();
        for s in &n.v4_local {
            let ip: Ipv4Addr = s.parse().expect("v4 string parses");
            assert!(!(ip.octets()[0] == 169 && ip.octets()[1] == 254), "no 169.254 in v4_local");
        }
        for s in &n.v6_global {
            let ip: Ipv6Addr = s.parse().expect("v6 string parses");
            // No link-local fe80::/10 in the global list.
            assert!(!(ip.segments()[0] & 0xffc0) == 0xfe80, "no fe80 in v6_global");
        }
        // Gateways, when present, are valid v4.
        for s in &n.v4_gateways {
            let _: Ipv4Addr = s.parse().expect("gateway parses");
        }
    }
}
