// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025 LightFoxLauncher contributors
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! NAT-PMP / PCP port mapping — the second "attempt" path after UPnP.
//!
//! Some routers do not run UPnP but do speak NAT-PMP (RFC 6886) or PCP
//! (RFC 6887). Both are simple binary UDP protocols to the default gateway on
//! port 5351, which makes them cheap to try: PCP first (modern), NAT-PMP as
//! the fallback (older Apple-style gateways).
//!
//! OWNER: lead.

use std::net::{Ipv4Addr, UdpSocket};
use std::time::Duration;

use crate::hostnet;
use crate::util::Res;
use crate::upnp::Mapping;

/// RFC 6886 / 6887: both protocols listen on the gateway at 5351.
const PMP_PORT: u16 = 5351;

/// Retry schedule from RFC 6886 §3.1: 250 ms doubling, up to 9 attempts.
const RETRY_MS: [u64; 9] = [250, 500, 1000, 2000, 4000, 8000, 16000, 32000, 64000];

/// A NAT-PMP/PCP refusal or failure.
#[derive(Debug, Clone, PartialEq)]
pub enum PmpErr {
    /// No default gateway to ask.
    NoGateway,
    /// The gateway did not answer within the retry budget.
    Timeout,
    /// The gateway answered with a non-zero result code.
    Refused { code: u16, detail: String },
    /// Local socket problems.
    Net(String),
}

impl core::fmt::Display for PmpErr {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            PmpErr::NoGateway => write!(f, "没有默认网关，无法尝试 NAT-PMP/PCP。"),
            PmpErr::Timeout => write!(
                f, "网关未响应 NAT-PMP/PCP（已重试 9 次）。路由器可能不支持该协议。"
            ),
            PmpErr::Refused { code, detail } => write!(f, "网关拒绝（NAT-PMP/PCP 结果码 {code}：{detail}）"),
            PmpErr::Net(e) => write!(f, "NAT-PMP/PCP 通信失败：{e}"),
        }
    }
}

fn result_detail(code: u16) -> &'static str {
    match code {
        1 => "不支持的版本",
        2 => "网关拒绝",
        3 => "网络故障",
        4 => "资源耗尽",
        5 => "不支持的操作",
        6 => "资源不足",
        7 => "地址不匹配",
        10 => "超出用户配额",
        11 => "无法提供外部地址",
        12 => "地址不匹配（映射属于其他主机）",
        _ => "未知错误",
    }
}

/// Ask hostnet for the default gateway; shared by both protocols.
fn gateway() -> Result<Ipv4Addr, PmpErr> {
    hostnet::detect()
        .v4_gateways
        .first()
        .and_then(|s| s.parse().ok())
        .ok_or(PmpErr::NoGateway)
}

/// Send `req` with the RFC 6886 retry schedule; verify the reply came from
/// the gateway (spoofed replies are otherwise a mapping-injection vector).
fn send_with_retries(gw: Ipv4Addr, req: &[u8], expect_len: usize) -> Result<Vec<u8>, PmpErr> {
    let sock = UdpSocket::bind(("0.0.0.0", 0)).map_err(|e| PmpErr::Net(e.to_string()))?;
    let dst = (gw, PMP_PORT);
    let mut buf = vec![0u8; 128];
    for &ms in &RETRY_MS {
        if sock.send_to(req, dst).is_err() {
            continue;
        }
        sock.set_read_timeout(Some(Duration::from_millis(ms)))
            .map_err(|e| PmpErr::Net(e.to_string()))?;
        loop {
            match sock.recv_from(&mut buf) {
                Ok((n, from)) => {
                    // RFC 6886 §3.3: the response MUST come from the gateway.
                    if from.ip() != std::net::IpAddr::V4(gw) {
                        continue; // spoofed or stray; keep waiting
                    }
                    if n >= expect_len {
                        return Ok(buf[..n].to_vec());
                    }
                    // Short packet: keep waiting for a well-formed one.
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
                {
                    break; // next, longer attempt
                }
                Err(e) => return Err(PmpErr::Net(e.to_string())),
            }
        }
    }
    Err(PmpErr::Timeout)
}

/// NAT-PMP TCP map request (opcode 2), 12 bytes, big-endian.
fn pmp_request(internal_port: u16, external_port: u16, lifetime: u32) -> [u8; 12] {
    let mut r = [0u8; 12];
    r[0] = 0; // version
    r[1] = 2; // opcode: Map TCP
    // r[2..4] reserved
    r[4..6].copy_from_slice(&internal_port.to_be_bytes());
    r[6..8].copy_from_slice(&external_port.to_be_bytes());
    r[8..12].copy_from_slice(&lifetime.to_be_bytes());
    r
}

/// Try NAT-PMP. `external_port == 0` lets the gateway pick.
pub fn pmp_add_mapping(external_port: u16, internal_port: u16, lifetime: u32) -> Result<Mapping, PmpErr> {
    let gw = gateway()?;
    let req = pmp_request(internal_port, external_port, lifetime);
    let resp = send_with_retries(gw, &req, 16)?;
    // Response: ver(1) opcode|128(1) result(2) epoch(4) int(2) ext(2) lease(4)
    let result = u16::from_be_bytes([resp[2], resp[3]]);
    if result != 0 {
        return Err(PmpErr::Refused {
            code: result,
            detail: result_detail(result).to_string(),
        });
    }
    let got_internal = u16::from_be_bytes([resp[8], resp[9]]);
    let got_external = u16::from_be_bytes([resp[10], resp[11]]);
    let _got_lease = u32::from_be_bytes([resp[12], resp[13], resp[14], resp[15]]);
    let _ = got_internal; // echo of our request; not interesting to the user
    Ok(Mapping {
        external_ip: pmp_external_ip(gw)?.external_ip,
        external_port: got_external,
        lease: lifetime,
    })
}

/// NAT-PMP public-address request (2 bytes) → external IPv4.
fn pmp_external_ip(gw: Ipv4Addr) -> Result<Mapping, PmpErr> {
    let resp = send_with_retries(gw, &[0, 0], 8)?;
    let result = u16::from_be_bytes([resp[2], resp[3]]);
    if result != 0 {
        return Err(PmpErr::Refused {
            code: result,
            detail: result_detail(result).to_string(),
        });
    }
    Ok(Mapping {
        external_ip: Ipv4Addr::new(resp[4 + 4], resp[5 + 4], resp[6 + 4], resp[7 + 4]).to_string(),
        external_port: 0,
        lease: 0,
    })
}

/// Best-effort removal (lifetime 0, suggested external port 0).
pub fn pmp_delete_mapping(external_port: u16) -> Res<()> {
    let Ok(gw) = gateway() else {
        return Ok(()); // nothing to delete against no gateway
    };
    let _ = send_with_retries(gw, &pmp_request(external_port, external_port, 0), 16);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pmp_request_layout() {
        let r = pmp_request(25565, 0, 7200);
        assert_eq!(r[0], 0, "version 0");
        assert_eq!(r[1], 2, "opcode 2 = Map TCP");
        assert_eq!(&r[2..4], &[0, 0], "reserved");
        assert_eq!(&r[4..6], &25565u16.to_be_bytes());
        assert_eq!(&r[6..8], &0u16.to_be_bytes());
        assert_eq!(&r[8..12], &7200u32.to_be_bytes());
    }

    #[test]
    fn retry_schedule_matches_rfc() {
        assert_eq!(RETRY_MS[0], 250);
        assert_eq!(RETRY_MS.len(), 9);
        for w in RETRY_MS.windows(2) {
            assert_eq!(w[1], w[0] * 2, "doubling");
        }
    }

    #[test]
    fn pmp_error_messages_name_the_code() {
        let e = PmpErr::Refused { code: 2, detail: "refused".into() };
        assert!(e.to_string().contains("2"));
        assert!(e.to_string().contains("拒绝"));
    }
}
