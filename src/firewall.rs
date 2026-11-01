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

/// Serialise firewall mutation during tests. The hosting integration tests and
/// this module's own test share the single global netsh rule name, and
/// `cargo test` runs them in parallel; a concurrent `remove` from another
/// thread would delete the rule one test just added and flake its count. The
/// lock is taken only under `#[cfg(test)]`, so production behaviour is
/// unchanged.
#[cfg(test)]
static TEST_FIREWALL_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Delete every rule we own, without the test lock. `add_rule` calls this
/// directly so it does not try to re-lock the mutex it already holds.
fn delete_owned_rules() -> Res<()> {
    let _ = run_netsh(&format!(
        "advfirewall firewall delete rule name=\"{RULE_NAME}\""
    ))?;
    Ok(())
}

/// Add inbound rules for `port` (TCP + UDP). Returns false when the command
/// could not run (not elevated, netsh missing); the UI should say so rather
/// than pretend the rule exists.
///
/// Idempotent across calls and resilient to a port change: any existing rule
/// we own is removed first, then exactly one TCP + one UDP rule for `port` is
/// created. Without the removal step, repeated 开服 clicks (or switching the
/// hosting port) would stack duplicate rules with the same name — Windows keeps
/// every copy, which is how a single host ends up with dozens of identical
/// entries, all widening the same hole.
pub fn add_rule(port: u16) -> Res<bool> {
    #[cfg(test)]
    let _guard = TEST_FIREWALL_LOCK.lock().unwrap();
    // Remove any prior rule we own (possibly a different port) so we end with
    // exactly the rules for the requested port. `delete_owned_rules` is a
    // no-op when nothing is there, and swallows elevation/ netsh failures.
    let _ = delete_owned_rules();
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
    #[cfg(test)]
    let _guard = TEST_FIREWALL_LOCK.lock().unwrap();
    delete_owned_rules()
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

    /// Count how many of our rules netsh currently lists (Windows only).
    ///
    /// `netsh` localises its headers (e.g. `规则名称:` on a zh-CN box), so we
    /// do not match a header string. We count occurrences of `RULE_NAME`
    /// itself, which netsh prints once per rule regardless of language.
    #[cfg(windows)]
    fn count_our_rules() -> usize {
        use std::os::windows::process::CommandExt;
        let out = std::process::Command::new("cmd")
            .args(["/C", &format!("netsh advfirewall firewall show rule name=\"{RULE_NAME}\"")])
            .creation_flags(0x0800_0000)
            .output()
            .expect("netsh runs");
        let text = String::from_utf8_lossy(&out.stdout);
        text.matches(RULE_NAME).count()
    }

    #[test]
    fn add_rule_is_idempotent_and_not_port_fragile() {
        // Regression: repeated 开服 clicks (and switching the port) must not
        // stack duplicate rules. On non-Windows there is no netsh, so skip.
        if !cfg!(windows) {
            return;
        }
        let _ = remove_rule();
        // Creating rules needs elevation; if we are not elevated the command
        // fails (add_rule reports Ok(false)) and there is nothing to verify.
        let Ok(true) = add_rule(25565) else {
            return;
        };
        let first = count_our_rules();
        assert!(first >= 1, "expected at least one rule after add");
        // Call again with the same port — count must not grow.
        add_rule(25565).expect("add again");
        assert_eq!(count_our_rules(), first, "re-adding same port stacked duplicates");
        // Switching the port must not leave the old one behind and must not
        // stack: remove+readd keeps exactly the same number of rules.
        add_rule(25570).expect("add other port");
        assert_eq!(count_our_rules(), first, "port change left stale rules or stacked");
        let _ = remove_rule();
    }
}
