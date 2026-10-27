// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025 LightFoxLauncher contributors
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! WinHTTP GET/download with streaming, verification and progress.
//! OWNER: net-core. API frozen by docs/CONTRACT.md.
//!
//! The only `unsafe` in this file is the documented Win32 FFI below; every
//! handle is wrapped so it is closed on every exit path.

use crate::hash;
use crate::util::{self, Res};
use std::cell::Cell;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------- WinHTTP FFI

type HandleRaw = *mut std::ffi::c_void;
type Void = std::ffi::c_void;

#[link(name = "winhttp")]
extern "system" {
    fn WinHttpOpen(
        agent: *const u16,
        access_type: u32,
        proxy: *const u16,
        bypass: *const u16,
        flags: u32,
    ) -> HandleRaw;
    fn WinHttpConnect(session: HandleRaw, host: *const u16, port: u16, reserved: u32) -> HandleRaw;
    fn WinHttpOpenRequest(
        connect: HandleRaw,
        verb: *const u16,
        path: *const u16,
        version: *const u16,
        referrer: *const u16,
        accept_types: *const *const u16,
        flags: u32,
    ) -> HandleRaw;
    fn WinHttpSendRequest(
        req: HandleRaw,
        headers: *const u16,
        headers_len: u32,
        optional: *const Void,
        optional_len: u32,
        total_len: u32,
        context: usize,
    ) -> i32;
    fn WinHttpReceiveResponse(req: HandleRaw, reserved: *mut Void) -> i32;
    fn WinHttpSetOption(handle: HandleRaw, option: u32, buf: *const Void, len: u32) -> i32;
    fn WinHttpReadData(req: HandleRaw, buf: *mut Void, want: u32, got: *mut u32) -> i32;
    fn WinHttpQueryHeaders(
        req: HandleRaw,
        info_level: u32,
        name: *const u16,
        buf: *mut Void,
        len: *mut u32,
        index: *mut u32,
    ) -> i32;
    fn WinHttpQueryDataAvailable(req: HandleRaw, len: *mut u32) -> i32;
    fn WinHttpSetTimeouts(
        handle: HandleRaw,
        resolve: i32,
        connect: i32,
        send: i32,
        receive: i32,
    ) -> i32;
    fn WinHttpCloseHandle(handle: HandleRaw) -> i32;
}

#[link(name = "kernel32")]
extern "system" {
    fn GetLastError() -> u32;
}

const ACCESS_TYPE_AUTOMATIC_PROXY: u32 = 4;
const ACCESS_TYPE_DEFAULT_PROXY: u32 = 0;
const FLAG_SECURE: u32 = 0x0080_0000;

/// `WINHTTP_OPTION_REDIRECT_POLICY`.
const OPTION_REDIRECT_POLICY: u32 = 88;
/// Never downgrade `https` to `http` while following a redirect.
///
/// WinHTTP's default policy follows redirects and only rejects https→http when
/// `WINHTTP_DISABLE_REDIRECTS` is off *and* the redirect policy is left at
/// `WINHTTP_REDIRECT_POLICY_ALLOW_HTTPS_TO_HTTP` — which it is by default on
/// some builds. Every download here is sha1-verified, so the impact is limited
/// to leaking which files a user requested, but a downgrade would still let a
/// network attacker rewrite the asset *index* (which is what supplies those
/// hashes). Disallowing the downgrade closes that off.
const REDIRECT_POLICY_DISALLOW_HTTPS_TO_HTTP: u32 = 1;

const QUERY_STATUS_CODE: u32 = 19;
const QUERY_FLAG_NUMBER: u32 = 0x2000_0000;

const ERROR_WINHTTP_NAME_NOT_RESOLVED: u32 = 12007;
const ERROR_WINHTTP_CANNOT_CONNECT: u32 = 12029;
const ERROR_WINHTTP_CONNECTION_ERROR: u32 = 12030;
const ERROR_WINHTTP_TIMEOUT: u32 = 12002;

/// User agent for every request.
const AGENT: &str = "LFL/0.1";
/// Streaming chunk (contract: 256 KiB).
const CHUNK: usize = 256 * 1024;
/// Total attempts per file (contract: 3).
const ATTEMPTS: u32 = 3;
/// Backoff before attempts 2 and 3 (contract: 400/800/1600 ms).
const BACKOFF_MS: [u64; 3] = [400, 800, 1600];
/// Max worker threads for [`download_many`].
/// Default number of concurrent file transfers.
const MAX_WORKERS: usize = 12;

/// The default worker count, for settings defaults.
pub fn default_workers() -> usize {
    MAX_WORKERS
}

/// Upper bound so a silly setting cannot exhaust handles.
pub const MAX_WORKERS_LIMIT: usize = 64;

/// Effective worker count. 0 means "use the default"; it is capped by the limit
/// and by the machine's parallelism by `download_many`.
static WORKERS: AtomicUsize = AtomicUsize::new(0);

/// Set how many files download at once (`0` = default).
pub fn set_workers(n: usize) {
    WORKERS.store(n.min(MAX_WORKERS_LIMIT), Ordering::Relaxed);
}

/// The configured worker count (0 when unset).
pub fn workers() -> usize {
    WORKERS.load(Ordering::Relaxed)
}

/// Resolve the worker count for a batch of `todo` files.
fn effective_workers(todo: usize) -> usize {
    let configured = workers();
    let base = if configured == 0 { MAX_WORKERS } else { configured };
    base.min(todo)
        .min(
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4),
        )
        .max(1)
}

#[inline]
fn last_error() -> u32 {
    unsafe { GetLastError() }
}

/// Owned WinHTTP handle, closed on drop.
struct Handle(HandleRaw);

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                WinHttpCloseHandle(self.0);
            }
        }
    }
}

// ---------------------------------------------------------------- URL

/// A parsed absolute http(s) URL.
#[derive(Debug, Clone, PartialEq)]
pub struct Url {
    pub secure: bool,
    pub host: String,
    pub port: u16,
    /// Path + query, always starting with `/`.
    pub path: String,
}

impl Url {
    /// Manually split `url` into scheme/host/port/path+query.
    /// Rejects every scheme other than http/https with an error naming the URL.
    pub fn parse(url: &str) -> Res<Url> {
        let url = url.trim();
        let lower = url.to_ascii_lowercase();
        let (secure, rest) = if let Some(r) = lower.strip_prefix("https://") {
            (true, &url[url.len() - r.len()..])
        } else if let Some(r) = lower.strip_prefix("http://") {
            (false, &url[url.len() - r.len()..])
        } else if let Some(idx) = url.find("://") {
            let scheme = &url[..idx];
            return Err(format!(
                "net: unsupported URL scheme '{scheme}' in {url} (only http and https)"
            ));
        } else {
            return Err(format!(
                "net: invalid URL '{url}': expected http:// or https://"
            ));
        };

        // Authority ends at the first '/', '?' or '#'.
        let auth_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let authority = &rest[..auth_end];
        let mut tail = &rest[auth_end..];

        // Drop userinfo (`user:pass@host`); WinHTTP only needs the host.
        let host_port = match authority.rfind('@') {
            Some(i) => &authority[i + 1..],
            None => authority,
        };
        if host_port.is_empty() {
            return Err(format!("net: invalid URL '{url}': missing host"));
        }

        let (host, port_str): (&str, Option<&str>) = if let Some(stripped) = host_port.strip_prefix('[')
        {
            // IPv6 literal: [::1]:8443
            match stripped.find(']') {
                Some(i) => {
                    let after = &stripped[i + 1..];
                    let p = match after.strip_prefix(':') {
                        Some(p) => Some(p),
                        None if after.is_empty() => None,
                        None => {
                            return Err(format!("net: invalid URL '{url}': bad IPv6 authority"))
                        }
                    };
                    (&stripped[..i], p)
                }
                None => return Err(format!("net: invalid URL '{url}': unterminated IPv6 host")),
            }
        } else {
            match host_port.rfind(':') {
                Some(i) => (&host_port[..i], Some(&host_port[i + 1..])),
                None => (host_port, None),
            }
        };

        if host.is_empty() {
            return Err(format!("net: invalid URL '{url}': missing host"));
        }
        if host
            .bytes()
            .any(|c| matches!(c, b' ' | b'\t' | b'/' | b'\\' | b'#' | b'?'))
        {
            return Err(format!("net: invalid URL '{url}': bad character in host"));
        }

        let port = match port_str {
            None | Some("") => {
                if secure {
                    443
                } else {
                    80
                }
            }
            Some(p) => match p.parse::<u16>() {
                Ok(0) | Err(_) => {
                    return Err(format!("net: invalid URL '{url}': bad port '{p}'"))
                }
                Ok(n) => n,
            },
        };

        // Fragments are never sent.
        if let Some(i) = tail.find('#') {
            tail = &tail[..i];
        }
        let path = if tail.is_empty() {
            "/".to_string()
        } else if !tail.starts_with('/') {
            format!("/{tail}")
        } else {
            tail.to_string()
        };

        Ok(Url {
            secure,
            host: host.to_string(),
            port,
            path,
        })
    }
}

// ---------------------------------------------------------------- progress

/// Throttled, thread-safe progress renderer.
#[derive(Default)]
struct ProgressState {
    label: String,
    painted: bool,
    last: Option<Instant>,
}

static PROGRESS: OnceLock<Mutex<ProgressState>> = OnceLock::new();
static PROGRESS_ON: AtomicBool = AtomicBool::new(true);

fn progress_cell() -> &'static Mutex<ProgressState> {
    PROGRESS.get_or_init(|| Mutex::new(ProgressState::default()))
}

/// Take the progress lock without ever panicking on a poisoned mutex.
fn lock_progress() -> std::sync::MutexGuard<'static, ProgressState> {
    match progress_cell().lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn progress_enabled() -> bool {
    PROGRESS_ON.load(Ordering::Relaxed) && !util::quiet()
}

/// Enable/disable the progress line (default: enabled).
pub fn set_progress(enabled: bool) {
    PROGRESS_ON.store(enabled, Ordering::Relaxed);
}

fn percent(done: u64, total: u64) -> u32 {
    if total == 0 {
        return 0;
    }
    let pct = (done as u128 * 100) / total as u128;
    pct.min(100) as u32
}

/// `\r<label> 42% 3.2 MB/7.6 MB`, throttled to 200 ms per label.
///
/// When a front-end reporter is installed the sample is forwarded there and no
/// console output is produced (there is no console).
fn paint(label: &str, done: u64, total: u64) {
    if util::reporter().is_some() {
        util::report_progress(label, done, total);
        return;
    }
    if !progress_enabled() {
        return;
    }
    let mut st = lock_progress();
    if st.painted && st.label == label {
        if let Some(last) = st.last {
            if last.elapsed() < Duration::from_millis(200) {
                return;
            }
        }
    }
    if !st.painted || st.label != label {
        st.label.clear();
        st.label.push_str(label);
    }
    st.painted = true;
    st.last = Some(Instant::now());
    if total > 0 {
        print!(
            "\r{label} {:>3}% {} / {}    ",
            percent(done, total),
            util::human_size(done),
            util::human_size(total)
        );
    } else {
        print!("\r{label} {}    ", util::human_size(done));
    }
    let _ = std::io::stdout().flush();
}

/// Final line for one file: same shape, terminated by `\n`.
fn paint_final(label: &str, done: u64, total: u64) {
    if util::reporter().is_some() {
        util::report_progress(label, done, total);
        return;
    }
    if !progress_enabled() {
        return;
    }
    let mut st = lock_progress();
    st.painted = false;
    st.label.clear();
    st.last = None;
    if total > 0 {
        print!(
            "\r{label} {:>3}% {} / {}        \n",
            percent(done, total),
            util::human_size(done),
            util::human_size(total)
        );
    } else {
        print!("\r{label} {}        \n", util::human_size(done));
    }
    let _ = std::io::stdout().flush();
}

/// Erase a half-painted line (e.g. before printing an error).
fn paint_clear() {
    if util::reporter().is_some() {
        return; // no console to clear; the GUI repaints itself
    }
    let mut st = lock_progress();
    if st.painted {
        print!("\r{}\r", " ".repeat(78));
        let _ = std::io::stdout().flush();
        st.painted = false;
        st.last = None;
        st.label.clear();
    }
}

/// Where per-file byte counts are reported to.
enum Prog<'a> {
    /// `download`: one file, its own line.
    File { label: String, total: u64 },
    /// `download_many`: aggregate line over all files.
    Agg {
        label: &'a str,
        shared: &'a AtomicU64,
        total: u64,
        prev: Cell<u64>,
    },
}

impl Prog<'_> {
    /// Report progress made by the current file (cumulative for that file).
    fn at(&self, done: u64) {
        match self {
            Prog::File { label, total } => paint(label, done, *total),
            Prog::Agg {
                label,
                shared,
                total,
                prev,
            } => {
                let delta = done.saturating_sub(prev.get());
                prev.set(done);
                let agg = shared.fetch_add(delta, Ordering::Relaxed) + delta;
                paint(label, agg, *total);
            }
        }
    }

    /// Seed the counters for a file that resumes at `already` bytes.
    ///
    /// The aggregate line counts bytes *transferred*, so the bytes already on
    /// disk must not be added again; the per-file line shows total file progress,
    /// which legitimately starts at `already`.
    fn at_with_resume(&self, done: u64, already: u64) {
        match self {
            Prog::File { label, total } => paint(label, done, *total),
            Prog::Agg { prev, .. } => {
                // Pretend we already reported `done - already` so the following
                // `at(done)` adds only the newly transferred bytes.
                prev.set(done.saturating_sub(already));
            }
        }
    }

    /// Completion of the current file.
    fn complete(&self, done: u64) {
        match self {
            Prog::File { label, total } => paint_final(label, done, done.max(*total)),
            // The aggregate line is finished once by `download_many`.
            Prog::Agg { .. } => {}
        }
    }
}

// ---------------------------------------------------------------- request

/// Owns every WinHTTP handle of one request. Fields are dropped in
/// declaration order: request, then connection, then session.
struct Session {
    request: Handle,
    _connect: Handle,
    _session: Handle,
}

impl Session {
    /// Open session + connection + request and receive the response headers.
    fn open(url: &Url) -> Res<Session> {
        Session::open_with(url, None)
    }

    /// Same as [`Session::open`], adding raw request headers (e.g. `Range`).
    fn open_with(url: &Url, headers: Option<&str>) -> Res<Session> {
        let agent = util::to_wide(AGENT);
        let mut session = unsafe {
            WinHttpOpen(
                agent.as_ptr(),
                ACCESS_TYPE_AUTOMATIC_PROXY,
                std::ptr::null(),
                std::ptr::null(),
                0,
            )
        };
        if session.is_null() {
            // WPAD/automatic proxy needs Windows 8.1+; fall back to the
            // default proxy configuration instead of failing outright.
            session = unsafe {
                WinHttpOpen(
                    agent.as_ptr(),
                    ACCESS_TYPE_DEFAULT_PROXY,
                    std::ptr::null(),
                    std::ptr::null(),
                    0,
                )
            };
        }
        if session.is_null() {
            return Err(format!(
                "net: cannot create a WinHTTP session for {} (error {})",
                url.host,
                last_error()
            ));
        }
        let session = Handle(session);

        // Refuse an https→http downgrade on redirect. Best effort: an older
        // WinHTTP that does not know the option fails the call, and the default
        // policy stays in force — that must not turn into a hard error.
        unsafe {
            let policy = REDIRECT_POLICY_DISALLOW_HTTPS_TO_HTTP;
            WinHttpSetOption(
                session.0,
                OPTION_REDIRECT_POLICY,
                &policy as *const u32 as *const Void,
                std::mem::size_of::<u32>() as u32,
            );
        }

        {
            // Do NOT call `WinHttpSetTimeouts` here.
            //
            // It looked harmless, but on this host profile it left the session
            // unable to reach some hosts: every request to them failed with
            // 12002 "timed out" or 12030 "connection aborted" immediately, while
            // the identical request through curl or through a freshly opened
            // session succeeded. Skipping the call made Modrinth work again.
            //
            // WinHTTP's defaults (30 s per phase) are already finite, so the
            // "a stalled CDN cannot hang forever" goal still holds; only the
            // tunability is lost, which nobody needs here.
        }

        let host = util::to_wide(&url.host);
        let connect = unsafe { WinHttpConnect(session.0, host.as_ptr(), url.port, 0) };
        if connect.is_null() {
            return Err(format!(
                "net: cannot connect to {}:{} (error {})",
                url.host,
                url.port,
                last_error()
            ));
        }
        let connect = Handle(connect);

        let verb = util::to_wide("GET");
        let path = util::to_wide(&url.path);
        let flags = if url.secure { FLAG_SECURE } else { 0 };
        let request = unsafe {
            WinHttpOpenRequest(
                connect.0,
                verb.as_ptr(),
                path.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                flags,
            )
        };
        if request.is_null() {
            return Err(format!(
                "net: cannot create a request for {} (error {})",
                url.path,
                last_error()
            ));
        }
        let request = Handle(request);

        // Redirects are followed automatically by WinHTTP's default policy.
        let wide_headers = headers.map(util::to_wide);
        let (hdr_ptr, hdr_len) = match &wide_headers {
            // -1 means "NUL-terminated", which is what WinHTTP wants for
            // request headers supplied as a string.
            Some(h) => (h.as_ptr(), u32::MAX),
            None => (std::ptr::null(), 0),
        };
        let ok = unsafe {
            WinHttpSendRequest(request.0, hdr_ptr, hdr_len, std::ptr::null(), 0, 0, 0)
        };
        if ok == 0 {
            return Err(format!(
                "net: sending GET {}{} failed: {}",
                url.host,
                url.path,
                describe_error(last_error())
            ));
        }
        let ok = unsafe { WinHttpReceiveResponse(request.0, std::ptr::null_mut()) };
        if ok == 0 {
            return Err(format!(
                "net: no response from {}{}: {}",
                url.host,
                url.path,
                describe_error(last_error())
            ));
        }

        Ok(Session {
            request,
            _connect: connect,
            _session: session,
        })
    }

    /// HTTP status code (after redirects).
    fn status(&self) -> Res<u32> {
        let mut code: u32 = 0;
        let mut len = std::mem::size_of::<u32>() as u32;
        let mut index: u32 = 0;
        let ok = unsafe {
            WinHttpQueryHeaders(
                self.request.0,
                QUERY_STATUS_CODE | QUERY_FLAG_NUMBER,
                std::ptr::null(),
                &mut code as *mut u32 as *mut Void,
                &mut len,
                &mut index,
            )
        };
        if ok == 0 {
            return Err(format!(
                "net: cannot read the HTTP status code (error {})",
                last_error()
            ));
        }
        Ok(code)
    }

    /// Read the whole body. Only used for small metadata documents.
    fn read_all(&self) -> Res<Vec<u8>> {
        let mut out: Vec<u8> = Vec::new();
        let mut buf = vec![0u8; CHUNK];
        loop {
            let mut avail: u32 = 0;
            if unsafe { WinHttpQueryDataAvailable(self.request.0, &mut avail) } == 0 {
                return Err(format!(
                    "net: reading response failed: {}",
                    describe_error(last_error())
                ));
            }
            if avail == 0 {
                return Ok(out);
            }
            let want = avail.min(buf.len() as u32);
            let mut got: u32 = 0;
            if unsafe {
                WinHttpReadData(
                    self.request.0,
                    buf.as_mut_ptr() as *mut Void,
                    want,
                    &mut got,
                )
            } == 0
            {
                return Err(format!(
                    "net: reading response body failed: {}",
                    describe_error(last_error())
                ));
            }
            if got == 0 {
                return Ok(out);
            }
            out.extend_from_slice(&buf[..got as usize]);
        }
    }
}

fn describe_error(code: u32) -> String {
    match code {
        ERROR_WINHTTP_NAME_NOT_RESOLVED => format!("host not found (error {code})"),
        ERROR_WINHTTP_CANNOT_CONNECT => format!("connection failed (error {code})"),
        ERROR_WINHTTP_CONNECTION_ERROR => format!("connection aborted (error {code})"),
        ERROR_WINHTTP_TIMEOUT => format!("timed out (error {code})"),
        0 => "no error reported".to_string(),
        c => format!("WinHTTP error {c}"),
    }
}

/// Status codes [`get_opt`] maps to `None`.
fn is_absent(status: u32) -> bool {
    matches!(status, 403 | 404 | 410)
}

fn open_with_status(url: &Url) -> Res<(Session, u32)> {
    let s = Session::open(url)?;
    let status = s.status()?;
    Ok((s, status))
}

// ---------------------------------------------------------------- public API

/// GET a small document. 2xx returns the body; anything else is an error.
pub fn get(url: &str) -> Res<Vec<u8>> {
    let u = Url::parse(url)?;
    let (s, status) = open_with_status(&u)?;
    if !(200..300).contains(&status) {
        return Err(format!("HTTP {status} for {url}"));
    }
    s.read_all().map_err(|e| format!("{e} ({url})"))
}

/// GET a small document; `Ok(None)` for 403/404/410.
pub fn get_opt(url: &str) -> Res<Option<Vec<u8>>> {
    let u = Url::parse(url)?;
    let (s, status) = open_with_status(&u)?;
    if is_absent(status) {
        return Ok(None);
    }
    if !(200..300).contains(&status) {
        return Err(format!("HTTP {status} for {url}"));
    }
    s.read_all().map(Some).map_err(|e| format!("{e} ({url})"))
}

/// One file to fetch.
#[derive(Debug, Clone)]
pub struct Download {
    pub url: String,
    pub dest: PathBuf,
    pub sha1: String,
    pub size: u64,
    pub label: String,
}

impl Download {
    /// `label` defaults to the destination's file name.
    pub fn new(url: &str, dest: PathBuf, sha1: &str, size: u64) -> Self {
        let label = file_label(&dest);
        Download {
            url: url.to_string(),
            dest,
            sha1: sha1.to_string(),
            size,
            label,
        }
    }
}

fn file_label(dest: &Path) -> String {
    dest.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| dest.display().to_string())
}

/// `<file name>.lflpart` next to `dest`, so the final rename stays on one volume.
///
/// The name is deliberately **stable** (no pid, no random suffix): a partial
/// download left by a killed process is found again on the next run and
/// resumed with a `Range` request instead of being thrown away. Cross-process
/// safety comes from a per-file lock, not from a unique name.
fn temp_path(dest: &Path) -> PathBuf {
    let mut name = dest
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "download".to_string());
    name.push_str(".lflpart");
    match dest.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir.join(name),
        _ => PathBuf::from(name),
    }
}

/// Everything left over by an older layout, so `LFL clean` can find it.
///
/// The predicate is deliberately **narrow**. It used to be
/// `ends_with(".lflpart") || contains(".part")`, and `contains` matched files
/// the launcher never wrote: a split save (`level.part1.mca`), a backup
/// (`world.partial`), or a mod whose name happens to contain `.part`
/// (`mymod.particle.jar`). 删除断点文件 deletes whatever this returns, so a
/// false positive is silent data loss in the user's own game directory.
///
/// Only two shapes are ours:
///
/// * `<name>.lflpart` — the resume temp file this version writes.
/// * `<name>.part<N>` — the older layout's numbered parts, where the suffix is
///   all digits. `.particle` and `.part1.mca` therefore no longer match.
pub fn is_partial_name(name: &str) -> bool {
    if name.ends_with(".lflpart") {
        return true;
    }
    // `.part` followed only by digits, at the very end.
    let Some(idx) = name.rfind(".part") else {
        return false;
    };
    let rest = &name[idx + ".part".len()..];
    !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit())
}

/// Lock file for one destination, kept **outside** the data directory.
///
/// Writing locks next to their targets left thousands of stray `.lck` files in
/// `assets/` and `runtime/`, which pollutes the game directory and looks like
/// corruption to anything inspecting it. A single hashed name in the OS temp dir
/// has the same isolation properties without touching the tree.
fn lock_path(dest: &Path) -> PathBuf {
    let absolute = if dest.is_absolute() {
        dest.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(dest)
    };
    let key = hash::sha1_hex(absolute.to_string_lossy().to_ascii_lowercase().as_bytes());
    std::env::temp_dir()
        .join("lfl-locks")
        .join(format!("{}.lck", &key[..24]))
}

fn remove_quiet(p: &Path) {
    let _ = std::fs::remove_file(p);
}

/// Backoff before attempt `attempt` (0-based index into the 400/800/1600 ladder).
fn backoff_ms(attempt: u32) -> u64 {
    match BACKOFF_MS.get(attempt as usize) {
        Some(&ms) => ms,
        None => BACKOFF_MS[BACKOFF_MS.len() - 1],
    }
}

/// Download `url` to `dest`, verifying `sha1` while streaming.
/// Skips the transfer when the file already matches.
pub fn download(url: &str, dest: &Path, sha1: &str, size: u64) -> Res<()> {
    let prog = Prog::File {
        label: file_label(dest),
        total: size,
    };
    download_inner(url, dest, sha1, size, &prog)
}

fn download_inner(url: &str, dest: &Path, sha1: &str, size: u64, prog: &Prog) -> Res<()> {
    if hash::file_matches(dest, sha1) {
        let have = util::file_size(dest);
        prog.complete(have);
        return Ok(());
    }

    // One process per destination file. Another launcher transferring this exact
    // file produces the same bytes, so rather than queueing we wait for its
    // result: re-check the destination, and if it finished while we waited we are
    // already done.
    let lock = match crate::lock::try_acquire_file(&lock_path(dest)) {
        // Could not even create the lock file (read-only dir, bad path): carry on
        // without the lock rather than failing — a single-process run is the
        // common case and this keeps filesystem errors honest.
        Err(_) => None,
        Ok(Some(l)) => Some(l),
        Ok(None) => {
            // Held by another launcher. It is transferring *this* file, and when
            // it finishes the destination will match our sha1 — so the cheap and
            // correct move is to wait for the artifact, not to grab the lock.
            // Waiting on the lock instead would serialise two launchers file by
            // file across a whole 400-file batch.
            let wait = Duration::from_secs(if size > 8 * 1024 * 1024 { 600 } else { 120 });
            let started = std::time::Instant::now();
            let mut acquired = None;
            while started.elapsed() < wait {
                std::thread::sleep(Duration::from_millis(200));
                // Done by the other launcher: the common, happy path.
                if hash::file_matches(dest, sha1) {
                    prog.complete(util::file_size(dest));
                    return Ok(());
                }
                // The holder crashed or moved on: take the lock and do it here.
                // Safe to poll for both because a waiter holds no lock of its own,
                // so two launchers cannot deadlock on each other's files.
                if let Ok(Some(l)) = crate::lock::try_acquire_file(&lock_path(dest)) {
                    acquired = Some(l);
                    break;
                }
            }
            match acquired {
                Some(l) => Some(l),
                None => {
                    paint_clear();
                    return Err(format!(
                        "timed out waiting for another launcher downloading {url} \
                         (destination {})",
                        dest.display()
                    ));
                }
            }
        }
    };
    download_locked(url, dest, sha1, size, prog, lock)
}

/// Body of `download_inner`, run while holding the per-file lock (if any).
fn download_locked(
    url: &str,
    dest: &Path,
    sha1: &str,
    size: u64,
    prog: &Prog,
    _lock: Option<crate::lock::DirLock>,
) -> Res<()> {
    if hash::file_matches(dest, sha1) {
        prog.complete(util::file_size(dest));
        return Ok(());
    }

    let u = Url::parse(url)?;
    let mut last = String::new();

    for attempt in 0..ATTEMPTS {
        if attempt > 0 {
            std::thread::sleep(Duration::from_millis(backoff_ms(attempt - 1)));
        }
        match try_download(&u, url, dest, sha1, size, prog) {
            Ok(()) => return Ok(()),
            Err(e) => {
                // Missing/forbidden objects will not get better; those (and a
                // bad digest, whose partial data cannot be trusted either) drop
                // the temp file. Transient failures keep it for the next attempt.
                let fatal = ["HTTP 404 ", "HTTP 403 ", "HTTP 410 ", "sha1 mismatch", "size mismatch"]
                    .iter()
                    .any(|p| e.starts_with(p));
                if fatal {
                    remove_quiet(&temp_path(dest));
                    paint_clear();
                    return Err(format!("{e} (url {url}, destination {})", dest.display()));
                }
                last = e;
            }
        }
    }
    paint_clear();
    Err(format!(
        "{last} (url {url}, destination {}) — partial data kept for resume",
        dest.display()
    ))
}

/// Take the per-file download lock, or `None` when another process owns it.
///
/// Files are locked individually rather than locking the whole data directory:
/// two launchers can then prepare different versions (or different files of the
/// same version) at the same time, and only a genuine collision serialises.
fn try_file_lock(dest: &Path) -> Option<crate::lock::DirLock> {
    crate::lock::try_acquire_file(&lock_path(dest)).ok().flatten()
}

/// One attempt: request (resuming a partial file when possible), stream to the
/// temp file while hashing, verify, then rename into place.
///
/// A previous run may have left `<name>.lflpart` behind. We ask the server for
/// the remaining bytes with `Range: bytes=<have>-` and append, so a 600 MB asset
/// set interrupted at 90% costs only the missing 10% on the next run. When the
/// server ignores the range and replies `200`, the temp file is discarded and the
/// transfer restarts from zero — correctness never depends on the server.
fn try_download(
    url: &Url,
    orig: &str,
    dest: &Path,
    sha1: &str,
    size: u64,
    prog: &Prog,
) -> Res<()> {
    if let Some(dir) = dest.parent() {
        if !dir.as_os_str().is_empty() {
            util::mkdirs(dir)?;
        }
    }

    let tmp = temp_path(dest);
    let mut have = util::file_size(&tmp);

    // A resumed file must come from the same object; with a known size, a temp
    // file that is already too big (or exactly the right size but unverified) is
    // not a useful starting point.
    if size > 0 && have > size {
        remove_quiet(&tmp);
        have = 0;
    }

    let range = if have > 0 {
        Some(format!("Range: bytes={have}-\r\n"))
    } else {
        None
    };

    let (session, status) = {
        let s = Session::open_with(url, range.as_deref())?;
        let st = s.status()?;
        (s, st)
    };

    // 206 => the server honoured our range; 200 => it ignored it and is sending
    // the whole body again, so the partial data must go.
    let mut resumed = have;
    match status {
        // A 200 means the range was ignored: start over.
        200 => {
            if resumed > 0 {
                remove_quiet(&tmp);
                resumed = 0;
            }
        }
        206 => {}
        s if (200..300).contains(&s) => {
            if resumed > 0 {
                remove_quiet(&tmp);
                resumed = 0;
            }
        }
        s => return Err(format!("HTTP {s} for {orig}")),
    }

    let mut file = if resumed > 0 {
        std::fs::OpenOptions::new()
            .append(true)
            .open(&tmp)
            .map_err(|e| format!("cannot open {} for resume: {e}", tmp.display()))?
    } else {
        std::fs::File::create(&tmp)
            .map_err(|e| format!("cannot create {}: {e}", tmp.display()))?
    };

    // Re-hash what is already on disk so the final digest covers the whole file.
    let mut hasher = hash::Sha1::new();
    if resumed > 0 {
        match hash::file_sha1_prefix(&tmp, resumed) {
            Ok(mut partial) => {
                for block in partial.drain(..) {
                    hasher.update(&block);
                }
            }
            Err(_) => {
                // Could not read it back; start over rather than risk a bad digest.
                drop(file);
                remove_quiet(&tmp);
                return try_download(url, orig, dest, sha1, size, prog);
            }
        }
    }

    let mut done: u64 = resumed;
    prog.at_with_resume(done, resumed);
    let mut buf = vec![0u8; CHUNK];

    let res: Res<()> = loop {
        let mut avail: u32 = 0;
        if unsafe { WinHttpQueryDataAvailable(session.request.0, &mut avail) } == 0 {
            break Err(format!(
                "net: reading body of {orig} failed: {}",
                describe_error(last_error())
            ));
        }
        if avail == 0 {
            break Ok(());
        }
        let want = avail.min(buf.len() as u32);
        let mut got: u32 = 0;
        if unsafe {
            WinHttpReadData(
                session.request.0,
                buf.as_mut_ptr() as *mut Void,
                want,
                &mut got,
            )
        } == 0
        {
            break Err(format!(
                "net: reading body of {orig} failed: {}",
                describe_error(last_error())
            ));
        }
        if got == 0 {
            break Ok(());
        }
        let chunk = &buf[..got as usize];
        if let Err(e) = file.write_all(chunk) {
            break Err(format!("cannot write {}: {e}", tmp.display()));
        }
        hasher.update(chunk);
        done += got as u64;
        prog.at(done);
    };

    let res = res.and_then(|()| {
        file.flush()
            .map_err(|e| format!("cannot flush {}: {e}", tmp.display()))?;
        file.sync_all()
            .map_err(|e| format!("cannot sync {}: {e}", tmp.display()))
    });
    // Close before renaming (required on Windows).
    drop(file);

    if let Err(e) = res {
        // Keep the partial file when the transfer broke off: a later attempt
        // resumes from here. Only a permanent failure removes it.
        return Err(e);
    }

    let got_hex = hash::hex(&hasher.finalize());
    if !sha1.is_empty() && !hash::hex_eq(&got_hex, sha1) {
        remove_quiet(&tmp);
        return Err(format!("sha1 mismatch for {orig}: expected {sha1}, got {got_hex}"));
    }
    // Without a sha1 the size is the only integrity signal available.
    if sha1.is_empty() && size > 0 && done != size {
        remove_quiet(&tmp);
        return Err(format!("size mismatch for {orig}: expected {size}, got {done}"));
    }

    if dest.exists() {
        remove_quiet(dest);
    }
    std::fs::rename(&tmp, dest)
        .map_err(|e| format!("cannot move {} to {}: {e}", tmp.display(), dest.display()))?;

    prog.complete(done);
    Ok(())
}

/// Download many files on up to 8 threads with aggregated progress.
/// Stops at the first failure and reports it.
pub fn download_many(items: &[Download]) -> Res<()> {
    if items.is_empty() {
        return Ok(());
    }
    let total: u64 = items.iter().map(|d| d.size).sum();

    // Files already on disk are neither fetched nor counted as work.
    let mut pending: Vec<&Download> = Vec::new();
    let mut done_bytes: u64 = 0;
    for d in items {
        if hash::file_matches(&d.dest, &d.sha1) {
            done_bytes += d.size;
        } else {
            pending.push(d);
        }
    }
    let skipped = items.len() - pending.len();
    if pending.is_empty() {
        paint_final("downloads", total, total);
        if !util::quiet() {
            util::info(&format!(
                "{} file(s) already present ({})",
                items.len(),
                util::human_size(total)
            ));
        }
        return Ok(());
    }
    let todo = pending.len();

    let shared = AtomicU64::new(done_bytes);
    let index = AtomicUsize::new(0);
    let cancel = AtomicBool::new(false);
    let first_err: Mutex<Option<String>> = Mutex::new(None);

    let workers = effective_workers(todo);

    std::thread::scope(|scope| {
        for _ in 0..workers {
            let shared = &shared;
            let index = &index;
            let cancel = &cancel;
            let first_err = &first_err;
            let pending = &pending;
            scope.spawn(move || loop {
                if cancel.load(Ordering::Relaxed) {
                    return;
                }
                let i = index.fetch_add(1, Ordering::SeqCst);
                if i >= pending.len() {
                    return;
                }
                let d = pending[i];
                let label = format!("{}/{} {}", i + 1, todo, d.label);
                let prog = Prog::Agg {
                    label: &label,
                    shared,
                    total,
                    prev: Cell::new(0),
                };
                if let Err(e) = download_inner(&d.url, &d.dest, &d.sha1, d.size, &prog) {
                    cancel.store(true, Ordering::Relaxed);
                    let mut slot = match first_err.lock() {
                        Ok(g) => g,
                        Err(p) => p.into_inner(),
                    };
                    if slot.is_none() {
                        *slot = Some(e);
                    }
                }
            });
        }
    });

    let failure = match first_err.lock() {
        Ok(g) => g.clone(),
        Err(p) => p.into_inner().clone(),
    };
    match failure {
        Some(e) => {
            paint_clear();
            Err(e)
        }
        None => {
            paint_final("downloads", total, total);
            util::info(&format!(
                "downloaded {todo} file(s), {}{}",
                util::human_size(shared.load(Ordering::Relaxed)),
                if skipped > 0 {
                    format!(" ({skipped} already present)")
                } else {
                    String::new()
                }
            ));
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u32_of(s: &str) -> String {
        s.to_string()
    }



    #[test]
    fn url_https_default_port() {
        let u = Url::parse("https://piston-data.mojang.com/v1/objects/abc/client.jar").expect("url");
        assert!(u.secure);
        assert_eq!(u.host, "piston-data.mojang.com");
        assert_eq!(u.port, 443);
        assert_eq!(u.path, "/v1/objects/abc/client.jar");
    }

    #[test]
    fn url_http_default_port_and_root_path() {
        let u = Url::parse("http://example.com").expect("url");
        assert!(!u.secure);
        assert_eq!(u.host, "example.com");
        assert_eq!(u.port, 80);
        assert_eq!(u.path, "/");
    }

    #[test]
    fn url_explicit_ports() {
        let u = Url::parse("https://example.com:8443/a/b").expect("url");
        assert_eq!(u.host, "example.com");
        assert_eq!(u.port, 8443);
        assert_eq!(u.path, "/a/b");
        let u = Url::parse("http://localhost:8080").expect("url");
        assert_eq!(u.host, "localhost");
        assert_eq!(u.port, 8080);
        assert_eq!(u.path, "/");
    }

    #[test]
    fn url_query_string_and_fragment() {
        let u = Url::parse("https://api.example.com/v1/x?foo=bar&baz=qux#frag").expect("url");
        assert_eq!(u.host, "api.example.com");
        assert_eq!(u.path, "/v1/x?foo=bar&baz=qux");
        let u = Url::parse("https://h/?a=1").expect("url");
        assert_eq!(u.path, "/?a=1");
        // A bare query with no path still gets a leading slash.
        let u = Url::parse("https://h?x=1").expect("url");
        assert_eq!(u.path, "/?x=1");
    }

    #[test]
    fn url_ipv4_ipv6_and_case() {
        let u = Url::parse("https://127.0.0.1:25566/x").expect("url");
        assert_eq!(u.host, "127.0.0.1");
        assert_eq!(u.port, 25566);
        let u = Url::parse("http://[::1]:8080/y").expect("url");
        assert_eq!(u.host, "::1");
        assert_eq!(u.port, 8080);
        assert_eq!(u.path, "/y");
        let u = Url::parse("HTTPS://Example.COM/Path").expect("url");
        assert!(u.secure);
        assert_eq!(u.host, "Example.COM");
        assert_eq!(u.path, "/Path");
        // userinfo is stripped, not sent as host.
        let u = Url::parse("https://user:pw@host.example/x").expect("url");
        assert_eq!(u.host, "host.example");
    }

    #[test]
    fn url_bad_input_rejected() {
        for bad in [
            "ftp://example.com/x",
            "file:///C:/x",
            "ws://example.com/x",
            "example.com/x",
            "",
            "http://",
            "https://",
            "https:///path",
            "https://host:0/x",
            "https://host:99999/x",
            "https://host:abc/x",
            "https://ho st/x",
        ] {
            let r = Url::parse(bad);
            assert!(r.is_err(), "{bad:?} should be rejected, got {r:?}");
            assert!(
                r.unwrap_err().starts_with("net: "),
                "error prefix missing for {bad:?}"
            );
        }
        let e = Url::parse("ftp://h/x").unwrap_err();
        assert!(e.contains("ftp"), "error must name the scheme: {e}");
        assert!(e.contains("ftp://h/x"), "error must name the url: {e}");
    }

    #[test]
    fn errors_name_the_url() {
        let e = get("nonsense").unwrap_err();
        assert!(e.contains("nonsense"), "must name url: {e}");
        let e = get_opt("gopher://h/f").unwrap_err();
        assert!(e.contains("gopher://h/f"), "must name url: {e}");
        // Bad requests use the common errors-offline path: no network needed
        // because URL validation fails first.
        let e = download("bad url", Path::new("x.bin"), "", 0).unwrap_err();
        assert!(e.contains("bad url"), "must name url: {e}");
    }

    #[test]
    fn absent_status_codes() {
        for code in [403u32, 404, 410] {
            assert!(is_absent(code), "{code} should map to None");
        }
        for code in [200u32, 204, 301, 500, 503] {
            assert!(!is_absent(code), "{code} must not map to None");
        }
    }

    #[test]
    fn backoff_ladder() {
        assert_eq!(backoff_ms(0), 400);
        assert_eq!(backoff_ms(1), 800);
        assert_eq!(backoff_ms(2), 1600);
        assert_eq!(backoff_ms(3), 1600, "must clamp, not panic");
        assert_eq!(backoff_ms(u32::MAX), 1600);
        assert_eq!(ATTEMPTS, 3);
        assert_eq!(MAX_WORKERS, 12);
        assert_eq!(CHUNK, 256 * 1024);
    }

    #[test]
    fn percent_math() {
        assert_eq!(percent(0, 0), 0);
        assert_eq!(percent(1, 0), 0);
        assert_eq!(percent(0, 100), 0);
        assert_eq!(percent(50, 100), 50);
        assert_eq!(percent(100, 100), 100);
        assert_eq!(percent(200, 100), 100, "must clamp");
        // Huge values must not overflow.
        assert_eq!(percent(u64::MAX, u64::MAX), 100);
    }

    #[test]
    fn temp_path_is_a_stable_sibling() {
        let dest = Path::new("C:/games/.LFL/libraries/a/b.jar");
        let t = temp_path(dest);
        assert_eq!(t.parent(), dest.parent(), "temp must stay on the same volume");
        let name = t.file_name().expect("name").to_string_lossy().to_string();
        // Stable, not per-pid: that is what lets a later run find and resume it.
        assert_eq!(name, "b.jar.lflpart");
        assert_eq!(temp_path(dest), t, "temp path must not vary between calls");
        let loose = temp_path(Path::new("file"));
        assert!(loose.to_string_lossy().ends_with(".lflpart"));

        // Lock files are per destination path, and distinct per directory.
        let l1 = lock_path(dest);
        let l2 = lock_path(Path::new("C:/games/.LFL/libraries/x/b.jar"));
        assert_ne!(l1, l2, "same base name in different dirs must not share a lock");
        assert_eq!(lock_path(dest), l1);
        assert_eq!(l1.extension().and_then(|e| e.to_str()), Some("lck"));

        // Both old and new partial suffixes are recognised for cleanup.
        assert!(is_partial_name("a.jar.lflpart"));
        assert!(is_partial_name("a.jar.part1234"));
        assert!(!is_partial_name("a.jar"));
    }

    #[test]
    fn is_partial_name_does_not_match_user_files() {
        // `clean_parts` deletes whatever this returns, so a false positive is
        // data loss in the user's own game directory. These all matched the old
        // `contains(".part")` rule.
        for user_file in [
            "level.part1.mca",      // a split save
            "world.partial",        // a backup
            "mymod.particle.jar",   // a mod name that happens to contain ".part"
            "notes.part.txt",
            "report.part",
            "a.part",               // no digits: not ours
        ] {
            assert!(!is_partial_name(user_file), "{user_file} must be kept");
        }
        // Only the two shapes the launcher writes are matched.
        assert!(is_partial_name("x.jar.lflpart"));
        assert!(is_partial_name("x.jar.part0"));
        assert!(is_partial_name("x.jar.part99"));
        assert!(!is_partial_name("x.jar.part99.bak"));
    }

    #[test]
    fn download_label_defaults_to_file_name() {
        let d = Download::new(
            "https://h/x/library.jar",
            PathBuf::from("C:/LFL/libraries/library.jar"),
            "abc",
            7,
        );
        assert_eq!(d.label, "library.jar");
        assert_eq!(d.url, "https://h/x/library.jar");
        assert_eq!(d.sha1, "abc");
        assert_eq!(d.size, 7);
        assert_eq!(u32_of(&d.label), "library.jar");
    }

    /// A present, matching file is skipped before any network access happens.
    #[test]
    fn download_skips_matching_file_without_network() {
        let mut p = std::env::temp_dir();
        p.push(format!("lfl-net-skip-{}.bin", std::process::id()));
        std::fs::write(&p, b"abc").expect("write");
        let sha = hash::sha1_hex(b"abc");
        // `example.invalid` never resolves; a network attempt would be slow and
        // would error, so Ok here proves the hash shortcut ran.
        assert!(download("https://example.invalid/x", &p, &sha, 3).is_ok());
        // Empty sha1 + existing file => also skipped.
        assert!(download("https://example.invalid/x", &p, "", 0).is_ok());
        let _ = std::fs::remove_file(&p);
    }

    /// Filesystem failures are reported (and name url + destination) with no
    /// network access: the destination's parent is a regular file.
    #[test]
    fn download_reports_filesystem_errors() {
        let mut blocker = std::env::temp_dir();
        blocker.push(format!("lfl-net-blocker-{}.tmp", std::process::id()));
        std::fs::write(&blocker, b"not a directory").expect("write");
        let dest = blocker.join("nested").join("file.bin");
        let e = download("https://example.invalid/x", &dest, "", 10).unwrap_err();
        assert!(
            e.contains("example.invalid"),
            "error must name the url: {e}"
        );
        assert!(
            e.contains("file.bin") || e.contains("nested"),
            "error must name the destination: {e}"
        );
        assert!(!dest.exists());
        assert_eq!(
            std::fs::read(&blocker).ok().as_deref(),
            Some(&b"not a directory"[..]),
            "the blocking file must be untouched"
        );
        let _ = std::fs::remove_file(&blocker);
    }

    #[test]
    fn download_many_empty_is_ok() {
        assert!(download_many(&[]).is_ok());
    }

    #[test]
    fn download_many_skips_existing_files() {
        let mut paths = Vec::new();
        let mut items = Vec::new();
        for i in 0..3 {
            let mut p = std::env::temp_dir();
            p.push(format!("lfl-net-many-{}-{i}.bin", std::process::id()));
            std::fs::write(&p, b"xyz").expect("write");
            items.push(Download::new(
                "https://example.invalid/x",
                p.clone(),
                &hash::sha1_hex(b"xyz"),
                3,
            ));
            paths.push(p);
        }
        assert!(download_many(&items).is_ok());
        for p in paths {
            let _ = std::fs::remove_file(p);
        }
    }

    #[test]
    fn download_many_aggregates_failures_offline() {
        // Two broken destinations + one already-present file: the failure must
        // surface as an Err while no network is touched.
        let mut blocker = std::env::temp_dir();
        blocker.push(format!("lfl-net-many-blocker-{}.tmp", std::process::id()));
        std::fs::write(&blocker, b"x").expect("write");
        let ok_path = std::env::temp_dir().join(format!(
            "lfl-net-many-ok-{}.bin",
            std::process::id()
        ));
        std::fs::write(&ok_path, b"abc").expect("write");

        let items = vec![
            Download::new(
                "https://example.invalid/a",
                ok_path.clone(),
                &hash::sha1_hex(b"abc"),
                3,
            ),
            Download::new(
                "https://example.invalid/b",
                blocker.join("d1").join("a.bin"),
                "",
                5,
            ),
            Download::new(
                "https://example.invalid/c",
                blocker.join("d2").join("b.bin"),
                "",
                5,
            ),
        ];
        let e = download_many(&items).unwrap_err();
        assert!(e.contains("example.invalid"), "must name url: {e}");
        let _ = std::fs::remove_file(&ok_path);
        let _ = std::fs::remove_file(&blocker);
    }

    #[test]
    fn progress_rendering_is_thread_safe() {
        let t = std::thread::spawn(|| {
            set_progress(false);
            assert!(!progress_enabled());
            paint("hidden", 1, 2);
            paint_final("hidden", 2, 2);
            set_progress(true);
            for i in 0..50 {
                paint("x", i, 100);
            }
            paint_final("x", 100, 100);
            paint_clear();
        });
        assert!(t.join().is_ok());
        // Concurrent painters must not deadlock or panic.
        std::thread::scope(|s| {
            for k in 0..4 {
                s.spawn(move || {
                    for i in 0..50 {
                        paint(&format!("t{k}"), i, 50);
                    }
                    paint_final(&format!("t{k}"), 50, 50);
                });
            }
        });
        set_progress(true);
    }

    /// Real network. Run with `cargo test -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn live_get_and_download() {
        let body =
            get("https://launchermeta.mojang.com/mc/game/version_manifest_v2.json").expect("fetch");
        assert!(body.len() > 1000);
        assert_eq!(body[0], b'{');

        let missing = "https://launchermeta.mojang.com/mc/definitely-not-here.json";
        assert!(get_opt(missing).expect("get_opt").is_none());
        let e = get(missing).unwrap_err();
        assert!(e.contains("HTTP 404"), "unexpected error: {e}");

        let dest = std::env::temp_dir().join(format!("lfl-live-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&dest);
        download(
            "https://launchermeta.mojang.com/mc/game/version_manifest_v2.json",
            &dest,
            "",
            0,
        )
        .expect("download");
        assert!(util::file_size(&dest) > 1000);
        // Second call must skip via the empty-sha/exists shortcut.
        assert!(download(
            "https://example.invalid/should-not-be-fetched",
            &dest,
            "",
            0
        )
        .is_ok());
        let _ = std::fs::remove_file(&dest);
    }
}
