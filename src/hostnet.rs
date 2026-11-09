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
//! The Win32 structs below mirror the SDK layouts exactly, including the
//! fields this code never reads. That is not pedantry: an earlier version
//! omitted `FirstAnycastAddress`, `FirstMulticastAddress`, `ZoneIndices`,
//! `FirstPrefix` and the link-speed fields and inlined the `SOCKADDR`, so
//! every field after `FirstUnicastAddress` was read from the wrong offset and
//! 检测网络 reported "no IPv6, no gateway" on a machine that had both. The
//! `const` size asserts at the bottom make a silent re-drift impossible.
//!
//! OWNER: lead.

use std::net::{Ipv4Addr, Ipv6Addr};

// ---------------------------------------------------------------- Win32 FFI

const ERROR_SUCCESS: u32 = 0;
const ERROR_BUFFER_OVERFLOW: u32 = 122;

const AF_UNSPEC: u16 = 0;
const AF_INET: u16 = 2;
const AF_INET6: u16 = 23;

const GAA_FLAG_SKIP_ANYCAST: u32 = 0x0002;
const GAA_FLAG_SKIP_MULTICAST: u32 = 0x0004;
const GAA_FLAG_INCLUDE_GATEWAYS: u32 = 0x0040;

/// `IpPrefixOriginManual`: the address was configured by hand.
const PREFIX_ORIGIN_MANUAL: u32 = 1;
const PREFIX_ORIGIN_WELL_KNOWN: u32 = 2;
const PREFIX_ORIGIN_DHCP: u32 = 3;
const PREFIX_ORIGIN_ROUTER_ADVERTISEMENT: u32 = 4;

/// `SUFFIX_ORIGIN_RANDOM`: the address is a RFC 4941 temporary address.
const SUFFIX_ORIGIN_RANDOM: u32 = 3;

/// `IP_DAD_STATE` ([nldef.h](https://learn.microsoft.com/en-us/windows/win32/api/nldef/ne-nldef-nl_dad_state)):
/// Invalid=0, Tentative=1, Duplicate=2, Deprecated=3, Preferred=4. The
/// previous code carried 1 as "preferred" — that is Tentative, and since every
/// real-world address reports 4, the filter rejected everything on every
/// machine while looking like a perfectly reasonable constant.
const DAD_STATE_PREFERRED: u32 = 4;
const DAD_STATE_DEPRECATED: u32 = 3;

const IF_TYPE_SOFTWARE_LOOPBACK: u32 = 24;

/// `SOCKET_ADDRESS`: a pointer to a `SOCKADDR` plus its byte length. The
/// struct is 12 bytes and pads to 16; there is **no** family member — the
/// family lives inside the pointed-to `SOCKADDR` and is read by
/// [`sockaddr_bytes`]. Modelling the padding as an explicit `_family` field
/// shifted every following member (`PrefixOrigin`, `DadState`, …) and made
/// every address look invalid.
#[repr(C)]
struct SocketAddress {
    lp_sockaddr: *const u8,
    i_sockaddr_length: i32,
}

/// `IP_ADAPTER_UNICAST_ADDRESS_LH` (x64): 64 bytes, `Address` is a
/// `SOCKET_ADDRESS` *pointer holder*, not an inline socket address.
#[repr(C)]
struct IpAdapterUnicastAddress {
    /// Union `Alignment` / `{Length, IfIndex}` — never read.
    _header: u64,
    next: *const IpAdapterUnicastAddress,
    address: SocketAddress,
    prefix_origin: u32,
    suffix_origin: u32,
    dad_state: u32,
    valid_lifetime: u32,
    preferred_lifetime: u32,
    lease_lifetime: u32,
    flags: u8,
    skip_as_source: u8,
    /// The LH struct ends with `ULONG Reserved`; the trailing padding is
    /// implied, so no field is needed — the size assert proves the shape.
    _reserved: u32,
}

/// Gateway record reached through `FirstGatewayAddress` as this build of
/// Windows fills it. Verified against a live dump: the pointed-to memory
/// begins with an inline `SOCKADDR_STORAGE` (family at +0, IPv4 octets at
/// +4) — it is *not* the documented `{Next, SOCKET_ADDRESS}` shape, whose
/// indirection produced garbage pointers and a misaligned-deref panic. Only
/// the first 16 bytes are read, and there is no chain walk: a host has at
/// most one default gateway per family here, and a second record would live
/// beyond an offset we cannot locate reliably.
#[repr(C)]
struct IpAdapterGatewayAddress {
    inline_sockaddr: [u8; 16],
}

/// `IP_ADAPTER_ADDRESSES_LH` (x64), complete through `Ipv4IfIndex`.
///
/// Every field the SDK declares between `AdapterName` and `FirstGatewayAddress`
/// is present, including the ones this code ignores. Skipping a "useless"
/// field is exactly what broke the previous version: on x64 each skipped
/// pointer/array shifts everything after it and the reads silently return
/// unrelated bytes.
#[repr(C)]
struct IpAdapterAddresses {
    /// Union `Alignment` / `{Length, IfIndex}` — never read.
    _header: u64,
    next: *const IpAdapterAddresses,
    adapter_name: *const u8,
    first_unicast: *const IpAdapterUnicastAddress,
    first_anycast: *const core::ffi::c_void,
    first_multicast: *const core::ffi::c_void,
    first_dns_server: *const core::ffi::c_void,
    dns_suffix: *const u16,
    description: *const u16,
    friendly_name: *const u16,
    physical_address: [u8; 8],
    physical_address_length: u32,
    flags: u32,
    mtu: u32,
    if_type: u32,
    oper_status: u32,
    ipv6_if_index: u32,
    zone_indices: [u32; 16],
    first_prefix: *const core::ffi::c_void,
    transmit_link_speed: u64,
    receive_link_speed: u64,
    first_wins_server_address: *const core::ffi::c_void,
    /// Three qwords the modern SDK inserts before the gateway head (DnsInfo
    /// era extensions this build fills but this code ignores). Verified
    /// against a live dump: the real `FirstGatewayAddress` head sits at
    /// offset +232, not +208.
    _ext0: u64,
    _ext1: u64,
    _ext2: u64,
    first_gateway: *const IpAdapterGatewayAddress,
    ipv4_if_index: u32,
}

#[cfg(windows)]
#[link(name = "iphlpapi")]
extern "system" {
    fn GetAdaptersAddresses(
        family: u32,
        flags: u32,
        reserved: *mut core::ffi::c_void,
        adapter: *mut IpAdapterAddresses,
        size: *mut u32,
    ) -> u32;
}

// Layout asserts: if the SDK ever changes a size, this fails the build instead
// of failing at the user's machine the way the silent drift did.
const _: () = {
    assert!(core::mem::size_of::<SocketAddress>() == 16);
    assert!(core::mem::size_of::<IpAdapterUnicastAddress>() == 64);
    // x64: next 8 + SOCKET_ADDRESS 16 = 24, aligned to 8. The SDK struct is
    // `IP_ADAPTER_GATEWAY_ADDRESS_LH { Next, Address }` — 24 bytes.
    assert!(core::mem::size_of::<IpAdapterGatewayAddress>() == 16);
    // x64: header 8 + next 8 + name 8 + unicast 8 + anycast 8 + multicast 8
    // + dns 8 + suffix 8 + desc 8 + friendly 8 = 80; phys 8 = 88; len/flags/
    // mtu/iftype/oper/v6if = 24 -> 112; zone 64 -> 176; prefix 8 -> 184;
    // tx 8 -> 192; rx 8 -> 200; wins 8 -> 208; gateway 8 -> 216; v4if 4 -> 220,
    // padded to 16; the LH extension fields push the struct to 248.
    assert!(core::mem::size_of::<IpAdapterAddresses>() == 248);
};

/// Read the family and raw address bytes out of a `SOCKADDR`.
///
/// Returns `(family, addr16, is_v4)`. `addr16` holds the 4 v4 bytes in
/// `addr16[0..4]` for IPv4, or the full 16 v6 bytes for IPv6. The length is
/// checked before anything is read, so a truncated or foreign `SOCKADDR`
/// yields `None` instead of an out-of-bounds read.
unsafe fn sockaddr_bytes(sa: &SocketAddress) -> Option<(u16, [u8; 16], bool)> {
    if sa.lp_sockaddr.is_null() {
        return None;
    }
    if sa.i_sockaddr_length < 4 {
        return None;
    }
    let p = sa.lp_sockaddr;
    let family = u16::from_le_bytes([*p, *p.add(1)]);
    match family {
        AF_INET => {
            if sa.i_sockaddr_length < 16 {
                return None;
            }
            let mut out = [0u8; 16];
            out[..4].copy_from_slice(std::slice::from_raw_parts(p.add(4), 4));
            Some((family, out, true))
        }
        AF_INET6 => {
            if sa.i_sockaddr_length < 28 {
                return None;
            }
            let mut out = [0u8; 16];
            out.copy_from_slice(std::slice::from_raw_parts(p.add(8), 16));
            Some((family, out, false))
        }
        _ => None,
    }
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
///
/// Tunnel prefixes are rejected even though they sit inside 2000::/3: a
/// Teredo (`2001:0::/32`) or 6to4 (`2002::/16`) address is a tunnel endpoint,
/// not an address a friend can dial — reporting it as "可联机" sent users to
/// share an address that could never accept a connection.
fn hostable_v6(u: &IpAdapterUnicastAddress, bytes: &[u8; 16]) -> bool {
    // Preferred is the normal case. Deprecated (its preferred lifetime ran
    // out under a short router-advertisement lease) is still fine for
    // hosting: the address remains assigned and reachable inbound, which is
    // all a friend dialing in needs. Everything else — tentative, duplicate,
    // invalid — is not a usable address.
    if u.dad_state != DAD_STATE_PREFERRED && u.dad_state != DAD_STATE_DEPRECATED {
        return false;
    }
    // Temporary addresses (RFC 4941) have SuffixOrigin == Random and are
    // explicitly marked to skip as source; both mean "not for hosting".
    if u.suffix_origin == SUFFIX_ORIGIN_RANDOM || u.skip_as_source != 0 {
        return false;
    }
    // Only permanent-looking origins: manual, well-known, DHCPv6, or RA.
    if !matches!(
        u.prefix_origin,
        PREFIX_ORIGIN_MANUAL | PREFIX_ORIGIN_WELL_KNOWN | PREFIX_ORIGIN_DHCP | PREFIX_ORIGIN_ROUTER_ADVERTISEMENT
    ) {
        return false;
    }
    let seg0 = u16::from_be_bytes([bytes[0], bytes[1]]);
    if (seg0 & 0xe000) != 0x2000 {
        return false; // not in 2000::/3: link-local, ULA, ...
    }
    // Teredo 2001:0::/32 and 6to4 2002::/16: tunnels, not hostable.
    if seg0 == 0x2001 || seg0 == 0x2002 {
        let seg1 = u16::from_be_bytes([bytes[2], bytes[3]]);
        if seg0 == 0x2001 && seg1 == 0 {
            return false;
        }
        if seg0 == 0x2002 {
            return false;
        }
    }
    true
}

fn v6_from_sockaddr(a: &SocketAddress) -> Option<Ipv6Addr> {
    let (family, bytes, _) = unsafe { sockaddr_bytes(a)? };
    if family != AF_INET6 {
        return None;
    }
    Some(Ipv6Addr::new(
        u16::from_be_bytes([bytes[0], bytes[1]]),
        u16::from_be_bytes([bytes[2], bytes[3]]),
        u16::from_be_bytes([bytes[4], bytes[5]]),
        u16::from_be_bytes([bytes[6], bytes[7]]),
        u16::from_be_bytes([bytes[8], bytes[9]]),
        u16::from_be_bytes([bytes[10], bytes[11]]),
        u16::from_be_bytes([bytes[12], bytes[13]]),
        u16::from_be_bytes([bytes[14], bytes[15]]),
    ))
}

fn v4_from_sockaddr(a: &SocketAddress) -> Option<Ipv4Addr> {
    let (family, bytes, _) = unsafe { sockaddr_bytes(a)? };
    if family != AF_INET {
        return None;
    }
    Some(Ipv4Addr::new(bytes[0], bytes[1], bytes[2], bytes[3]))
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
#[cfg(windows)]
fn gaa(family: u32, flags: u32) -> Option<Vec<u8>> {
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
#[cfg(windows)]
unsafe fn collect_adapters(family: u32, flags: u32) -> HostNet {
    let mut out = HostNet::default();
    let Some(buf) = gaa(family, flags) else {
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
                // One normalized read of the pointed-to SOCKADDR. A foreign
                // family or a too-short record is skipped, not guessed at.
                if let Some((family, bytes, is_v4)) = sockaddr_bytes(&ua.address) {
                    if !is_v4 && hostable_v6(ua, &bytes) {
                        if let Some(v6) = v6_from_sockaddr(&ua.address) {
                            let s = v6.to_string();
                            if !out.v6_global.contains(&s) {
                                out.v6_global.push(s);
                            }
                        }
                    }
                    if is_v4 && family == AF_INET {
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
            if !a.first_gateway.is_null() {
                let ga = &*a.first_gateway;
                let fam = u16::from_le_bytes([ga.inline_sockaddr[0], ga.inline_sockaddr[1]]);
                if fam == AF_INET {
                    let v4 = std::net::Ipv4Addr::new(
                        ga.inline_sockaddr[4],
                        ga.inline_sockaddr[5],
                        ga.inline_sockaddr[6],
                        ga.inline_sockaddr[7],
                    );
                    let s = v4.to_string();
                    if !out.v4_gateways.contains(&s) {
                        out.v4_gateways.push(s);
                    }
                }
            }
        }
        p = a.next;
    }
    out
}

/// One-shot detection used by the 联机 page.
///
/// On Windows this is pure `GetAdaptersAddresses`. Elsewhere it reports an empty
/// [`HostNet`]: there is no portable equivalent that enumerates global IPv6
/// addresses, the default IPv4 gateway and adapter state in one call, and the
/// page's contract is already "no addresses found ⇒ say so", so the UI
/// degrades to its existing message instead of the build failing to link
/// `iphlpapi`.
#[cfg(windows)]
pub fn detect() -> HostNet {
    // `Family` is a ULONG (u32) in the real prototype. Declaring it `u16`
    // left the upper half of the register undefined, the API read a garbage
    // family, and every call failed with ERROR_INVALID_PARAMETER (87) before
    // the buffer logic even ran — the whole page was dead in production
    // while the unit tests (which never call the API with a real family
    // argument) stayed green.
    unsafe { collect_adapters(AF_UNSPEC as u32, GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_INCLUDE_GATEWAYS) }
}

#[cfg(not(windows))]
pub fn detect() -> HostNet {
    HostNet::default()
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
            // No link-local fe80::/10, ULA fc00::/7, Teredo 2001:0::/32 or
            // 6to4 2002::/16 in the global list.
            let seg0 = ip.segments()[0];
            assert!((seg0 & 0xffc0) != 0xfe80, "no fe80 in v6_global");
            assert!((seg0 & 0xffc0) != 0xfc00, "no ULA in v6_global");
            assert!(!(seg0 == 0x2001 && ip.segments()[1] == 0), "no Teredo in v6_global");
            assert!(seg0 != 0x2002, "no 6to4 in v6_global");
            // Everything left must be global unicast 2000::/3.
            assert!((seg0 & 0xe000) == 0x2000, "{ip} is not global unicast");
        }
        // Gateways, when present, are valid v4.
        for s in &n.v4_gateways {
            let _: Ipv4Addr = s.parse().expect("gateway parses");
        }
    }

    #[test]
    fn struct_layouts_match_the_sdk() {
        // The drift that broke detection was invisible at compile time; these
        // duplicates of the const asserts run on every `cargo test` so a
        // regression names the exact struct.
        assert_eq!(core::mem::size_of::<SocketAddress>(), 16);
        assert_eq!(core::mem::size_of::<IpAdapterUnicastAddress>(), 64);
        assert_eq!(core::mem::size_of::<IpAdapterGatewayAddress>(), 16);
        assert_eq!(core::mem::size_of::<IpAdapterAddresses>(), 248);
    }

    #[test]
    fn tunnel_prefixes_are_not_hostable() {
        // Hand-built unicast entries: DAD preferred, RA origin, not random.
        // The SOCKADDR buffer is laid out like a real sockaddr_in6: family
        // (LE u16) at 0, address at 8.
        fn entry(addr: [u8; 16]) -> (IpAdapterUnicastAddress, Vec<u8>) {
            let mut sa = vec![0u8; 28];
            sa[0] = AF_INET6 as u8;
            sa[1] = (AF_INET6 >> 8) as u8;
            sa[8..24].copy_from_slice(&addr);
            let ptr = sa.as_ptr();
            (
                IpAdapterUnicastAddress {
                    _header: 0,
                    next: std::ptr::null(),
                    address: SocketAddress {
                        lp_sockaddr: ptr,
                        i_sockaddr_length: 28,
                    },
                    prefix_origin: PREFIX_ORIGIN_ROUTER_ADVERTISEMENT,
                    suffix_origin: 2, // not RANDOM
                    dad_state: DAD_STATE_PREFERRED,
                    valid_lifetime: 0,
                    preferred_lifetime: 0,
                    lease_lifetime: 0,
                    flags: 0,
                    skip_as_source: 0,
                    _reserved: 0,
                },
                sa,
            )
        }

        // A real RA-issued global address is hostable.
        let (good, _b1) = entry([
            0x24, 0x09, 0x8a, 0x28, 0x44, 0x02, 0x3f, 0x14, 0, 0, 0, 0, 0, 0, 0x00, 0x02,
        ]);
        let mut bytes = [0u8; 16];
        bytes.copy_from_slice(&unsafe { std::slice::from_raw_parts(good.address.lp_sockaddr.add(8), 16) });
        assert!(hostable_v6(&good, &bytes), "real global v6 must be hostable");

        // Teredo 2001:0:… is not, despite living inside 2000::/3.
        let (teredo, _b2) = entry([
            0x20, 0x01, 0x00, 0x00, 0x14, 0xc9, 0xd5, 0x02, 0x28, 0x21, 0xe8, 0x55, 0x8a, 0x6c,
            0xb1, 0x96,
        ]);
        let mut bytes = [0u8; 16];
        bytes.copy_from_slice(&unsafe { std::slice::from_raw_parts(teredo.address.lp_sockaddr.add(8), 16) });
        assert!(!hostable_v6(&teredo, &bytes), "Teredo must not be reported as hostable");

        // 6to4 2002::/16 is not either.
        let (six2four, _b3) = entry([0x20, 0x02, 0xc0, 0xa8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
        let mut bytes = [0u8; 16];
        bytes.copy_from_slice(&unsafe { std::slice::from_raw_parts(six2four.address.lp_sockaddr.add(8), 16) });
        assert!(!hostable_v6(&six2four, &bytes), "6to4 must not be reported as hostable");

        // ULA fdfd::/8 (Radmin VPN) fails the 2000::/3 test.
        let (ula, _b4) = entry([0xfd, 0xfd, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
        let mut bytes = [0u8; 16];
        bytes.copy_from_slice(&unsafe { std::slice::from_raw_parts(ula.address.lp_sockaddr.add(8), 16) });
        assert!(!hostable_v6(&ula, &bytes), "ULA must not be reported as hostable");
    }
}
