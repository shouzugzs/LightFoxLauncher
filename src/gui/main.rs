// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025 LightFoxLauncher contributors
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! LFL-gui — the launcher window, or a pointer to the CLI on other platforms.
//!
//! The real UI is native Win32: one window class, a handful of child controls
//! and GDI drawing, with no toolkit and no crate. That is a Windows API, so the
//! application lives in [`win_main`] behind a `cfg` and this file only dispatches.
//!
//! The gate has to be here rather than in `Cargo.toml` because Cargo has no
//! per-platform `[[bin]]`: without it, `cargo build` on Linux or macOS would try
//! to link a binary that references `user32`, `gdi32` and `comctl32`.

#![windows_subsystem = "windows"]

#[cfg(windows)]
#[path = "win_main.rs"]
mod win_main;

#[cfg(windows)]
fn main() {
    win_main::main();
}

/// Non-Windows builds ship the CLI only; say so plainly instead of failing.
#[cfg(not(windows))]
fn main() {
    eprintln!(
        "LFL-gui is a native Win32 window and is not available on this platform.\n\
         Use the `LFL` command-line launcher instead — it runs every download,\n\
         verification and launch step this GUI would have run.\n\n\
         Run `LFL --help` for the command list."
    );
    std::process::exit(2);
}
