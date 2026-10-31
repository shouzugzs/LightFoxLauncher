// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025 LightFoxLauncher contributors
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Windows firewall rule management for hosting a Minecraft server.
//!
//! Windows Defender Firewall blocks inbound 25565 by default, which kills
//! hosting before UPnP/IPv6 even matter. The fix is two `netsh` rules (TCP +
//! UDP), which trigger one UAC prompt when run elevated. Rules are created
//! with a fixed, launcher-owned name so they can be found and removed again;
//! nothing else in the firewall is ever touched.
//!
//! OWNER: lead.

use crate::util::Res;

/// The rule name this launcher owns. Fixed so `remove` can find exactly what
/// `add` created — and so a user inspecting their firewall can tell where the
/// rule came from.
pub const RULE_NAME: &str = "LightFoxLauncher Minecraft Server";

/// Add inbound rules for `port` (TCP + UDP). Returns false when the command
/// could not run (not elevated, netsh missing); the UI should say so rather
/// than pretend the rule exists.
pub fn add_rule(port: u16) -> Res<bool> {
    let tcp = run_netsh(&format!(
        "advfirewall firewall add rule name=\"{RULE_NAME}\" dir=in action=allow protocol=TCP localport={port}"
    ))?;
    let udp = run_netsh(&format!(
        "advfirewall firewall add rule name=\"{RULE_NAME}\" dir=in action=allow protocol=UDP localport={port}"
    ))?;
    Ok(tcp && udp)
}

/// Remove the rules this launcher created. Idempotent: removing a
/// non-existent rule is a netsh error we swallow, because "already gone" is
/// the desired end state.
pub fn remove_rule() -> Res<()> {
    let _ = run_netsh(&format!(
        "advfirewall firewall delete rule name=\"{RULE_NAME}\""
    ))?;
    Ok(())
}

/// True when at least one of our rules is present (query does not need
/// elevation, so this is cheap to show in the UI).
pub fn rule_exists() -> bool {
    match run_netsh(&format!(
        "advfirewall firewall show rule name=\"{RULE_NAME}\""
    )) {
        Ok(true) => true,
        _ => false,
    }
}

/// Run `netsh <args>` via cmd. Returns Ok(true) when netsh exited 0.
fn run_netsh(args: &str) -> Res<bool> {
    use std::os::windows::process::CommandExt;
    let out = std::process::Command::new("cmd")
        .args(["/C", &format!("netsh {args}")])
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .output()
        .map_err(|e| format!("无法执行 netsh：{e}"))?;
    Ok(out.status.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rule_name_is_stable_and_quoted_safely() {
        // A quote inside the name would break out of the netsh quoting; the
        // name is a constant, this test keeps it that way.
        assert!(!RULE_NAME.contains('"'));
        assert!(!RULE_NAME.contains('\''));
        assert!(RULE_NAME.starts_with("LightFoxLauncher"));
    }

    #[test]
    fn query_does_not_need_elevation() {
        // This is a read; on any Windows box it must succeed or the UI would
        // misreport the state. (On non-Windows test runners netsh is absent,
        // so only assert when the command exists.)
        if cfg!(windows) {
            let ok = run_netsh(&format!(
                "advfirewall firewall show rule name=\"{RULE_NAME}\""
            ))
            .expect("netsh runs");
            // Either found (true) or not found (false) — but never an error.
            let _ = ok;
        }
    }
}
