// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025 LightFoxLauncher contributors
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! LightFox — a tiny, zero-dependency Minecraft launcher.
//!
//! This library crate holds everything the front-ends share; the
//! [`LFL`](../LFL/index.html) CLI and the `LFL-gui` windowed launcher are thin
//! shells over it.
//!
//! # Platforms
//!
//! Both front-ends target Windows, and `LFL-gui` is native Win32 — there is no
//! portable window toolkit here and no crate to supply one. The **library**,
//! however, builds and runs on any platform Rust supports:
//!
//! * Windows uses WinHTTP for transfers and `GetAdaptersAddresses` for host
//!   detection, so HTTPS, proxies and IPv6 hosting all work.
//! * Everywhere else `net` falls back to a plain HTTP/1.1 client over
//!   `std::net::TcpStream` (no TLS without a crate, so `https://` reports a
//!   clear error), `firewall` and `hostnet` become honest no-ops, and the CLI is
//!   fully usable against `http://` mirrors.
//!
//! The platform seams are `net.rs`, `firewall.rs` and `hostnet.rs`; each keeps a
//! `#[cfg(windows)]` implementation beside a documented fallback rather than a
//! `todo!()`.
//!
//! Module APIs are frozen in `docs/CONTRACT.md`.
//!
//! The modules expose a small, deliberately complete API (e.g. `json::escape`,
//! `hash::sha1_hex`, `inflate::inflate_zlib`) so they stay reusable and
//! individually testable; some of those helpers are exercised by the unit tests
//! rather than by the current call graph. That is why dead-code analysis is
//! switched off crate-wide instead of decorating each helper — the alternative
//! would be 16 scattered `#[allow]`s that obscure the code without removing a
//! single byte from the binaries.
#![allow(dead_code)]

pub mod assets;
pub mod deps;
pub mod firewall;
pub mod hash;
pub mod hostnet;
pub mod hosting;
pub mod inflate;
pub mod java;
pub mod javafind;
pub mod json;
pub mod libraries;
pub mod loaders;
pub mod lock;
pub mod modrinth;
pub mod natpmp;
pub mod net;
pub mod notify;
pub mod profiles;
pub mod room;
pub mod settings;
pub mod servermod;
pub mod util;
pub mod version;
pub mod versionmgr;
pub mod zip;
pub mod upnp;

pub mod launcher;

/// Launcher version, from `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Product name: the launcher is LightFox, and the label the user sees for it
/// is **LightFoxLauncher**.
pub const NAME: &str = "LightFoxLauncher";

/// Short brand string used in JVM arguments.
pub const BRAND: &str = "LFL";

/// Full window title: `LightFoxLauncher 0.2.0`.
pub const TITLE: &str = "LightFoxLauncher";
