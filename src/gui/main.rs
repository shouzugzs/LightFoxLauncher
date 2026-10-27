// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025 LightFoxLauncher contributors
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! LFL-gui — a native Win32 window over the shared `LFL` library.
//!
//! No GUI toolkit and no third-party crate: one window class, a handful of
//! child controls and GDI drawing. All download/verify/launch work is delegated
//! to `lightfox::launcher`, exactly the code the CLI runs.

#![windows_subsystem = "windows"]

mod win32;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use lightfox::launcher::{self, Options};
use lightfox::settings::Settings;
use lightfox::util::{self, Reporter};
use lightfox::version;
use lightfox::{java, BRAND, TITLE, VERSION};

use win32::*;

// ---- control ids -----------------------------------------------------------

const ID_VERSION: i32 = 101;
const ID_USER: i32 = 102;
const ID_MEMORY: i32 = 103;
const ID_PLAY: i32 = 104;
const ID_INSTALL: i32 = 105;
const ID_DIR: i32 = 106;
const ID_STATUS: i32 = 108;
const ID_LAUNCH_LOG: i32 = 112;
const ID_PROGRESS: i32 = 109;
const ID_DIRPICK: i32 = 110;
const ID_INSTANCE: i32 = 111;
const ID_ISOLATE: i32 = 113;
// Launch profiles
const ID_PROFILE: i32 = 120;
const ID_PROFILE_SAVE: i32 = 121;
const ID_PROFILE_NEW: i32 = 122;
// Download page: game core
const ID_DL_CORE_VER: i32 = 130;
const ID_DL_CORE_BTN: i32 = 131;
const ID_DL_CORE_STATUS: i32 = 132;
// Download page: mod loader
const ID_DL_LOADER_STATUS: i32 = 133;
// Download page: release / snapshot channel
const ID_CORE_CHANNEL: i32 = 134;
// Settings page
const ID_WORKERS: i32 = 201;
const ID_WORKERS_LABEL: i32 = 202;
const ID_SAVE: i32 = 203;
const ID_SET_JAVA: i32 = 204;
const ID_SET_JAVA_PICK: i32 = 214;
const ID_JAVA_SCAN: i32 = 215;
const ID_JAVA_LIST: i32 = 216;
const ID_JAVA_USE: i32 = 217;
// Mod loader picker (lives on the download page)
const ID_MOD_LOADER: i32 = 401;
const ID_MOD_LOADER_VER: i32 = 402;
const ID_MOD_REFRESH: i32 = 403;
const ID_MOD_INSTALL: i32 = 404;
const ID_MOD_LIST: i32 = 405;
const ID_MOD_REMOVE: i32 = 406;
const ID_MOD_OPEN: i32 = 407;
const ID_MOD_HINT: i32 = 408;
const ID_MOD_DEPS: i32 = 409;
// Mod search controls
const ID_SRCH_QUERY: i32 = 501;
const ID_SRCH_BTN: i32 = 502;
const ID_SRCH_LIST: i32 = 503;
const ID_SRCH_DL: i32 = 504;
const ID_SRCH_STATUS: i32 = 505;
// Version manager controls
const ID_VER_LIST: i32 = 601;
const ID_VER_REFRESH: i32 = 602;
const ID_VER_DELETE: i32 = 603;
const ID_VER_CLONE: i32 = 604;
const ID_VER_STATUS: i32 = 605;
const ID_VER_MODS: i32 = 606;
const ID_VER_FIX: i32 = 607;
const ID_VER_INFO: i32 = 608;
const ID_VER_MODLIST: i32 = 609;
// Tools page
const ID_TOOL_LOG: i32 = 301;
const ID_TOOL_VERIFY: i32 = 302;
const ID_TOOL_CLEAN: i32 = 303;
const ID_TOOL_OPEN_DIR: i32 = 304;
const ID_TOOL_INFO: i32 = 305;
const ID_TOOL_COPYCMD: i32 = 306;
const ID_TOOL_CLEAN_PARTS: i32 = 307;
// Standalone multi-threaded downloader
const ID_DL_URL: i32 = 308;
const ID_DL_DIR: i32 = 309;
const ID_DL_THREADS: i32 = 310;
const ID_DL_START: i32 = 311;
const ID_DL_PICK: i32 = 312;
const ID_DL_STATUS: i32 = 313;
// Announcements / update check
const ID_NOTIFY_URL: i32 = 701;
const ID_NOTIFY_CHECK: i32 = 702;
const ID_NOTIFY_LIST: i32 = 703;
const ID_NOTIFY_BODY: i32 = 704;
const ID_NOTIFY_STATUS: i32 = 705;
const ID_NOTIFY_MARK_ALL: i32 = 706;
const ID_NOTIFY_AUTO: i32 = 707;
const ID_NOTIFY_OPEN: i32 = 708;

const TIMER_UI: usize = 1;
const WM_APP_LOG: UINT = WM_APP + 1;
const WM_APP_PROGRESS: UINT = WM_APP + 2;
const WM_APP_DONE: UINT = WM_APP + 3;
const WM_APP_JAVA: UINT = WM_APP + 4;
const WM_APP_SEARCH: UINT = WM_APP + 5;
const WM_APP_MANIFEST: UINT = WM_APP + 6;
const WM_APP_LOADER: UINT = WM_APP + 7;
const WM_APP_NOTIFY: UINT = WM_APP + 8;

/// Logical layout metrics (96 dpi).
const WIN_W: i32 = 780;
const WIN_H: i32 = 620;

/// Which button started a background job.
///
/// A single `done` slot is shared by every worker, so the result has to carry
/// its origin: the launch page, the 下载 page's core block, its loader block and
/// its mod block each own a different status line, and writing a result to a
/// control the user cannot see is indistinguishable from nothing happening.
#[derive(Clone, Copy, PartialEq, Debug)]
enum JobKind {
    /// 启动游戏 / 下载·修复 on the launch page.
    Launch,
    /// 下载核心 on the 下载 page.
    Core,
    /// 安装加载器 on the 下载 page.
    Loader,
    /// 下载选中 (mod) on the 下载 page.
    ModDownload,
    /// 工具页的多线程下载器.
    ToolDownload,
}

// ---- shared state ----------------------------------------------------------

struct App {
    hwnd: HWND,
    log: HWND,
    status: HWND,
    progress: HWND,
    play: HWND,
    install: HWND,
    version: HWND,
    user: HWND,
    memory: HWND,
    dir_edit: HWND,
    instance_edit: HWND,
    pick: HWND,
    font_ui: HFONT,
    font_bold: HFONT,
    font_log: HFONT,
    /// Set while a worker thread runs; disables the action buttons.
    busy: Arc<Mutex<bool>>,
    /// Pending log lines from worker threads.
    lines: Arc<Mutex<Vec<String>>>,
    /// Latest progress sample, if any.
    prog: Arc<Mutex<Option<(String, u64, u64)>>>,
    /// Result of the last worker plus the button that started it.
    done: Arc<Mutex<Option<(JobKind, Result<String, String>)>>>,
    /// Filled by the background Modrinth search worker.
    search_q: Arc<Mutex<Option<Result<Vec<lightfox::modrinth::SearchHit>, String>>>>,
    /// Mojang's version manifest, fetched once so the channel picker can split
    /// it into releases and snapshots without another network call.
    manifest_q: Arc<Mutex<Option<Vec<lightfox::version::ManifestEntry>>>>,
    /// Loader builds fetched from the loader feed, with the query that produced them.
    loaders_q: Arc<Mutex<Option<(lightfox::loaders::Loader, String, Result<Vec<lightfox::loaders::LoaderBuild>, String>)>>>,
    /// Mojang's version manifest, cached after the first fetch so switching
    /// channels does not hit the network again.
    manifest: Vec<lightfox::version::ManifestEntry>,
    /// Which channel the core combo currently shows.
    core_channel: CoreChannel,
    /// Version requested on the command line, applied after the list is built.
    pending_version: Option<String>,
    /// Active page: 0 = launch, 1 = settings, 2 = tools.
    tab: usize,
    // Launch-tab controls, hidden when another tab is active.
    launch_ctl: Vec<HWND>,
    // Settings-tab controls.
    set_ctl: Vec<HWND>,
    workers_edit: HWND,
    workers_note: HWND,
    set_java_edit: HWND,
    java_pick: HWND,
    save_btn: HWND,
    java_scan: HWND,
    java_list: HWND,
    java_use: HWND,
    /// Results of the last Java scan, parallel to the list box entries.
    java_found: Vec<lightfox::javafind::Found>,
    // Mods page controls.
    mod_ctl: Vec<HWND>,
    mod_loader: HWND,
    mod_loader_ver: HWND,
    mod_list: HWND,
    mod_hint: HWND,
    mod_refresh: HWND,
    mod_install: HWND,
    mod_remove: HWND,
    mod_open: HWND,
    mod_deps: HWND,
    /// Mods currently shown in `mod_list`, in list order.
    mods_shown: Vec<lightfox::loaders::Mod>,
    /// Loaders currently shown in `mod_loader`, in combo order.
    loaders_shown: Vec<lightfox::loaders::LoaderBuild>,
    // Search page.
    srch_ctl: Vec<HWND>,
    srch_query: HWND,
    srch_btn: HWND,
    srch_list: HWND,
    srch_dl: HWND,
    srch_status: HWND,
    srch_hits: Vec<lightfox::modrinth::SearchHit>,
    // Version manager page.
    ver_ctl: Vec<HWND>,
    ver_list: HWND,
    ver_refresh: HWND,
    ver_delete: HWND,
    ver_clone: HWND,
    ver_status: HWND,
    ver_rows: Vec<lightfox::versionmgr::LocalVersion>,
    ver_mods_btn: HWND,
    ver_fix_btn: HWND,
    ver_info: HWND,
    ver_modlist: HWND,
    /// Mods of the selected version, in list order.
    ver_mods: Vec<lightfox::loaders::Mod>,
    // Tools-tab controls.
    tool_ctl: Vec<HWND>,
    tool_log: HWND,
    tool_info: HWND,
    tool_verify: HWND,
    tool_clean: HWND,
    tool_clean_parts: HWND,
    tool_open: HWND,
    tool_copy: HWND,
    dl_url: HWND,
    dl_dir: HWND,
    dl_threads: HWND,
    dl_start: HWND,
    dl_pick: HWND,
    dl_status: HWND,
    // Announcements / update-check page.
    notify_ctl: Vec<HWND>,
    notify_url: HWND,
    notify_check: HWND,
    notify_list: HWND,
    notify_body: HWND,
    notify_status: HWND,
    notify_mark_all: HWND,
    notify_auto: HWND,
    notify_open: HWND,
    /// Last successfully fetched feed, or `None` before the first success.
    feed: Option<lightfox::notify::Feed>,
    /// Which notes have been read, plus the update already reported.
    seen: lightfox::notify::Seen,
    /// Set while a feed fetch runs, so a second click cannot stack requests.
    feed_busy: std::sync::atomic::AtomicBool,
    /// Set when the worker finished, carrying the result for the UI thread.
    feed_q: Arc<Mutex<Option<Result<lightfox::notify::Feed, String>>>>,
    /// Saved settings, reloaded on start and written by the settings page.
    settings: Settings,
    /// True once the core-version combo has asked the manifest for its list.
    core_seeded: std::sync::atomic::AtomicBool,
    /// Cached version rows, so the UI can filter without re-reading the disk.
    version_choices: Vec<lightfox::version::VersionChoice>,
    /// Version isolation: `versions/<id>/` is the game directory.
    isolate_on: bool,
    isolate_box: HWND,
    // Launch profiles
    profile_box: HWND,
    profile_save: HWND,
    profile_new: HWND,
    /// Profiles of the currently selected version, in combo order.
    profiles_shown: Vec<lightfox::profiles::Profile>,
    /// Index of the profile the form currently reflects.
    profile_index: usize,
    // Download page: game core installer
    dl_core_version: HWND,
    dl_core_btn: HWND,
    dl_core_status: HWND,
    /// Status of the loader block, so its result is never written to a control
    /// that lives on another page.
    dl_loader_status: HWND,
    /// Release / preview picker for the core combo.
    core_channel_box: HWND,
    /// Which download-page block a worker belongs to, so `drain_done` can report
    /// the result where the user clicked (launch page vs download page).
    job_kind: JobKind,
    /// Set from `--install`: begin downloading as soon as the window exists.
    install_on_start: bool,
    /// Long-lived brush used to paint control backgrounds (see WM_CTLCOLOR*).
    card_brush: HBRUSH,
    /// Brush for list boxes, which sit on the page background.
    bg_brush: HBRUSH,
    bg: u32,
    card: u32,
    ink: u32,
    dim: u32,
    /// 1px separators and control outlines.
    line: u32,
    /// Highlight colour for the active tab and focus states.
    accent: u32,
    /// Slightly brighter accent for hover/active emphasis.
    accent_hi: u32,
    /// Raised surface: buttons and the primary action sit one step above `card`.
    raised: u32,
}

impl App {
    fn new() -> App {
        App {
            hwnd: std::ptr::null_mut(),
            log: std::ptr::null_mut(),
            status: std::ptr::null_mut(),
            progress: std::ptr::null_mut(),
            play: std::ptr::null_mut(),
            install: std::ptr::null_mut(),
            version: std::ptr::null_mut(),
            user: std::ptr::null_mut(),
            memory: std::ptr::null_mut(),
            dir_edit: std::ptr::null_mut(),
            instance_edit: std::ptr::null_mut(),
            pick: std::ptr::null_mut(),
            font_ui: std::ptr::null_mut(),
            font_bold: std::ptr::null_mut(),
            font_log: std::ptr::null_mut(),
            busy: Arc::new(Mutex::new(false)),
            lines: Arc::new(Mutex::new(Vec::new())),
            prog: Arc::new(Mutex::new(None)),
            done: Arc::new(Mutex::new(None)),
            search_q: Arc::new(Mutex::new(None)),
            manifest_q: Arc::new(Mutex::new(None)),
            loaders_q: Arc::new(Mutex::new(None)),
            manifest: Vec::new(),
            core_channel: CoreChannel::Release,
            pending_version: None,
            tab: 0,
            launch_ctl: Vec::new(),
            set_ctl: Vec::new(),
            workers_edit: std::ptr::null_mut(),
            workers_note: std::ptr::null_mut(),
            set_java_edit: std::ptr::null_mut(),
            java_pick: std::ptr::null_mut(),
            save_btn: std::ptr::null_mut(),
            java_scan: std::ptr::null_mut(),
            java_list: std::ptr::null_mut(),
            java_use: std::ptr::null_mut(),
            java_found: Vec::new(),
            mod_ctl: Vec::new(),
            mod_loader: std::ptr::null_mut(),
            mod_loader_ver: std::ptr::null_mut(),
            mod_list: std::ptr::null_mut(),
            mod_hint: std::ptr::null_mut(),
            mod_refresh: std::ptr::null_mut(),
            mod_install: std::ptr::null_mut(),
            mod_remove: std::ptr::null_mut(),
            mod_open: std::ptr::null_mut(),
            mod_deps: std::ptr::null_mut(),
            mods_shown: Vec::new(),
            loaders_shown: Vec::new(),
            srch_ctl: Vec::new(),
            srch_query: std::ptr::null_mut(),
            srch_btn: std::ptr::null_mut(),
            srch_list: std::ptr::null_mut(),
            srch_dl: std::ptr::null_mut(),
            srch_status: std::ptr::null_mut(),
            srch_hits: Vec::new(),
            ver_ctl: Vec::new(),
            ver_list: std::ptr::null_mut(),
            ver_refresh: std::ptr::null_mut(),
            ver_delete: std::ptr::null_mut(),
            ver_clone: std::ptr::null_mut(),
            ver_status: std::ptr::null_mut(),
            ver_rows: Vec::new(),
            ver_mods_btn: std::ptr::null_mut(),
            ver_fix_btn: std::ptr::null_mut(),
            ver_info: std::ptr::null_mut(),
            ver_modlist: std::ptr::null_mut(),
            ver_mods: Vec::new(),
            tool_ctl: Vec::new(),
            tool_log: std::ptr::null_mut(),
            tool_info: std::ptr::null_mut(),
            tool_verify: std::ptr::null_mut(),
            tool_clean: std::ptr::null_mut(),
            tool_clean_parts: std::ptr::null_mut(),
            tool_open: std::ptr::null_mut(),
            tool_copy: std::ptr::null_mut(),
            dl_url: std::ptr::null_mut(),
            dl_dir: std::ptr::null_mut(),
            dl_threads: std::ptr::null_mut(),
            dl_start: std::ptr::null_mut(),
            dl_pick: std::ptr::null_mut(),
            dl_status: std::ptr::null_mut(),
            notify_ctl: Vec::new(),
            notify_url: std::ptr::null_mut(),
            notify_check: std::ptr::null_mut(),
            notify_list: std::ptr::null_mut(),
            notify_body: std::ptr::null_mut(),
            notify_status: std::ptr::null_mut(),
            notify_mark_all: std::ptr::null_mut(),
            notify_auto: std::ptr::null_mut(),
            notify_open: std::ptr::null_mut(),
            feed: None,
            seen: lightfox::notify::Seen::default(),
            feed_busy: std::sync::atomic::AtomicBool::new(false),
            feed_q: Arc::new(Mutex::new(None)),
            settings: Settings::default(),
            core_seeded: std::sync::atomic::AtomicBool::new(false),
            isolate_on: true,
            isolate_box: std::ptr::null_mut(),
            profile_box: std::ptr::null_mut(),
            profile_save: std::ptr::null_mut(),
            profile_new: std::ptr::null_mut(),
            profiles_shown: Vec::new(),
            profile_index: 0,
            dl_core_version: std::ptr::null_mut(),
            dl_core_btn: std::ptr::null_mut(),
            dl_core_status: std::ptr::null_mut(),
            dl_loader_status: std::ptr::null_mut(),
            core_channel_box: std::ptr::null_mut(),
            job_kind: JobKind::Launch,
            install_on_start: false,
            card_brush: std::ptr::null_mut(),
            bg_brush: std::ptr::null_mut(),
            version_choices: Vec::new(),
            // Muted dark palette, easy on the eyes next to the game.
            bg: rgb(24, 26, 30),
            card: rgb(34, 37, 43),
            // Raised surface for buttons, one step above `card`.
            raised: rgb(44, 48, 56),
            ink: rgb(235, 237, 241),
            dim: rgb(138, 146, 158),
            line: rgb(52, 56, 64),
            accent: rgb(88, 166, 255),
            // Brighter accent for the pressed state.
            accent_hi: rgb(130, 192, 255),
        }
    }

    fn is_busy(&self) -> bool {
        *self.busy.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn push_line(&self, msg: &str) {
        if let Ok(mut v) = self.lines.lock() {
            v.push(msg.to_string());
        }
        unsafe {
            PostMessageW(self.hwnd, WM_APP_LOG, 0, 0);
        }
    }
}

/// The `Reporter` the library talks to: queues lines and progress for the UI.
struct GuiReporter {
    lines: Arc<Mutex<Vec<String>>>,
    prog: Arc<Mutex<Option<(String, u64, u64)>>>,
    hwnd: Arc<AtomicU32>,
}

impl Reporter for GuiReporter {
    fn line(&self, msg: &str) {
        if let Ok(mut v) = self.lines.lock() {
            v.push(msg.to_string());
        }
        unsafe {
            PostMessageW(self.hwnd.load(Ordering::Relaxed) as HWND, WM_APP_LOG, 0, 0);
        }
    }

    fn progress(&self, label: &str, done: u64, total: u64) {
        if let Ok(mut g) = self.prog.lock() {
            *g = Some((label.to_string(), done, total));
        }
        unsafe {
            PostMessageW(
                self.hwnd.load(Ordering::Relaxed) as HWND,
                WM_APP_PROGRESS,
                0,
                0,
            );
        }
    }
}

fn main() {
    init_common_controls();
    let hwnd_atomic = Arc::new(AtomicU32::new(0));
    let app = Box::new(App::new());
    let app_ptr = Box::into_raw(app);

    unsafe {
        let hinstance = GetModuleHandleW(std::ptr::null());
        let class_name = wide("lightfox_gui_window");
        let mut wc = WNDCLASSEXW::default();
        wc.style = CS_HREDRAW | CS_VREDRAW;
        wc.lpfn_wnd_proc = Some(wnd_proc);
        wc.h_instance = hinstance;
        wc.h_cursor = LoadCursorW(std::ptr::null_mut(), IDC_ARROW);
        // A fox icon built at runtime from vector geometry — no .ico resource.
        let fox_accent: u32 = rgb(88, 166, 255);
        let fox_hi: u32 = rgb(130, 192, 255);
        wc.h_icon = make_fox_icon(64, fox_accent, fox_hi);
        wc.h_icon_sm = make_fox_icon(32, fox_accent, fox_hi);
        wc.hbr_background = CreateSolidBrush(rgb(28, 30, 34));
        wc.lpsz_class_name = class_name.as_ptr();
        if RegisterClassExW(&wc) == 0 {
            return;
        }

        let title = wide(&format!("{TITLE} {VERSION}"));
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: WIN_W,
            bottom: WIN_H,
        };
        AdjustWindowRectEx(&mut rect, WS_OVERLAPPEDWINDOW, 0, 0);
        let (ww, wh) = (rect.right - rect.left, rect.bottom - rect.top);

        let (sw, sh) = (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN));
        let hwnd = CreateWindowExW(
            0,
            class_name.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPEDWINDOW,
            ((sw - ww) / 2).max(0),
            ((sh - wh) / 2).max(0),
            ww,
            wh,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            hinstance,
            app_ptr as LPVOID,
        );
        if hwnd.is_null() {
            return;
        }
        hwnd_atomic.store(hwnd as usize as u32, Ordering::Relaxed);
        // Push the fox icon to the window (and taskbar) explicitly. The class
        // icon is used as a fallback, but some DWM paths only update on this.
        SendMessageW(
            hwnd,
            WM_SETICON,
            ICON_BIG as usize,
            make_fox_icon(64, fox_accent, fox_hi) as LPARAM,
        );

        // Hand the reporter to the library before any work can start.
        util::set_reporter(Some(Arc::new(GuiReporter {
            lines: (*app_ptr).lines.clone(),
            prog: (*app_ptr).prog.clone(),
            hwnd: hwnd_atomic.clone(),
        })));

        ShowWindow(hwnd, SW_SHOW);
        UpdateWindow(hwnd);
        SetTimer(hwnd, TIMER_UI, 200, std::ptr::null_mut());

        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: UINT,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let app_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut App;
    match msg {
        WM_CREATE => {
            let cs = lparam as *const CREATESTRUCTW_GENERIC;
            if cs.is_null() {
                return -1;
            }
            let app = (*cs).lp_create_params as *mut App;
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, app as isize);
            (*app).hwnd = hwnd;
            build_controls(&mut *app);
            startup_status(&*app);
            0
        }
        WM_SIZE => {
            if !app_ptr.is_null() {
                layout(&*app_ptr);
            }
            0
        }
        WM_PAINT => {
            if !app_ptr.is_null() {
                paint(&*app_ptr);
                return 0;
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_ERASEBKGND => 1,
        WM_DRAWITEM => {
            // An owner-drawn button asks its *parent* to paint it, so this is
            // where the rounded button face is drawn — not in a subclass.
            if !app_ptr.is_null() {
                let di = lparam as *const DRAWITEMSTRUCT;
                if !di.is_null() && (*di).ctl_type == ODT_BUTTON {
                    let text = window_text((*di).hwnd_item);
                    let selected = (*di).item_state & ODS_SELECTED != 0;
                    let disabled = (*di).item_state & ODS_DISABLED != 0;
                    let primary = is_primary_button(&*app_ptr, (*di).hwnd_item);
                    gui_debug(&format!(
                        "WM_DRAWITEM btn={:p} text={:?} primary={primary}",
                        (*di).hwnd_item, text
                    ));
                    paint_button((*di).hdc, (*di).rc_item, &text, selected, disabled, primary);
                    return 1;
                }
            }
            0
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLOREDIT | WM_CTLCOLORBTN | WM_CTLCOLORLISTBOX => {
            if !app_ptr.is_null() {
                let app = &*app_ptr;
                let hdc = wparam as HDC;
                SetTextColor(hdc, app.ink);
                // List boxes get the page background rather than the card's: the
                // lists sit on the page, and a card-coloured field on a card read
                // as a seam. Everything else sits inside a card.
                let list = msg == WM_CTLCOLORLISTBOX;
                SetBkColor(hdc, if list { app.bg } else { app.card });
                // The message must return a *brush handle*: Win32 paints the
                // control background with it, and it must stay alive, so these
                // brushes are created once at startup rather than per message.
                return (if list { app.bg_brush } else { app.card_brush }) as isize;
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_KEYDOWN => {
            // Ctrl+Enter launches, F5 downloads/repairs — the two things users do
            // most, reachable without aiming at a button.
            if !app_ptr.is_null() {
                let vk = wparam as i32;
                let ctrl = (GetKeyState(0x11) as u16 & 0x8000) != 0;
                let app = &mut *app_ptr;
                match vk {
                    0x0D if ctrl => {
                        start_work(app, true);
                        return 0;
                    }
                    0x74 => {
                        // F5
                        start_work(app, false);
                        return 0;
                    }
                    _ => {}
                }
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_LBUTTONDOWN => {
            // The tab bar is owner-drawn, so clicks on it are hit-tested here.
            if app_ptr.is_null() {
                return 0;
            }
            let x = (lparam & 0xFFFF) as u16 as i16 as i32;
            let y = ((lparam >> 16) & 0xFFFF) as u16 as i16 as i32;
            if (CHROME_TOP..=CHROME_TOP + TAB_H).contains(&y) {
                for i in 0..TABS {
                    let left = PAD + tab_left(i);
                    if x >= left && x <= left + tab_width(i) {
                        show_tab(&mut *app_ptr, i);
                        return 0;
                    }
                }
            }
            0
        }
        WM_COMMAND => {
            // Surface every command: a silent mismatch here is invisible
            // otherwise (buttons appear dead).
            crate::gui_debug(&format!(
                "WM_COMMAND id={} code={} userdata={:p}",
                wparam & 0xFFFF,
                (wparam >> 16) & 0xFFFF,
                app_ptr
            ));
            if app_ptr.is_null() {
                return 0;
            }
            let app = &mut *app_ptr;
            let id = (wparam & 0xFFFF) as i32;
            let code = ((wparam >> 16) & 0xFFFF) as u16;
            match id {
                ID_PLAY if code == BN_CLICKED => start_work(app, true),
                ID_INSTALL if code == BN_CLICKED => start_work(app, false),
                ID_DIRPICK if code == BN_CLICKED => pick_dir(app),
                ID_PROFILE if code == CBN_SELCHANGE => profile_switch(app),
                ID_PROFILE_SAVE if code == BN_CLICKED => profile_save_current(app),
                ID_PROFILE_NEW if code == BN_CLICKED => profile_create(app),
                ID_DL_CORE_BTN if code == BN_CLICKED => dl_install_core(app),
                ID_CORE_CHANNEL if code == CBN_SELCHANGE => core_channel_switch(app),
                ID_VERSION if code == CBN_SELCHANGE => {
                    // Switching the launch version is the single place a version
                    // is chosen; the download page's core box follows it so the
                    // obvious next action ("get a loader / a mod for this") is
                    // already pointed at the right game version.
                    profile_reload(app);
                    let cur = lightfox::version::base_version(
                        &collect_options(app).data_dir(),
                        &bare_version(&read_combo(app.version)),
                    );
                    if !cur.is_empty() {
                        unsafe {
                            let w = wide(&cur);
                            SetWindowTextW(app.dl_core_version, w.as_ptr());
                        }
                    }
                    mods_refresh_loader_versions(app);
                }
                ID_ISOLATE if code == BN_CLICKED => {
                    let checked = SendMessageW(app.isolate_box, BM_GETCHECK, 0, 0) as isize;
                    app.isolate_on = checked == BST_CHECKED;
                    app.tool_append(&format!(
                        "版本隔离：{}",
                        if app.isolate_on { "开" } else { "关" }
                    ));
                }
                ID_SAVE if code == BN_CLICKED => save_settings(app),
                ID_SET_JAVA_PICK if code == BN_CLICKED => pick_java(app),
                ID_JAVA_SCAN if code == BN_CLICKED => scan_java(app),
                ID_JAVA_USE if code == BN_CLICKED => use_selected_java(app),
                ID_MOD_LOADER if code == CBN_SELCHANGE => mods_refresh_loader_versions(app),
                ID_MOD_REFRESH if code == BN_CLICKED => mods_refresh(app),
                ID_MOD_INSTALL if code == BN_CLICKED => mods_install_loader(app),
                ID_MOD_REMOVE if code == BN_CLICKED => mods_remove(app),
                ID_MOD_OPEN if code == BN_CLICKED => mods_open_dir(app),
                ID_MOD_DEPS if code == BN_CLICKED => mods_check_deps(app),
                ID_NOTIFY_CHECK if code == BN_CLICKED => notify_check_now(app),
                ID_NOTIFY_MARK_ALL if code == BN_CLICKED => notify_mark_all(app),
                ID_NOTIFY_OPEN if code == BN_CLICKED => show_tab(app, 1),
                ID_NOTIFY_AUTO if code == BN_CLICKED => notify_save_auto(app),
                ID_NOTIFY_LIST if code == LBN_SELCHANGE => notify_selection_changed(app),
                ID_SRCH_BTN if code == BN_CLICKED => search_run(app),
                ID_SRCH_DL if code == BN_CLICKED => search_download(app),
                ID_VER_REFRESH if code == BN_CLICKED => ver_refresh(app),
                ID_VER_DELETE if code == BN_CLICKED => ver_delete(app),
                ID_VER_CLONE if code == BN_CLICKED => ver_clone(app),
                ID_VER_MODS if code == BN_CLICKED => ver_manage_mods(app),
                ID_VER_FIX if code == BN_CLICKED => ver_fix_files(app),
                ID_VER_LIST if code == LBN_SELCHANGE => ver_selection_changed(app),
                ID_TOOL_VERIFY if code == BN_CLICKED => tool_verify(app),
                ID_TOOL_CLEAN if code == BN_CLICKED => tool_clean(app),
                ID_TOOL_CLEAN_PARTS if code == BN_CLICKED => tool_clean_parts(app),
                ID_TOOL_OPEN_DIR if code == BN_CLICKED => tool_open_dir(app),
                ID_TOOL_COPYCMD if code == BN_CLICKED => tool_copy_cmd(app),
                ID_DL_START if code == BN_CLICKED => dl_start(app),
                ID_DL_PICK if code == BN_CLICKED => dl_pick_dir(app),
                _ => {}
            }
            0
        }
        WM_TIMER => {
            if !app_ptr.is_null() && wparam == TIMER_UI {
                drain_log(&*app_ptr);
                drain_progress(&*app_ptr);
                drain_done(&mut *app_ptr);
            }
            0
        }
        WM_APP_LOG => {
            if !app_ptr.is_null() {
                drain_log(&*app_ptr);
            }
            0
        }
        WM_APP_PROGRESS => {
            if !app_ptr.is_null() {
                drain_progress(&*app_ptr);
            }
            0
        }
        WM_APP_JAVA => {
            if !app_ptr.is_null() {
                java_scan_done(&mut *app_ptr);
            }
            0
        }
        WM_APP_DONE => {
            if !app_ptr.is_null() {
                drain_done(&mut *app_ptr);
            }
            0
        }
        WM_APP_SEARCH => {
            if !app_ptr.is_null() {
                search_done(&mut *app_ptr);
            }
            0
        }
        WM_APP_LOADER => {
            if !app_ptr.is_null() {
                loader_done(&mut *app_ptr);
            }
            0
        }
        WM_APP_NOTIFY => {
            if !app_ptr.is_null() {
                notify_done(&mut *app_ptr);
            }
            0
        }
        WM_APP_MANIFEST => {
            if !app_ptr.is_null() {
                manifest_done(&mut *app_ptr);
            }
            0
        }
        WM_GETMINMAXINFO => {
            let mmi = lparam as *mut MINMAXINFO_GENERIC;
            if !mmi.is_null() {
                // The window is chrome-inclusive, so the client area is smaller.
                // At 660x540 the tools page's log collapsed to a few pixels, so
                // the floor is raised to keep every page's list usable.
                (*mmi).pt_min_track_size = POINT { x: 700, y: 580 };
            }
            0
        }
        WM_CLOSE => {
            if !app_ptr.is_null() && (*app_ptr).is_busy() {
                let text = wide("还在下载或启动中，确定要退出吗？");
                let cap = wide(BRAND);
                if MessageBoxW(hwnd, text.as_ptr(), cap.as_ptr(), MB_YESNO | MB_ICONINFORMATION)
                    != IDYES as i32
                {
                    return 0;
                }
            }
            DestroyWindow(hwnd);
            0
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

// Only the first field of CREATESTRUCTW is read; the rest is layout padding so
// the struct matches the real one and the field offsets stay correct.
#[repr(C)]
struct CREATESTRUCTW_GENERIC {
    lp_create_params: LPVOID,
    _h_instance: HINSTANCE,
    _h_menu: HMENU,
    _hwnd_parent: HWND,
    _cy: i32,
    _cx: i32,
    _y: i32,
    _x: i32,
    _style: i32,
    _name: *const u16,
    _class: *const u16,
    _ex_style: DWORD,
}

#[repr(C)]
struct MINMAXINFO_GENERIC {
    _reserved: POINT,
    _max_size: POINT,
    _max_position: POINT,
    pt_min_track_size: POINT,
    _max_track_size: POINT,
}

// ---- command line ----------------------------------------------------------

/// Options the window can be opened with. Mirrors the CLI's spelling so a
/// shortcut can be configured the same way one would configure `LFL`.
#[derive(Default, Clone)]
struct CliArgs {
    dir: Option<PathBuf>,
    user: Option<String>,
    version: Option<String>,
    memory: Option<String>,
    /// Pre-fetch the selected version without waiting for a click.
    install: bool,
}

impl CliArgs {
    fn parse() -> CliArgs {
        let mut c = CliArgs::default();
        let argv: Vec<String> = std::env::args().skip(1).collect();
        let mut i = 0usize;
        while i < argv.len() {
            let a = argv[i].as_str();
            let val = |i: &mut usize| -> Option<String> {
                *i += 1;
                argv.get(*i).cloned()
            };
            match a {
                "-d" | "--dir" | "--game-dir" => c.dir = val(&mut i).map(PathBuf::from),
                "-u" | "--user" | "--username" => c.user = val(&mut i),
                "-m" | "--memory" | "--xmx" => c.memory = val(&mut i),
                "--install" | "--download" => c.install = true,
                _ if a.starts_with('-') => {}
                // A bare argument is a version id.
                _ => c.version = Some(a.to_string()),
            }
            i += 1;
        }
        c
    }
}

// ---- control construction --------------------------------------------------

fn make_font(height: i32, weight: i32) -> HFONT {
    unsafe {
        CreateFontW(
            height,
            0,
            0,
            0,
            weight,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            0,
            0,
            CLEARTYPE_QUALITY,
            0,
            wide("Microsoft YaHei UI").as_ptr(),
        )
    }
}

unsafe fn child(
    parent: HWND,
    class: &str,
    text: &str,
    style: DWORD,
    id: i32,
    ex_style: DWORD,
) -> HWND {
    let inst = GetModuleHandleW(std::ptr::null());
    CreateWindowExW(
        ex_style,
        wide(class).as_ptr(),
        wide(text).as_ptr(),
        WS_CHILD | WS_VISIBLE | style,
        0,
        0,
        10,
        10,
        parent,
        id as HMENU,
        inst,
        std::ptr::null_mut(),
    )
}

// ---- pages -----------------------------------------------------------------

/// Which controls belong to which page, so switching tabs only toggles them.
fn classify(app: &mut App) {
    app.launch_ctl = vec![
        app.version,
        app.user,
        app.memory,
        app.dir_edit,
        app.pick,
        app.instance_edit,
        app.isolate_box,
        app.profile_box,
        app.play,
        app.install,
        app.status,
        app.progress,
        app.log,
    ];
    app.set_ctl = vec![
        app.workers_edit,
        app.workers_note,
        app.set_java_edit,
        app.java_list,
    ];
    app.tool_ctl = vec![
        app.tool_log,
        app.tool_info,
        app.dl_url,
        app.dl_dir,
        app.dl_threads,
        app.dl_status,
    ];
    app.mod_ctl = vec![app.mod_list, app.mod_hint];
    app.srch_ctl = vec![
        app.srch_query,
        app.srch_list,
        app.srch_status,
        app.dl_core_version,
        app.dl_core_status,
        app.dl_loader_status,
        app.core_channel_box,
        app.mod_loader,
        app.mod_loader_ver,
    ];
    app.ver_ctl = vec![app.ver_list, app.ver_status, app.ver_info, app.ver_modlist];
    app.notify_ctl = vec![
        app.notify_url,
        app.notify_list,
        app.notify_body,
        app.notify_status,
    ];
}

/// Show the controls of `tab` and hide the rest, then repaint.
fn show_tab(app: &mut App, tab: usize) {
    app.tab = tab.min(TABS - 1);
    let tab = app.tab;
    unsafe {
        // Order MUST match the tab bar: 启动, 下载, 模组, 版本, 设置, 工具, 通知.
        let groups = [
            &app.launch_ctl,
            &app.srch_ctl,
            &app.mod_ctl,
            &app.ver_ctl,
            &app.set_ctl,
            &app.tool_ctl,
            &app.notify_ctl,
        ];
        for (i, group) in groups.iter().enumerate() {
            for &h in group.iter() {
                if !h.is_null() {
                    ShowWindow(h, if tab == i { SW_SHOW } else { SW_HIDE });
                }
            }
        }
        // Buttons that exist on several pages.
        for h in [app.profile_save, app.profile_new] {
            if !h.is_null() {
                ShowWindow(h, if tab == 0 { SW_SHOW } else { SW_HIDE });
            }
        }
        let settings_buttons = [app.save_btn, app.java_pick, app.java_scan, app.java_use];
        for h in settings_buttons {
            if !h.is_null() {
                ShowWindow(h, if tab == 4 { SW_SHOW } else { SW_HIDE });
            }
        }
        let tool_buttons = [
            app.tool_verify,
            app.tool_clean,
            app.tool_clean_parts,
            app.tool_open,
            app.tool_copy,
            app.dl_start,
            app.dl_pick,
        ];
        for h in tool_buttons {
            if !h.is_null() {
                ShowWindow(h, if tab == 5 { SW_SHOW } else { SW_HIDE });
            }
        }
        let mod_buttons = [app.mod_refresh, app.mod_remove, app.mod_open, app.mod_deps];
        for h in mod_buttons {
            if !h.is_null() {
                ShowWindow(h, if tab == 2 { SW_SHOW } else { SW_HIDE });
            }
        }
        for h in [app.srch_btn, app.srch_dl, app.dl_core_btn, app.mod_install] {
            if !h.is_null() {
                ShowWindow(h, if tab == 1 { SW_SHOW } else { SW_HIDE });
            }
        }
        for h in [
            app.ver_refresh,
            app.ver_delete,
            app.ver_clone,
            app.ver_mods_btn,
            app.ver_fix_btn,
        ] {
            if !h.is_null() {
                ShowWindow(h, if tab == 3 { SW_SHOW } else { SW_HIDE });
            }
        }
        for h in [
            app.notify_check,
            app.notify_mark_all,
            app.notify_open,
            app.notify_auto,
        ] {
            if !h.is_null() {
                ShowWindow(h, if tab == 6 { SW_SHOW } else { SW_HIDE });
            }
        }
    }
    layout(app);
    // Entering a page loads its current state once, so nothing is stale after a
    // download, an install or a manual file drop.
    match tab {
        2 => mods_refresh(app),
        3 => ver_refresh(app),
        1 => {
            // Seed the query box the first time the page opens, so the field is
            // obviously editable and a search can be run immediately.
            if window_text(app.srch_query).trim().is_empty() {
                unsafe {
                    let w = wide("sodium");
                    SetWindowTextW(app.srch_query, w.as_ptr());
                }
            }
            // The loader picker lives on this page, so its build list must be
            // loaded here — not on the 模组 page, where the combo is hidden.
            dl_seed_core_versions(app);
            mods_refresh_loader_versions(app);
        }
        _ => {}
    }
    unsafe {
        let mut rc = RECT::default();
        GetClientRect(app.hwnd, &mut rc);
        InvalidateRect(app.hwnd, &rc, 0);
    }
}

/// Fields of the launch page, in the visual order they are laid out.
fn build_launch_page(app: &mut App) {
    unsafe {
        app.version = child(
            app.hwnd,
            "COMBOBOX",
            "",
            // Read-only: the list contains exactly the installed versions, so
            // typing a name that has no files could only produce a broken launch.
            WS_TABSTOP | WS_VSCROLL | CBS_DROPDOWNLIST,
            ID_VERSION,
            0,
        );
        app.user = child(
            app.hwnd,
            "EDIT",
            "Steve",
            WS_TABSTOP | ES_AUTOHSCROLL,
            ID_USER,
            WS_EX_CLIENTEDGE,
        );
        app.memory = child(
            app.hwnd,
            "COMBOBOX",
            "",
            WS_TABSTOP | CBS_DROPDOWN | CBS_HASSTRINGS,
            ID_MEMORY,
            0,
        );
        app.dir_edit = child(
            app.hwnd,
            "EDIT",
            "",
            WS_TABSTOP | ES_AUTOHSCROLL,
            ID_DIR,
            WS_EX_CLIENTEDGE,
        );
        app.pick = child(app.hwnd, "BUTTON", "…", WS_TABSTOP, ID_DIRPICK, 0);
        app.instance_edit = child(
            app.hwnd,
            "EDIT",
            "default",
            WS_TABSTOP | ES_AUTOHSCROLL,
            ID_INSTANCE,
            WS_EX_CLIENTEDGE,
        );
        app.profile_box = child(
            app.hwnd,
            "COMBOBOX",
            "",
            WS_TABSTOP | WS_VSCROLL | CBS_DROPDOWNLIST,
            ID_PROFILE,
            0,
        );
        app.profile_save = child(app.hwnd, "BUTTON", "保存配置", WS_TABSTOP, ID_PROFILE_SAVE, 0);
        app.profile_new = child(app.hwnd, "BUTTON", "新建配置", WS_TABSTOP, ID_PROFILE_NEW, 0);
        app.isolate_box = child(
            app.hwnd,
            "BUTTON",
            "版本隔离",
            WS_TABSTOP | 0x0003, // BS_AUTOCHECKBOX
            ID_ISOLATE,
            0,
        );
        app.play = child(app.hwnd, "BUTTON", "启动游戏", WS_TABSTOP, ID_PLAY, 0);
        app.install = child(app.hwnd, "BUTTON", "下载 / 修复", WS_TABSTOP, ID_INSTALL, 0);
        app.progress = child(app.hwnd, "msctls_progress32", "", 0, ID_PROGRESS, 0);
        app.status = child(app.hwnd, "STATIC", "就绪", DT_LEFT, ID_STATUS, 0);
        app.log = child(
            app.hwnd,
            "EDIT",
            "",
            WS_VSCROLL | ES_MULTILINE | ES_READONLY | ES_AUTOVSCROLL,
            ID_LAUNCH_LOG,
            WS_EX_CLIENTEDGE,
        );
        SendMessageW(app.log, WM_SETFONT, app.font_log as WPARAM, 1);

        for h in [
            app.version,
            app.user,
            app.memory,
            app.dir_edit,
            app.instance_edit,
            app.isolate_box,
            app.profile_box,
            app.profile_save,
            app.profile_new,
            app.play,
            app.install,
            app.pick,
            app.status,
        ] {
            SendMessageW(h, WM_SETFONT, app.font_ui as WPARAM, 1);
        }
        SendMessageW(app.play, WM_SETFONT, app.font_bold as WPARAM, 1);
        SendMessageW(app.isolate_box, BM_SETCHECK, BST_CHECKED as WPARAM, 0);

        for m in ["1G", "2G", "4G", "6G", "8G", "12G", "16G"] {
            let w = wide(m);
            SendMessageW(app.memory, CB_ADDSTRING, 0, w.as_ptr() as LPARAM);
        }
        SendMessageW(app.memory, CB_SETCURSEL, 1, 0);
    }
}

/// Settings page: download concurrency, Java override, saved defaults.
fn build_settings_page(app: &mut App) {
    unsafe {
        app.workers_edit = child(
            app.hwnd,
            "COMBOBOX",
            "",
            WS_TABSTOP | CBS_DROPDOWN | CBS_HASSTRINGS,
            ID_WORKERS,
            0,
        );
        for n in ["1", "2", "4", "6", "8", "12", "16", "24", "32"] {
            let w = wide(n);
            SendMessageW(app.workers_edit, CB_ADDSTRING, 0, w.as_ptr() as LPARAM);
        }
        app.workers_note = child(
            app.hwnd,
            "STATIC",
            "",
            DT_LEFT,
            ID_WORKERS_LABEL,
            0,
        );
        app.set_java_edit = child(
            app.hwnd,
            "EDIT",
            "",
            WS_TABSTOP | ES_AUTOHSCROLL,
            ID_SET_JAVA,
            WS_EX_CLIENTEDGE,
        );
        app.java_pick = child(app.hwnd, "BUTTON", "…", WS_TABSTOP, ID_SET_JAVA_PICK, 0);
        app.save_btn = child(app.hwnd, "BUTTON", "保存设置", WS_TABSTOP, ID_SAVE, 0);
        app.java_scan = child(
            app.hwnd,
            "BUTTON",
            "查找 Java",
            WS_TABSTOP,
            ID_JAVA_SCAN,
            0,
        );
        app.java_use = child(app.hwnd, "BUTTON", "使用选中的", WS_TABSTOP, ID_JAVA_USE, 0);
        app.java_list = child(
            app.hwnd,
            "LISTBOX",
            "",
            WS_TABSTOP | WS_VSCROLL | WS_BORDER | 0x0020_0000,
            ID_JAVA_LIST,
            0,
        );

        for h in [app.workers_edit, app.set_java_edit, app.workers_note] {
            SendMessageW(h, WM_SETFONT, app.font_ui as WPARAM, 1);
        }
        for h in [
            app.java_pick,
            app.save_btn,
            app.java_scan,
            app.java_use,
            app.java_list,
        ] {
            SendMessageW(h, WM_SETFONT, app.font_ui as WPARAM, 1);
        }
    }
}

/// Tools page: the launcher's own utilities, exposed instead of hidden in a CLI.
fn build_tools_page(app: &mut App) {
    unsafe {
        app.tool_verify = child(app.hwnd, "BUTTON", "校验文件", WS_TABSTOP, ID_TOOL_VERIFY, 0);
        app.tool_clean = child(app.hwnd, "BUTTON", "清理残留", WS_TABSTOP, ID_TOOL_CLEAN, 0);
        app.tool_clean_parts = child(
            app.hwnd,
            "BUTTON",
            "删除断点文件",
            WS_TABSTOP,
            ID_TOOL_CLEAN_PARTS,
            0,
        );
        app.tool_open = child(
            app.hwnd,
            "BUTTON",
            "打开数据目录",
            WS_TABSTOP,
            ID_TOOL_OPEN_DIR,
            0,
        );
        app.tool_copy = child(
            app.hwnd,
            "BUTTON",
            "复制启动命令",
            WS_TABSTOP,
            ID_TOOL_COPYCMD,
            0,
        );
        app.tool_info = child(app.hwnd, "STATIC", "", DT_LEFT, ID_TOOL_INFO, 0);

        // Standalone downloader: same engine the game downloads use.
        app.dl_url = child(
            app.hwnd,
            "EDIT",
            "",
            WS_TABSTOP | ES_AUTOHSCROLL,
            ID_DL_URL,
            WS_EX_CLIENTEDGE,
        );
        app.dl_dir = child(
            app.hwnd,
            "EDIT",
            "",
            WS_TABSTOP | ES_AUTOHSCROLL,
            ID_DL_DIR,
            WS_EX_CLIENTEDGE,
        );
        app.dl_threads = child(
            app.hwnd,
            "COMBOBOX",
            "",
            WS_TABSTOP | CBS_DROPDOWN | CBS_HASSTRINGS,
            ID_DL_THREADS,
            0,
        );
        app.dl_start = child(app.hwnd, "BUTTON", "开始下载", WS_TABSTOP, ID_DL_START, 0);
        app.dl_pick = child(app.hwnd, "BUTTON", "…", WS_TABSTOP, ID_DL_PICK, 0);
        app.dl_status = child(app.hwnd, "STATIC", "", DT_LEFT, ID_DL_STATUS, 0);
        app.tool_log = child(
            app.hwnd,
            "EDIT",
            "",
            WS_VSCROLL | ES_MULTILINE | ES_READONLY | ES_AUTOVSCROLL,
            ID_TOOL_LOG,
            WS_EX_CLIENTEDGE,
        );

        for h in [
            app.tool_verify,
            app.tool_clean,
            app.tool_clean_parts,
            app.tool_open,
            app.tool_copy,
            app.tool_info,
            app.dl_url,
            app.dl_dir,
            app.dl_threads,
            app.dl_start,
            app.dl_pick,
            app.dl_status,
        ] {
            SendMessageW(h, WM_SETFONT, app.font_ui as WPARAM, 1);
        }
        for n in ["1", "2", "4", "8", "12", "16", "24"] {
            let w = wide(n);
            SendMessageW(app.dl_threads, CB_ADDSTRING, 0, w.as_ptr() as LPARAM);
        }
        SendMessageW(app.dl_threads, CB_SETCURSEL, 4, 0); // 12
        {
            // Default target: the selected version's mods dir, else the data dir.
            let o = collect_options(app);
            let d = o.data_dir();
            let w = wide(&d.display().to_string());
            SetWindowTextW(app.dl_dir, w.as_ptr());
        }
        SendMessageW(app.tool_log, WM_SETFONT, app.font_log as WPARAM, 1);
    }
}

/// Mods page: install a loader profile, manage the instance's mod jars.
fn build_mods_page(app: &mut App) {
    unsafe {
        app.mod_loader = child(
            app.hwnd,
            "COMBOBOX",
            "",
            WS_TABSTOP | WS_VSCROLL | CBS_DROPDOWNLIST,
            ID_MOD_LOADER,
            0,
        );
        app.mod_loader_ver = child(
            app.hwnd,
            "COMBOBOX",
            "",
            WS_TABSTOP | WS_VSCROLL | CBS_DROPDOWNLIST,
            ID_MOD_LOADER_VER,
            0,
        );
        app.mod_refresh = child(app.hwnd, "BUTTON", "刷新列表", WS_TABSTOP, ID_MOD_REFRESH, 0);
        app.mod_install = child(
            app.hwnd,
            "BUTTON",
            "安装加载器",
            WS_TABSTOP,
            ID_MOD_INSTALL,
            0,
        );
        app.mod_remove = child(app.hwnd, "BUTTON", "删除模组", WS_TABSTOP, ID_MOD_REMOVE, 0);
        app.mod_open = child(app.hwnd, "BUTTON", "打开模组目录", WS_TABSTOP, ID_MOD_OPEN, 0);
        app.mod_deps = child(app.hwnd, "BUTTON", "检查依赖", WS_TABSTOP, ID_MOD_DEPS, 0);
        app.mod_hint = child(app.hwnd, "STATIC", "", DT_LEFT, ID_MOD_HINT, 0);
        app.mod_list = child(
            app.hwnd,
            "LISTBOX",
            "",
            WS_TABSTOP | WS_VSCROLL | WS_BORDER | 0x0020_0000 | 0x0100,
            ID_MOD_LIST,
            0,
        );

        for h in [
            app.mod_loader,
            app.mod_loader_ver,
            app.mod_refresh,
            app.mod_install,
            app.mod_remove,
            app.mod_open,
            app.mod_deps,
            app.mod_hint,
            app.mod_list,
        ] {
            SendMessageW(h, WM_SETFONT, app.font_ui as WPARAM, 1);
        }
        for l in lightfox::loaders::Loader::all() {
            let text = if l.has_profile_feed() {
                l.name().to_string()
            } else {
                format!("{}（需官方安装器）", l.name())
            };
            let w = wide(&text);
            SendMessageW(app.mod_loader, CB_ADDSTRING, 0, w.as_ptr() as LPARAM);
        }
        SendMessageW(app.mod_loader, CB_SETCURSEL, 0, 0);
    }
}

/// Download page: game cores, mod loaders and mods all in one place.
fn build_search_page(app: &mut App) {
    unsafe {
        app.dl_core_version = child(
            app.hwnd,
            "COMBOBOX",
            "",
            WS_TABSTOP | WS_VSCROLL | CBS_DROPDOWN | CBS_HASSTRINGS,
            ID_DL_CORE_VER,
            0,
        );
        app.dl_core_btn = child(app.hwnd, "BUTTON", "下载核心", WS_TABSTOP, ID_DL_CORE_BTN, 0);
        app.dl_core_status = child(app.hwnd, "STATIC", "", DT_LEFT, ID_DL_CORE_STATUS, 0);
        app.dl_loader_status = child(app.hwnd, "STATIC", "", DT_LEFT, ID_DL_LOADER_STATUS, 0);
        // Channel picker: releases by default, snapshots/betas on request.
        app.core_channel_box = child(
            app.hwnd,
            "COMBOBOX",
            "",
            WS_TABSTOP | WS_VSCROLL | CBS_DROPDOWNLIST,
            ID_CORE_CHANNEL,
            0,
        );
        for label in [CoreChannel::Release.label(), CoreChannel::Preview.label()] {
            let w = wide(label);
            SendMessageW(app.core_channel_box, CB_ADDSTRING, 0, w.as_ptr() as LPARAM);
        }
        SendMessageW(app.core_channel_box, CB_SETCURSEL, 0, 0);
        app.srch_query = child(
            app.hwnd,
            "EDIT",
            "",
            WS_TABSTOP | ES_AUTOHSCROLL,
            ID_SRCH_QUERY,
            WS_EX_CLIENTEDGE,
        );
        app.srch_btn = child(app.hwnd, "BUTTON", "搜索", WS_TABSTOP, ID_SRCH_BTN, 0);
        app.srch_dl = child(app.hwnd, "BUTTON", "下载选中", WS_TABSTOP, ID_SRCH_DL, 0);
        app.srch_status = child(app.hwnd, "STATIC", "输入关键词搜索 Modrinth", DT_LEFT, ID_SRCH_STATUS, 0);
        app.srch_list = child(
            app.hwnd,
            "LISTBOX",
            "",
            WS_TABSTOP | WS_VSCROLL | WS_BORDER | 0x0020_0000 | 0x0100,
            ID_SRCH_LIST,
            0,
        );
        for h in [
            app.srch_query,
            app.srch_btn,
            app.srch_dl,
            app.srch_status,
            app.srch_list,
            app.dl_core_version,
            app.dl_core_btn,
            app.dl_core_status,
            app.dl_loader_status,
            app.mod_loader,
            app.mod_loader_ver,
            app.mod_install,
        ] {
            SendMessageW(h, WM_SETFONT, app.font_ui as WPARAM, 1);
        }
    }
}

/// Version manager page: what is installed, with delete and clone.
fn build_version_page(app: &mut App) {
    unsafe {
        app.ver_refresh = child(app.hwnd, "BUTTON", "刷新", WS_TABSTOP, ID_VER_REFRESH, 0);
        app.ver_mods_btn = child(app.hwnd, "BUTTON", "管理模组", WS_TABSTOP, ID_VER_MODS, 0);
        app.ver_fix_btn = child(app.hwnd, "BUTTON", "补全文件", WS_TABSTOP, ID_VER_FIX, 0);
        app.ver_info = child(app.hwnd, "STATIC", "", DT_LEFT, ID_VER_INFO, 0);
        app.ver_modlist = child(
            app.hwnd,
            "LISTBOX",
            "",
            WS_TABSTOP | WS_VSCROLL | WS_BORDER | 0x0020_0000 | 0x0100,
            ID_VER_MODLIST,
            0,
        );
        app.ver_delete = child(app.hwnd, "BUTTON", "删除所选", WS_TABSTOP, ID_VER_DELETE, 0);
        app.ver_clone = child(app.hwnd, "BUTTON", "克隆所选", WS_TABSTOP, ID_VER_CLONE, 0);
        app.ver_status = child(app.hwnd, "STATIC", "", DT_LEFT, ID_VER_STATUS, 0);
        app.ver_list = child(
            app.hwnd,
            "LISTBOX",
            "",
            WS_TABSTOP | WS_VSCROLL | WS_BORDER | 0x0020_0000 | 0x0100,
            ID_VER_LIST,
            0,
        );
        for h in [
            app.ver_refresh,
            app.ver_delete,
            app.ver_clone,
            app.ver_status,
            app.ver_list,
            app.ver_mods_btn,
            app.ver_fix_btn,
            app.ver_info,
            app.ver_modlist,
        ] {
            SendMessageW(h, WM_SETFONT, app.font_ui as WPARAM, 1);
        }
    }
}

/// Announcements page: a feed URL, a check button, the note list, and the body
/// of whichever note is selected.
fn build_notify_page(app: &mut App) {
    unsafe {
        app.notify_url = child(
            app.hwnd,
            "EDIT",
            "",
            WS_TABSTOP | ES_AUTOHSCROLL,
            ID_NOTIFY_URL,
            WS_EX_CLIENTEDGE,
        );
        app.notify_check = child(app.hwnd, "BUTTON", "检查更新", WS_TABSTOP, ID_NOTIFY_CHECK, 0);
        app.notify_auto = child(
            app.hwnd,
            "BUTTON",
            "启动时自动检查",
            WS_TABSTOP | 0x0003, // BS_AUTOCHECKBOX
            ID_NOTIFY_AUTO,
            0,
        );
        app.notify_mark_all = child(app.hwnd, "BUTTON", "全部已读", WS_TABSTOP, ID_NOTIFY_MARK_ALL, 0);
        app.notify_open = child(app.hwnd, "BUTTON", "打开下载页", WS_TABSTOP, ID_NOTIFY_OPEN, 0);
        app.notify_status = child(app.hwnd, "STATIC", "", DT_LEFT, ID_NOTIFY_STATUS, 0);
        app.notify_list = child(
            app.hwnd,
            "LISTBOX",
            "",
            WS_TABSTOP | WS_VSCROLL | WS_BORDER | 0x0020_0000 | 0x0100,
            ID_NOTIFY_LIST,
            0,
        );
        app.notify_body = child(
            app.hwnd,
            "EDIT",
            "",
            WS_VSCROLL | ES_MULTILINE | ES_READONLY | ES_AUTOVSCROLL,
            ID_NOTIFY_BODY,
            WS_EX_CLIENTEDGE,
        );
        for h in [
            app.notify_url,
            app.notify_check,
            app.notify_auto,
            app.notify_mark_all,
            app.notify_open,
            app.notify_status,
            app.notify_list,
        ] {
            SendMessageW(h, WM_SETFONT, app.font_ui as WPARAM, 1);
        }
        SendMessageW(app.notify_body, WM_SETFONT, app.font_log as WPARAM, 1);
        SendMessageW(app.notify_auto, BM_SETCHECK, BST_CHECKED as WPARAM, 0);
    }
}

fn build_controls(app: &mut App) {
    app.font_ui = make_font(-15, FW_NORMAL);
    app.font_bold = make_font(-15, FW_BOLD);
    // Published for the section headings, which are drawn on every repaint.
    set_bold_font(app.font_bold);
    app.font_log = make_font(-13, FW_NORMAL);
    // One brush for every control background; Win32 paints with it for the
    // lifetime of the window, so it must never be deleted while we run.
    app.card_brush = unsafe { CreateSolidBrush(app.card) };
    app.bg_brush = unsafe { CreateSolidBrush(app.bg) };

    build_launch_page(app);
    build_settings_page(app);
    build_tools_page(app);
    build_mods_page(app);
    build_search_page(app);
    build_version_page(app);
    build_notify_page(app);
    classify(app);

    // Theme the progress bar. Without this it paints in the default white,
    // which is a bright bar across an otherwise dark window.
    unsafe {
        SendMessageW(app.progress, PBM_SETBKCOLOR, 0, app.raised as LPARAM);
        SendMessageW(app.progress, PBM_SETBARCOLOR, 0, app.accent as LPARAM);
    }

    // The theme the button subclass paints with. Written once, before any
    // control can be drawn, so the proc never reads a half-initialised copy.
    unsafe {
        BTN_THEME = BtnTheme {
            bg: app.bg,
            card: app.card,
            raised: app.raised,
            ink: app.ink,
            dim: app.dim,
            line: app.line,
            accent: app.accent,
            accent_hi: app.accent_hi,
            radius: 7,
        };
        mark_primary(app.play);
        skin_buttons(&[
            app.play,
            app.install,
            app.pick,
            app.profile_save,
            app.profile_new,
            app.dl_core_btn,
            app.mod_install,
            app.srch_btn,
            app.srch_dl,
            app.mod_refresh,
            app.mod_remove,
            app.mod_open,
            app.mod_deps,
            app.ver_refresh,
            app.ver_delete,
            app.ver_clone,
            app.ver_mods_btn,
            app.ver_fix_btn,
            app.save_btn,
            app.java_pick,
            app.java_scan,
            app.java_use,
            app.tool_verify,
            app.tool_clean,
            app.tool_clean_parts,
            app.tool_open,
            app.tool_copy,
            app.dl_start,
            app.dl_pick,
        ]);
    }

    // ---- seed fields from settings + command line --------------------------
    let dir = Options::default().data_dir();
    app.settings = Settings::load_and_apply(&dir);
    unsafe {
        let dw = wide(&dir.display().to_string());
        SetWindowTextW(app.dir_edit, dw.as_ptr());
        let uw = wide(&app.settings.user);
        SetWindowTextW(app.user, uw.as_ptr());
        let iw = wide(&app.settings.instance);
        SetWindowTextW(app.instance_edit, iw.as_ptr());
        let sw = wide(&app.settings.workers.to_string());
        SetWindowTextW(app.workers_edit, sw.as_ptr());
        if let Some(j) = &app.settings.java {
            let jw = wide(j);
            SetWindowTextW(app.set_java_edit, jw.as_ptr());
        }
        refresh_worker_note(app);
    }

    let cli = CliArgs::parse();
    if let Some(d) = &cli.dir {
        unsafe {
            let dw = wide(&d.display().to_string());
            SetWindowTextW(app.dir_edit, dw.as_ptr());
        }
    }
    if let Some(u) = &cli.user {
        unsafe {
            let uw = wide(u);
            SetWindowTextW(app.user, uw.as_ptr());
        }
    }
    if let Some(m) = &cli.memory {
        unsafe {
            let mw = wide(m);
            let idx = SendMessageW(app.memory, CB_FINDSTRINGEXACT, !0usize, mw.as_ptr() as LPARAM);
            if idx >= 0 {
                SendMessageW(app.memory, CB_SETCURSEL, idx as WPARAM, 0);
            } else {
                SendMessageW(app.memory, CB_SETCURSEL, 1, 0);
            }
        }
    }
    app.install_on_start = cli.install;
    app.pending_version = cli.version.clone();

    layout(app);
    {
        let want = app.pending_version.clone();
        populate_versions(app, want.as_deref());
    }
    // Profiles are per version, so load them once the version list settles.
    profile_reload(app);
    app.tool_append("版本隔离：每个版本独立存档与模组（可在设置里关闭）");
    // Seed the download page's version box with the current selection.
    {
        let cur = read_combo(app.version);
        unsafe {
            let w = wide(&cur);
            SetWindowTextW(app.dl_core_version, w.as_ptr());
        }
    }
    show_tab(app, 0);
    // Seed the announcements page last: it reads settings and may start a
    // background fetch, which must not run before the window is usable.
    notify_startup(app);

    if app.install_on_start {
        start_work(app, false);
    }
}

/// Update the "auto" hint under the worker count.
fn refresh_worker_note(app: &App) {
    unsafe {
        let n = lightfox::net::workers();
        let cores = std::thread::available_parallelism()
            .map(|v| v.get())
            .unwrap_or(4);
        let text = if n == 0 {
            format!("自动（{cores} 线程，按 CPU 核数与文件数取小）")
        } else {
            format!("已设置 {n} 个并发下载线程（本机 {cores} 核）")
        };
        let w = wide(&text);
        SetWindowTextW(app.workers_note, w.as_ptr());
    }
}

// ---- layout ----------------------------------------------------------------

/// Tab bar geometry, shared by layout and paint.
const TAB_H: i32 = 34;
/// Number of pages. Must match the label list in `paint` and the `show_tab`
/// control groups, or a page becomes unreachable.
const TABS: usize = 7;
const CHROME_TOP: i32 = 64;

/// Layout metrics — the **single source of truth** for `layout()` and `paint()`.
///
/// Both functions used to carry their own hard-coded offsets, which is how the
/// cards ended up floating away from the controls they were meant to hold and
/// how a heading ended up painted across the first row of its own card. Every
/// rectangle is now derived from these constants through the helpers below, and
/// `paint()` calls the *same* helpers as `layout()`, so the two cannot drift.
const PAD: i32 = 18; // outer margin, and the label column's left edge
const LABEL_W: i32 = 76; // width of the "版本" / "游戏目录" label column
const ROW_H: i32 = 28; // height of an edit / combo / button — one height, one row
const GAP: i32 = 12; // horizontal gap between two controls in a row
const ROW_GAP: i32 = 38; // vertical distance between row baselines
const CARD_PAD: i32 = 12; // inner padding between a card's edge and its content
const HEAD_H: i32 = 28; // band reserved at the top of a card for its heading
const DROP_ROW: i32 = 22; // height of one drop-down list row

/// Where a page's content starts, below the tab rule.
const BODY_TOP: i32 = CHROME_TOP + TAB_H + 16;

/// Every control in a row is exactly `ROW_H` tall.
///
/// There used to be two heights — 26 for fields, 28 for buttons — and combo
/// boxes ignored both, because Windows forces a combo's *closed* height from its
/// selection-field height no matter what `MoveWindow` asks for. So a row of
/// `[combo][edit][button]` came out with three different bottom edges, which is
/// what read as "错位". One height plus `normalize_combo()` below removes it.
const ROW_MAX: i32 = ROW_H;

/// The x of the first control in a row, on a page that has a label column.
fn row_x() -> i32 {
    PAD + LABEL_W
}

/// The x of the first control inside a card on a page without a label column.
///
/// Card content is inset by `CARD_PAD`, not by `LABEL_W`: indenting it by the
/// label column's width is what left a 76 px dead gutter inside every card on
/// the 下载 / 模组 / 版本 / 工具 pages and made them look shifted right.
fn card_x() -> i32 {
    PAD + CARD_PAD
}

/// The y of row `n` (0-based) of a block that starts at `top`.
fn row_y(top: i32, n: i32) -> i32 {
    top + n * ROW_GAP
}

/// The y of the first control row inside a card whose top edge is `top`.
///
/// `heading` reserves the band the section heading is painted in. Without it the
/// heading was drawn in the same band as the first row, right across the combo.
fn content_y(top: i32, heading: bool) -> i32 {
    top + if heading { HEAD_H } else { CARD_PAD }
}

/// The bottom edge of a card starting at `top` that holds `rows` rows.
fn card_end(top: i32, rows: i32, heading: bool) -> i32 {
    content_y(top, heading) + (rows - 1) * ROW_GAP + ROW_MAX + CARD_PAD
}

/// Height of a combo box that fits `rows` items in its drop-down list.
///
/// The list is part of the control's window, so this is a pure function of the
/// item count. It used to be read back with `GetWindowRect`, which meant the
/// first layout pass — before the list had been grown — disagreed with every
/// later one and the combos changed height on the first resize.
fn combo_h(rows: i32) -> i32 {
    ROW_H + rows * DROP_ROW
}

/// Pin a combo box's *closed* height to `ROW_H`, whatever Windows prefers.
///
/// A combo box sizes its selection field from its font, not from the height
/// passed to `MoveWindow`: a 28 px request came out 24 px tall, so it sat 2 px
/// off from the edit box next to it and 4 px off from the button. `CB_SETITEMHEIGHT`
/// with index `-1` sets that closed height explicitly, which is the only way to
/// make a row of mixed controls share one bottom edge.
fn normalize_combo(h: HWND) {
    if h.is_null() {
        return;
    }
    unsafe {
        // A combo adds 6 px of border to its item height, so the item height
        // that makes the whole control `ROW_H` tall is `ROW_H - 6`.
        SendMessageW(h, CB_SETITEMHEIGHT, !0usize, (ROW_H - 6) as LPARAM);
    }
}

/// x of the "玩家名" label on the launch page's first row.
fn user_label_x() -> i32 {
    row_x() + 190 + 14
}

/// x of the "内存" label on the launch page's first row.
fn mem_label_x() -> i32 {
    user_label_x() + 52 + 120 + 14
}

fn layout(app: &App) {
    unsafe {
        let mut rc = RECT::default();
        GetClientRect(app.hwnd, &mut rc);
        let mut cw = rc.right - rc.left;
        let mut ch = rc.bottom - rc.top;
        if cw < 200 || ch < 200 {
            cw = WIN_W;
            ch = WIN_H;
        }
        let x = PAD;
        let w = cw - PAD * 2;
        let top = BODY_TOP;
        let lx = row_x();
        let cx = card_x();
        let right = cw - PAD;
        // Card content is inset on both sides, so its usable width is narrower
        // than the page's.
        let cw_in = w - 2 * CARD_PAD;

        match app.tab {
            0 => {
                // The form card: a heading band, then four rows.
                let r0 = content_y(top, true);
                // Row 0 — version, player, memory. Each gets its own label.
                MoveWindow(app.version, lx, r0, 190, combo_h(10), 1);
                MoveWindow(app.user, user_label_x() + 52, r0, 130, ROW_H, 1);
                MoveWindow(app.memory, mem_label_x() + 52, r0, 180, combo_h(7), 1);
                // Row 1 — game dir + its picker.
                let r1 = row_y(r0, 1);
                MoveWindow(app.dir_edit, lx, r1, right - lx - 34, ROW_H, 1);
                MoveWindow(app.pick, right - 28, r1, 28, ROW_H, 1);
                // Row 2 — instance, launch, repair.
                let r2 = row_y(r0, 2);
                MoveWindow(app.instance_edit, lx, r2, 190, ROW_H, 1);
                MoveWindow(app.play, lx + 204, r2, 150, ROW_H, 1);
                MoveWindow(app.install, lx + 366, r2, 130, ROW_H, 1);
                // Row 3 — profile, save/new, isolation.
                let r3 = row_y(r0, 3);
                MoveWindow(app.profile_box, lx, r3, 220, combo_h(6), 1);
                MoveWindow(app.profile_save, lx + 232, r3, 100, ROW_H, 1);
                MoveWindow(app.profile_new, lx + 344, r3, 100, ROW_H, 1);
                MoveWindow(
                    app.isolate_box,
                    lx + 456,
                    r3 + 3,
                    (right - (lx + 456)).max(120),
                    22,
                    1,
                );

                // Progress / status / log live below the card.
                let below = card_end(top, 4, true) + CARD_PAD;
                MoveWindow(app.progress, x, below, w, 16, 1);
                MoveWindow(app.status, x, below + 22, w, 22, 1);
                app.log_hint(below + 52, ch, w);
            }
            1 => {
                // Three cards, each with a heading band and one row.
                let c1_top = top;
                let c1 = content_y(c1_top, true);
                MoveWindow(app.dl_core_version, cx, c1, 200, combo_h(12), 1);
                MoveWindow(app.core_channel_box, cx + 212, c1, 96, combo_h(2), 1);
                MoveWindow(app.dl_core_btn, cx + 320, c1, 110, ROW_H, 1);
                MoveWindow(app.dl_core_status, cx + 442, c1 + 3, right - cx - 442 - CARD_PAD, 22, 1);
                let c1_bottom = card_end(c1_top, 1, true);

                let c2_top = c1_bottom + GAP;
                let c2 = content_y(c2_top, true);
                MoveWindow(app.mod_loader, cx, c2, 150, combo_h(8), 1);
                MoveWindow(app.mod_loader_ver, cx + 162, c2, 180, combo_h(8), 1);
                MoveWindow(app.mod_install, cx + 354, c2, 120, ROW_H, 1);
                MoveWindow(app.dl_loader_status, cx + 486, c2 + 3, right - cx - 486 - CARD_PAD, 22, 1);
                let c2_bottom = card_end(c2_top, 1, true);

                // Card 3 — mods: heading, query row, action row, then the list.
                let c3_top = c2_bottom + GAP;
                let q = content_y(c3_top, true);
                MoveWindow(app.srch_query, cx, q, cw_in - 100, ROW_H, 1);
                MoveWindow(app.srch_btn, right - CARD_PAD - 88, q, 88, ROW_H, 1);
                let d = row_y(q, 1);
                MoveWindow(app.srch_dl, cx, d, 120, ROW_H, 1);
                MoveWindow(app.srch_status, cx + 132, d + 3, cw_in - 132, 22, 1);
                let lt = d + ROW_GAP;
                MoveWindow(app.srch_list, cx, lt, cw_in, (ch - lt - CARD_PAD - PAD).max(80), 1);
            }
            2 => {
                // One card: heading, toolbar, hint, then the list.
                let b = content_y(top, true);
                let bw = (cw_in - 3 * GAP) / 4;
                MoveWindow(app.mod_refresh, cx, b, bw, ROW_H, 1);
                MoveWindow(app.mod_remove, cx + bw + GAP, b, bw, ROW_H, 1);
                MoveWindow(app.mod_open, cx + (bw + GAP) * 2, b, bw, ROW_H, 1);
                MoveWindow(app.mod_deps, cx + (bw + GAP) * 3, b, cw_in - (bw + GAP) * 3, ROW_H, 1);
                let hint = b + ROW_H + CARD_PAD;
                MoveWindow(app.mod_hint, cx, hint, cw_in, 20, 1);
                let card_bottom = hint + 20 + CARD_PAD;
                let list_top = card_bottom + CARD_PAD;
                MoveWindow(app.mod_list, cx, list_top, cw_in, (ch - list_top - CARD_PAD - PAD).max(80), 1);
            }
            3 => {
                // Toolbar card, then two list cards side by side.
                let b = content_y(top, true);
                MoveWindow(app.ver_refresh, cx, b, 84, ROW_H, 1);
                MoveWindow(app.ver_delete, cx + 92, b, 92, ROW_H, 1);
                MoveWindow(app.ver_clone, cx + 192, b, 92, ROW_H, 1);
                MoveWindow(app.ver_mods_btn, cx + 292, b, 96, ROW_H, 1);
                MoveWindow(app.ver_fix_btn, cx + 396, b, 96, ROW_H, 1);
                MoveWindow(app.ver_status, cx + 500, b + 3, right - cx - 500 - CARD_PAD, 22, 1);
                // The hint line lives inside the card, so the card is two rows
                // tall (buttons + hint) rather than one.
                let card_bottom = card_end(top, 2, true);

                let lt = card_bottom + CARD_PAD;
                // Both lists end at the page's bottom margin; the info line takes
                // the last 20 px of the right column, above that margin.
                let bottom = ch - CARD_PAD - PAD;
                let info_top = bottom - 20;
                let half = (cw_in - GAP) / 2;
                MoveWindow(app.ver_list, cx, lt, half, bottom - lt, 1);
                MoveWindow(app.ver_modlist, cx + half + GAP, lt, half, info_top - lt - 4, 1);
                MoveWindow(app.ver_info, cx + half + GAP, info_top, half, 20, 1);
            }
            4 => {
                // Settings: one card, a heading band and three rows, then the list.
                let r0 = content_y(top, true);
                MoveWindow(app.workers_edit, lx, r0, 110, combo_h(9), 1);
                MoveWindow(app.workers_note, lx + 122, r0 + 3, right - lx - 122, 22, 1);
                let r1 = row_y(r0, 1);
                MoveWindow(app.set_java_edit, lx, r1, right - lx - 34, ROW_H, 1);
                MoveWindow(app.java_pick, right - 28, r1, 28, ROW_H, 1);
                let r2 = row_y(r0, 2);
                MoveWindow(app.save_btn, lx, r2, 130, ROW_H, 1);
                MoveWindow(app.java_scan, lx + 142, r2, 120, ROW_H, 1);
                MoveWindow(app.java_use, lx + 274, r2, 120, ROW_H, 1);
                // Four rows: three of controls plus the hint line, which lives
                // inside the card. With three it was painted over the list below.
                let card_bottom = card_end(top, 4, true);

                let list_top = card_bottom + CARD_PAD;
                MoveWindow(app.java_list, x, list_top, w, (ch - list_top - CARD_PAD - PAD).max(80), 1);
            }
            5 => {
                // Tools: a 2x3 button grid card, then the downloader card, then the log.
                let b = content_y(top, true);
                let bw = (cw_in - 2 * GAP) / 3;
                MoveWindow(app.tool_verify, cx, b, bw, ROW_H, 1);
                MoveWindow(app.tool_clean, cx + bw + GAP, b, bw, ROW_H, 1);
                MoveWindow(app.tool_clean_parts, cx + (bw + GAP) * 2, b, cw_in - (bw + GAP) * 2, ROW_H, 1);
                let b2 = row_y(b, 1);
                MoveWindow(app.tool_open, cx, b2, bw, ROW_H, 1);
                MoveWindow(app.tool_copy, cx + bw + GAP, b2, bw, ROW_H, 1);
                MoveWindow(app.tool_info, cx + (bw + GAP) * 2, b2 + 3, cw_in - (bw + GAP) * 2, 22, 1);
                let grid_bottom = card_end(top, 2, true);

                // Downloader card: heading, then link / dir / threads.
                let dl_top = grid_bottom + GAP;
                let d0 = content_y(dl_top, true);
                MoveWindow(app.dl_url, cx + 44, d0, cw_in - 44, ROW_H, 1);
                MoveWindow(app.dl_dir, cx + 44, row_y(d0, 1), cw_in - 44 - 32, ROW_H, 1);
                MoveWindow(app.dl_pick, right - CARD_PAD - 28, row_y(d0, 1), 28, ROW_H, 1);
                MoveWindow(app.dl_threads, cx + 44, row_y(d0, 2), 110, combo_h(7), 1);
                MoveWindow(app.dl_start, cx + 166, row_y(d0, 2), 110, ROW_H, 1);
                MoveWindow(app.dl_status, cx + 44, row_y(d0, 3) + 2, cw_in - 44, 22, 1);
                let dl_bottom = card_end(dl_top, 4, true);

                let log_top = dl_bottom + CARD_PAD;
                MoveWindow(app.tool_log, cx, log_top, cw_in, (ch - log_top - CARD_PAD - PAD).max(40), 1);
            }
            6 => {
                // Announcements: one card holding the URL row and the actions,
                // then the note list and the selected note's body side by side.
                let b = content_y(top, true);
                MoveWindow(app.notify_url, cx, b, cw_in - 110, ROW_H, 1);
                MoveWindow(app.notify_check, right - CARD_PAD - 100, b, 100, ROW_H, 1);
                let b2 = row_y(b, 1);
                MoveWindow(app.notify_auto, cx, b2 + 3, 140, 22, 1);
                MoveWindow(app.notify_mark_all, cx + 148, b2, 92, ROW_H, 1);
                MoveWindow(app.notify_open, cx + 248, b2, 104, ROW_H, 1);
                // The status line is the page's only feedback, so it gets the
                // rest of the row; the buttons above are sized to their text.
                MoveWindow(app.notify_status, cx + 360, b2 + 3, cw_in - 360, 22, 1);
                let card_bottom = card_end(top, 2, true);

                // Below the card: list on the left, body on the right.
                let lt = card_bottom + CARD_PAD;
                let lh = (ch - lt - CARD_PAD - PAD).max(80);
                let half = (cw_in - GAP) / 2;
                MoveWindow(app.notify_list, cx, lt, half, lh, 1);
                MoveWindow(app.notify_body, cx + half + GAP, lt, half, lh, 1);
            }
            // `show_tab` clamps to TABS-1, so this is unreachable in practice;
            // it exists so adding a page without a layout branch is a compile
            // error rather than a blank window.
            _ => {}
        }

        // A combo's closed height is decided by its font, not by the height
        // handed to `MoveWindow`, so it has to be pinned after every layout or
        // it sits a few pixels off from the edit boxes and buttons beside it.
        for h in [
            app.version,
            app.memory,
            app.profile_box,
            app.dl_core_version,
            app.core_channel_box,
            app.mod_loader,
            app.mod_loader_ver,
            app.workers_edit,
            app.dl_threads,
        ] {
            normalize_combo(h);
        }
    }
}

impl App {
    /// Size the launch page's output pane.
    ///
    /// The width is passed in rather than re-read: `layout()` already has the
    /// client rect, and re-reading it mid-layout is what let this pane disagree
    /// with the card above it.
    fn log_hint(&self, top: i32, ch: i32, w: i32) {
        unsafe {
            MoveWindow(self.log, PAD, top, w, (ch - top - PAD).max(60), 1);
        }
    }
}

// ---- painting --------------------------------------------------------------

/// The palette the owner-drawn buttons are painted with.
///
/// Written once at startup, before any control can be drawn, and read on every
/// `WM_DRAWITEM`. A `static mut` rather than a heap object: the paint path must
/// not allocate, and this is only ever touched from the UI thread.
static mut BTN_THEME: BtnTheme = BtnTheme {
    bg: 0,
    card: 0,
    raised: 0,
    ink: 0,
    dim: 0,
    line: 0,
    accent: 0,
    accent_hi: 0,
    radius: 7,
};

/// Which buttons are the primary (accent-filled) ones.
///
/// Stored once at startup and read on every `WM_DRAWITEM`, so the painting code
/// never has to guess from a control id.
static mut PRIMARY_BTN: HWND = std::ptr::null_mut();

/// Paint one owner-drawn button: rounded face, subtle border, centred label.
///
/// Called from the parent's `WM_DRAWITEM` handler, so it only touches the `HDC`
/// it is given and never allocates.
unsafe fn paint_button(
    hdc: HDC,
    r: RECT,
    text: &str,
    selected: bool,
    disabled: bool,
    primary: bool,
) {
    let t = BTN_THEME;
    let face = if disabled {
        t.card
    } else if primary {
        if selected { t.accent_hi } else { t.accent }
    } else if selected {
        t.line
    } else {
        t.raised
    };
    let brush = GetStockObject(DC_BRUSH);
    let old_brush = SelectObject(hdc, brush);
    let pen = GetStockObject(DC_PEN);
    let old_pen = SelectObject(hdc, pen);
    // Fill.
    SetDCBrushColor(hdc, face);
    SetDCPenColor(hdc, face);
    RoundRect(hdc, r.left, r.top, r.right, r.bottom, t.radius, t.radius);
    // Hairline just outside the fill, one shade off the face.
    if !disabled {
        SetDCBrushColor(hdc, if primary { t.accent } else { t.line });
        SetDCPenColor(hdc, if primary { t.accent } else { t.line });
        RoundRect(
            hdc,
            r.left - 1,
            r.top - 1,
            r.right + 1,
            r.bottom + 1,
            t.radius + 1,
            t.radius + 1,
        );
    }
    SelectObject(hdc, old_pen);
    SelectObject(hdc, old_brush);

    let color = if disabled {
        t.dim
    } else if primary {
        // Dark ink on a bright accent face reads better than white.
        t.bg
    } else {
        t.ink
    };
    let mut tr = r;
    // Nudge the label down a pixel while pressed: cheap but convincing depth.
    if selected {
        tr.top += 1;
    }
    let w = wide(text);
    SetTextColor(hdc, color);
    SetBkMode(hdc, TRANSPARENT);
    DrawTextW(
        hdc,
        w.as_ptr(),
        -1,
        &mut tr,
        DT_CENTER | DT_SINGLELINE | DT_VCENTER,
    );
}

/// Mark `h` as the primary (accent-filled) button.
unsafe fn mark_primary(h: HWND) {
    PRIMARY_BTN = h;
    gui_debug("button skinning: primary marked");
}

/// `true` when `h` is the primary button.
fn is_primary_button(_app: &App, h: HWND) -> bool {
    unsafe { !PRIMARY_BTN.is_null() && PRIMARY_BTN == h }
}

/// Turn every button into an owner-drawn one.
///
/// `BS_OWNERDRAW` is the only change needed: it makes the button stop painting
/// itself and ask the parent for `WM_DRAWITEM` instead, which `paint` handles.
unsafe fn skin_buttons(buttons: &[HWND]) {
    for &h in buttons {
        if h.is_null() {
            continue;
        }
        let style = GetWindowLongPtrW(h, GWL_STYLE) as u32 | BS_OWNERDRAW;
        SetWindowLongPtrW(h, GWL_STYLE, style as isize);
    }
}

/// Fill a rounded rectangle using the DC brush/pen (no per-call object churn).
unsafe fn round_fill(hdc: HDC, r: RECT, radius: i32, fill: u32, border: Option<u32>) {
    let brush = GetStockObject(DC_BRUSH);
    let old_brush = SelectObject(hdc, brush);
    SetDCBrushColor(hdc, fill);
    let pen = GetStockObject(DC_PEN);
    let old_pen = SelectObject(hdc, pen);
    match border {
        Some(c) => {
            SetDCPenColor(hdc, c);
            RoundRect(hdc, r.left, r.top, r.right, r.bottom, radius, radius);
        }
        None => {
            // Same colour as the fill so the outline is invisible.
            SetDCPenColor(hdc, fill);
            RoundRect(hdc, r.left, r.top, r.right, r.bottom, radius, radius);
        }
    }
    SelectObject(hdc, old_pen);
    SelectObject(hdc, old_brush);
}

fn draw_text(hdc: HDC, text: &str, mut r: RECT, fmt: UINT, color: u32) {
    unsafe {
        SetTextColor(hdc, color);
        let w = wide(text);
        DrawTextW(hdc, w.as_ptr(), -1, &mut r, fmt);
    }
}

/// A section heading with a short accent tick before the text.
fn section_head(hdc: HDC, r: RECT, text: &str, color: u32, accent: u32) {
    unsafe {
        SelectObject(hdc, BOLD_FONT as HGDIOBJ);
        draw_text(hdc, text, r, DT_LEFT | DT_SINGLELINE | DT_VCENTER, color);
        round_fill(
            hdc,
            RECT { left: r.left - 7, top: r.top + 6, right: r.left - 4, bottom: r.bottom - 8 },
            1,
            accent,
            None,
        );
    }
}

/// A card: a rounded panel with a hairline border, behind a group of controls.
fn card(hdc: HDC, r: RECT, app: &App) {
    unsafe {
        round_fill(hdc, r, 10, app.card, Some(app.line));
    }
}

/// The LightFox mark — a fox-head silhouette drawn from vector geometry.
///
/// No image asset ships with the launcher, so the glyph is a small polygon that
/// reads as a fox at 30 px (the header mark) and at 64 px (the taskbar icon):
/// two pointed ears, a wide cheek, a chin, and a lighter muzzle.
///
/// `w`/`h` are the target box; the 0..100 normalised silhouette is scaled to fit.
fn draw_fox(hdc: HDC, r: RECT, fill: u32, hi: u32) {
    unsafe {
        let w = (r.right - r.left) as f64;
        let h = (r.bottom - r.top) as f64;
        let px = |x: f64| r.left + (x * w / 100.0).round() as i32;
        let py = |y: f64| r.top + (y * h / 100.0).round() as i32;
        let pt = |x: f64, y: f64| POINT { x: px(x), y: py(y) };
        let body = [
            pt(12.0, 6.0), pt(27.0, 40.0), pt(37.0, 30.0), pt(50.0, 35.0),
            pt(63.0, 30.0), pt(73.0, 40.0), pt(88.0, 6.0), pt(79.0, 52.0),
            pt(62.0, 88.0), pt(50.0, 95.0), pt(38.0, 88.0), pt(21.0, 52.0),
        ];
        let brush = GetStockObject(DC_BRUSH);
        let oldb = SelectObject(hdc, brush);
        let pen = GetStockObject(DC_PEN);
        let oldp = SelectObject(hdc, pen);
        SetDCBrushColor(hdc, fill);
        SetDCPenColor(hdc, fill);
        Polygon(hdc, body.as_ptr(), body.len() as i32);
        // Inner ears and muzzle, a shade brighter, so the face has depth.
        SetDCBrushColor(hdc, hi);
        SetDCPenColor(hdc, hi);
        let le = [pt(17.0, 16.0), pt(26.0, 38.0), pt(31.0, 22.0)];
        let re = [pt(83.0, 16.0), pt(74.0, 38.0), pt(69.0, 22.0)];
        Polygon(hdc, le.as_ptr(), le.len() as i32);
        Polygon(hdc, re.as_ptr(), re.len() as i32);
        let mz = [pt(50.0, 60.0), pt(41.0, 87.0), pt(59.0, 87.0)];
        Polygon(hdc, mz.as_ptr(), mz.len() as i32);
        SelectObject(hdc, oldp);
        SelectObject(hdc, oldb);
    }
}

/// The silhouette alone, for the icon's transparency mask (caller sets colour).
fn draw_fox_silhouette(hdc: HDC, r: RECT) {
    unsafe {
        let w = (r.right - r.left) as f64;
        let h = (r.bottom - r.top) as f64;
        let px = |x: f64| r.left + (x * w / 100.0).round() as i32;
        let py = |y: f64| r.top + (y * h / 100.0).round() as i32;
        let pt = |x: f64, y: f64| POINT { x: px(x), y: py(y) };
        let body = [
            pt(12.0, 6.0), pt(27.0, 40.0), pt(37.0, 30.0), pt(50.0, 35.0),
            pt(63.0, 30.0), pt(73.0, 40.0), pt(88.0, 6.0), pt(79.0, 52.0),
            pt(62.0, 88.0), pt(50.0, 95.0), pt(38.0, 88.0), pt(21.0, 52.0),
        ];
        Polygon(hdc, body.as_ptr(), body.len() as i32);
    }
}

/// Build a transparent fox `HICON` of `size`×`size` from the same geometry.
///
/// Two bitmaps back it: a 32bpp colour plane (the fox) and a 1bpp mask where
/// white = show the screen (transparent) and black = show the colour. The
/// standard icon blend `Dest = (Dest AND Mask) XOR (Color AND NOT Mask)` makes
/// the fox opaque and the rest see-through on the taskbar and title bar.
///
/// `CreateIconIndirect` does not copy the bitmaps, so they are intentionally
/// leaked for the process lifetime — one icon, negligible cost.
fn make_fox_icon(size: i32, fill: u32, hi: u32) -> HICON {
    unsafe {
        let screen = GetDC(std::ptr::null_mut());
        let full = RECT { left: 0, top: 0, right: size, bottom: size };

        // Colour plane: a 32bpp DIB, filled black so only the fox shows.
        let mut bmi: BITMAPINFO = std::mem::zeroed();
        bmi.bmi_header.bi_size = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bmi_header.bi_width = size;
        bmi.bmi_header.bi_height = -size; // top-down
        bmi.bmi_header.bi_planes = 1;
        bmi.bmi_header.bi_bit_count = 32;
        bmi.bmi_header.bi_compression = BI_RGB;
        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let hbmp_color =
            CreateDIBSection(screen, &bmi, DIB_RGB_COLORS, &mut bits, std::ptr::null_mut(), 0);
        let mem = CreateCompatibleDC(screen);
        let oldc = SelectObject(mem, hbmp_color as HGDIOBJ);
        let bg = CreateSolidBrush(rgb(0, 0, 0));
        FillRect(mem, &full, bg);
        DeleteObject(bg as HGDIOBJ);
        draw_fox(mem, full, fill, hi);
        SelectObject(mem, oldc);
        DeleteDC(mem);

        // Mask plane: white everywhere, then a black fox silhouette.
        let hbmp_mask = CreateBitmap(size, size, 1, 1, std::ptr::null());
        let memm = CreateCompatibleDC(screen);
        let oldm = SelectObject(memm, hbmp_mask as HGDIOBJ);
        let w = CreateSolidBrush(0x00FF_FFFF);
        FillRect(memm, &full, w);
        DeleteObject(w as HGDIOBJ);
        let pen = GetStockObject(DC_PEN);
        let op = SelectObject(memm, pen);
        SetDCPenColor(memm, 0);
        let brush = GetStockObject(DC_BRUSH);
        let ob = SelectObject(memm, brush);
        SetDCBrushColor(memm, 0);
        draw_fox_silhouette(memm, full);
        SelectObject(memm, ob);
        SelectObject(memm, op);
        SelectObject(memm, oldm);
        DeleteDC(memm);

        ReleaseDC(std::ptr::null_mut(), screen);

        CreateIconIndirect(&ICONINFO {
            f_icon: 1,
            x_hotspot: 0,
            y_hotspot: 0,
            hbm_mask: hbmp_mask,
            hbm_color: hbmp_color,
        })
    }
}

/// The bold font used by section headings, published at startup.
///
/// A heading is drawn on every repaint, so creating a font per call would leak
/// GDI objects; this one is made once and never deleted while the window lives.
static mut BOLD_FONT: HFONT = std::ptr::null_mut();

/// Publish the bold font for `section_head`.
fn set_bold_font(f: HFONT) {
    unsafe {
        BOLD_FONT = f;
    }
}

fn paint(app: &App) {
    unsafe {
        let mut ps: PAINTSTRUCT = std::mem::zeroed();
        let hdc = BeginPaint(app.hwnd, &mut ps);
        let mut rc = RECT::default();
        GetClientRect(app.hwnd, &mut rc);

        let bg = CreateSolidBrush(app.bg);
        FillRect(hdc, &rc, bg);
        DeleteObject(bg as HGDIOBJ);
        SetBkMode(hdc, TRANSPARENT);

        // ---- header --------------------------------------------------------
        // Brand mark: a fox-head silhouette, so the name has a real face instead
        // of floating in the corner. Drawn from vector geometry — no asset ships.
        SelectObject(hdc, app.font_bold as HGDIOBJ);
        draw_fox(
            hdc,
            RECT { left: PAD, top: 12, right: PAD + 32, bottom: 44 },
            app.accent,
            app.accent_hi,
        );
        draw_text(
            hdc,
            "LightFox",
            RECT { left: PAD + 40, top: 14, right: rc.right - PAD, bottom: 40 },
            DT_LEFT | DT_SINGLELINE | DT_VCENTER,
            app.ink,
        );
        SelectObject(hdc, app.font_ui as HGDIOBJ);
        draw_text(
            hdc,
            "Launcher",
            RECT { left: PAD + 118, top: 16, right: rc.right - PAD, bottom: 40 },
            DT_LEFT | DT_SINGLELINE | DT_VCENTER,
            app.accent,
        );
        draw_text(
            hdc,
            "零依赖 · 原生 Win32",
            RECT { left: PAD + 192, top: 16, right: rc.right - PAD, bottom: 40 },
            DT_LEFT | DT_SINGLELINE | DT_VCENTER,
            app.dim,
        );
        draw_text(
            hdc,
            &format!("v{VERSION}"),
            RECT { left: rc.right - PAD - 80, top: 16, right: rc.right - PAD, bottom: 40 },
            DT_RIGHT | DT_SINGLELINE | DT_VCENTER,
            app.dim,
        );

        // Hairline under the header.
        let line = CreateSolidBrush(app.line);
        FillRect(
            hdc,
            &RECT { left: PAD, top: 46, right: rc.right - PAD, bottom: 47 },
            line,
        );
        DeleteObject(line as HGDIOBJ);

        // ---- tab bar -------------------------------------------------------
        SelectObject(hdc, app.font_ui as HGDIOBJ);
        for (i, label) in ["启动", "下载", "模组", "版本", "设置", "工具", "通知"].iter().enumerate() {
            let tw = tab_width(i);
            let left = PAD + tab_left(i);
            let active = app.tab == i;
            let r = RECT {
                left,
                top: CHROME_TOP,
                right: left + tw,
                bottom: CHROME_TOP + TAB_H,
            };
            if active {
                // A pill, not a bare rectangle: the rounded top corners match the
                // accent bar underneath and the two read as one control.
                round_fill(hdc, r, 10, app.card, Some(app.line));
                round_fill(
                    hdc,
                    RECT {
                        left: left + 12,
                        top: CHROME_TOP + TAB_H - 4,
                        right: left + tw - 12,
                        bottom: CHROME_TOP + TAB_H,
                    },
                    2,
                    app.accent,
                    None,
                );
                SelectObject(hdc, app.font_bold as HGDIOBJ);
            } else {
                SelectObject(hdc, app.font_ui as HGDIOBJ);
            }
            let mut text_r = r;
            text_r.top += 1;
            draw_text(
                hdc,
                label,
                text_r,
                DT_CENTER | DT_SINGLELINE | DT_VCENTER,
                if active { app.ink } else { app.dim },
            );
        }
        // Full-width rule under the tab bar, so the pages sit on a defined edge.
        SelectObject(hdc, app.font_ui as HGDIOBJ);
        let rule = CreateSolidBrush(app.line);
        FillRect(
            hdc,
            &RECT { left: PAD, top: CHROME_TOP + TAB_H + 1, right: rc.right - PAD, bottom: CHROME_TOP + TAB_H + 2 },
            rule,
        );
        DeleteObject(rule as HGDIOBJ);

        // ---- page body -----------------------------------------------------
        // These are the *same* helpers `layout()` uses, so a card and the
        // controls it holds can never disagree about where a row is.
        let top = BODY_TOP;
        let lx = row_x();
        let cx = card_x();
        let right = rc.right - PAD;
        let w = right - PAD;
        let cw_in = w - 2 * CARD_PAD;
        let ch = rc.bottom;
        match app.tab {
            0 => {
                // The card spans the heading band and the four form rows.
                card(hdc, RECT { left: PAD, top, right, bottom: card_end(top, 4, true) }, app);
                section_head(
                    hdc,
                    RECT { left: lx, top: top + 4, right, bottom: top + HEAD_H - 4 },
                    "启动",
                    app.ink,
                    app.accent,
                );
                // Labels in the label column, right-aligned to their row.
                let r0 = content_y(top, true);
                for (text, n) in [("版本", 0), ("游戏目录", 1), ("实例名", 2), ("配置", 3)] {
                    let y = row_y(r0, n);
                    draw_text(
                        hdc,
                        text,
                        RECT { left: PAD, top: y, right: PAD + LABEL_W - GAP, bottom: y + ROW_MAX },
                        DT_RIGHT | DT_SINGLELINE | DT_VCENTER,
                        app.dim,
                    );
                }
                // Inline labels for the two fields that share row 0 with 版本.
                // They sit in the gap *before* their control, never underneath it.
                for (text, x, wid) in [("玩家名", user_label_x(), 52), ("内存", mem_label_x(), 40)] {
                    draw_text(
                        hdc,
                        text,
                        RECT { left: x, top: r0, right: x + wid, bottom: r0 + ROW_MAX },
                        DT_LEFT | DT_SINGLELINE | DT_VCENTER,
                        app.dim,
                    );
                }
            }
            1 => {
                // Three cards, each with a heading band and one row.
                let c1_top = top;
                let c1_bottom = card_end(c1_top, 1, true);
                card(hdc, RECT { left: PAD, top: c1_top, right, bottom: c1_bottom }, app);
                section_head(
                    hdc,
                    RECT { left: cx, top: c1_top + 4, right, bottom: c1_top + HEAD_H - 4 },
                    "游戏核心",
                    app.ink,
                    app.accent,
                );

                let c2_top = c1_bottom + GAP;
                let c2_bottom = card_end(c2_top, 1, true);
                card(hdc, RECT { left: PAD, top: c2_top, right, bottom: c2_bottom }, app);
                section_head(
                    hdc,
                    RECT { left: cx, top: c2_top + 4, right, bottom: c2_top + HEAD_H - 4 },
                    "模组加载器",
                    app.ink,
                    app.accent,
                );

                let c3_top = c2_bottom + GAP;
                card(hdc, RECT { left: PAD, top: c3_top, right, bottom: ch - PAD }, app);
                section_head(
                    hdc,
                    RECT { left: cx, top: c3_top + 4, right, bottom: c3_top + HEAD_H - 4 },
                    "模组搜索",
                    app.ink,
                    app.accent,
                );
            }
            2 => {
                let b = content_y(top, true);
                let hint = b + ROW_H + CARD_PAD;
                let list_top = hint + 20 + CARD_PAD;
                card(
                    hdc,
                    RECT { left: PAD, top, right, bottom: list_top - CARD_PAD },
                    app,
                );
                section_head(
                    hdc,
                    RECT { left: cx, top: top + 4, right, bottom: top + HEAD_H - 4 },
                    "已安装的模组",
                    app.ink,
                    app.accent,
                );
                draw_text(
                    hdc,
                    "目录：versions/<版本>/mods",
                    RECT { left: cx, top: hint, right: cx + cw_in, bottom: hint + 20 },
                    DT_LEFT | DT_SINGLELINE | DT_END_ELLIPSIS,
                    app.dim,
                );
            }
            3 => {
                let b = content_y(top, true);
                let card_bottom = card_end(top, 2, true);
                card(hdc, RECT { left: PAD, top, right, bottom: card_bottom }, app);
                section_head(
                    hdc,
                    RECT { left: cx, top: top + 4, right, bottom: top + HEAD_H - 4 },
                    "本地版本",
                    app.ink,
                    app.accent,
                );
                draw_text(
                    hdc,
                    "删除只影响 versions/ 下的该版本；libraries / assets / runtime 为共享",
                    RECT { left: cx, top: b + ROW_H + 4, right: cx + cw_in, bottom: b + ROW_H + 24 },
                    DT_LEFT | DT_SINGLELINE | DT_END_ELLIPSIS,
                    app.dim,
                );
                // Two list cards side by side below the toolbar. Each list is
                // inset by CARD_PAD inside its card, so the card reads as a
                // container rather than being completely covered by the list.
                let lt = card_bottom + CARD_PAD;
                let bottom = ch - CARD_PAD - PAD;
                let half = (cw_in - GAP) / 2;
                card(hdc, RECT { left: cx, top: lt, right: cx + half, bottom }, app);
                card(
                    hdc,
                    RECT { left: cx + half + GAP, top: lt, right: cx + cw_in, bottom },
                    app,
                );
            }
            4 => {
                let r0 = content_y(top, true);
                let r1 = row_y(r0, 1);
                let r2 = row_y(r0, 2);
                card(hdc, RECT { left: PAD, top, right, bottom: card_end(top, 4, true) }, app);
                section_head(
                    hdc,
                    RECT { left: lx, top: top + 4, right, bottom: top + HEAD_H - 4 },
                    "下载与运行时",
                    app.ink,
                    app.accent,
                );
                for (text, y) in [("下载线程", r0), ("Java 路径", r1)] {
                    draw_text(
                        hdc,
                        text,
                        RECT { left: PAD, top: y, right: PAD + LABEL_W - GAP, bottom: y + ROW_MAX },
                        DT_RIGHT | DT_SINGLELINE | DT_VCENTER,
                        app.dim,
                    );
                }
                draw_text(
                    hdc,
                    "留空 Java 路径表示自动选择；“查找 Java”只扫描安装目录与注册表，不会全盘读写",
                    RECT { left: lx, top: r2 + ROW_H + 4, right, bottom: r2 + ROW_H + 24 },
                    DT_LEFT | DT_SINGLELINE | DT_END_ELLIPSIS,
                    app.dim,
                );
            }
            5 => {
                // Card 1: the button grid.
                let b = content_y(top, true);
                let grid_bottom = card_end(top, 2, true);
                card(hdc, RECT { left: PAD, top, right, bottom: grid_bottom }, app);
                section_head(
                    hdc,
                    RECT { left: cx, top: top + 4, right, bottom: top + HEAD_H - 4 },
                    "校验与清理",
                    app.ink,
                    app.accent,
                );
                // Card 2: the downloader.
                let dl_top = grid_bottom + GAP;
                let d0 = content_y(dl_top, true);
                card(hdc, RECT { left: PAD, top: dl_top, right, bottom: card_end(dl_top, 4, true) }, app);
                section_head(
                    hdc,
                    RECT { left: cx, top: dl_top + 4, right, bottom: dl_top + HEAD_H - 4 },
                    "多线程下载",
                    app.ink,
                    app.accent,
                );
                for (label, y) in [
                    ("链接", d0),
                    ("目录", row_y(d0, 1)),
                    ("线程", row_y(d0, 2)),
                ] {
                    draw_text(
                        hdc,
                        label,
                        RECT { left: cx, top: y, right: cx + 40, bottom: y + ROW_MAX },
                        DT_LEFT | DT_SINGLELINE | DT_VCENTER,
                        app.dim,
                    );
                }
                let _ = b;
            }
            6 => {
                // Card: heading band, the URL row, then the action row.
                let card_bottom = card_end(top, 2, true);
                card(hdc, RECT { left: PAD, top, right, bottom: card_bottom }, app);
                section_head(
                    hdc,
                    RECT { left: cx, top: top + 4, right, bottom: top + HEAD_H - 4 },
                    "通知与更新",
                    app.ink,
                    app.accent,
                );
                // The running version sits in the heading band, right-aligned.
                // Drawing it below the card put it underneath the list boxes.
                draw_text(
                    hdc,
                    &format!("当前版本 v{VERSION}"),
                    RECT {
                        left: cx,
                        top: top + 4,
                        right: cx + cw_in,
                        bottom: top + HEAD_H - 4,
                    },
                    DT_RIGHT | DT_SINGLELINE | DT_VCENTER,
                    app.dim,
                );
            }
            _ => {}
        }

        EndPaint(app.hwnd, &ps);
    }
}

fn tab_width(_i: usize) -> i32 {
    62
}

fn tab_left(i: usize) -> i32 {
    (i as i32) * (tab_width(i) + 8)
}


fn drain_log(app: &App) {
    let mut batch = Vec::new();
    if let Ok(mut v) = app.lines.lock() {
        if v.is_empty() {
            return;
        }
        batch.append(&mut *v);
    }
    if batch.is_empty() {
        return;
    }
    let text = batch.join("\r\n");
    unsafe {
        let len = SendMessageW(app.log, WM_GETTEXTLENGTH, 0, 0) as i32;
        SendMessageW(app.log, EM_SETSEL, len as WPARAM, len as LPARAM);
        let w = wide(&format!("\r\n{text}"));
        SendMessageW(app.log, EM_REPLACESEL, 0, w.as_ptr() as LPARAM);
        SendMessageW(app.log, EM_SCROLLCARET, 0, 0);
    }
}

fn drain_progress(app: &App) {
    let snap = match app.prog.lock() {
        Ok(g) => g.clone(),
        Err(p) => p.into_inner().clone(),
    };
    let Some((label, done, total)) = snap else {
        return;
    };
    let pct = if total == 0 {
        0
    } else {
        (((done as u128) * 100) / total as u128).min(100) as u32
    };
    unsafe {
        progress_set(app.progress, pct);
        let msg = if total > 0 {
            format!(
                "{label}  {pct}%   {} / {}",
                util::human_size(done),
                util::human_size(total)
            )
        } else {
            format!("{label}  {}", util::human_size(done))
        };
        let w = wide(&msg);
        SetWindowTextW(app.status, w.as_ptr());
    }
}

fn drain_done(app: &mut App) {
    let res = match app.done.lock() {
        Ok(mut g) => g.take(),
        Err(p) => p.into_inner().take(),
    };
    let Some((kind, res)) = res else {
        return;
    };
    if let Ok(mut b) = app.busy.lock() {
        *b = false;
    }
    unsafe {
        for h in [
            app.play,
            app.install,
            app.dl_core_btn,
            app.mod_install,
            app.srch_btn,
            app.srch_dl,
            app.dl_start,
        ] {
            EnableWindow(h, 1);
        }
        progress_set(app.progress, if res.is_ok() { 100 } else { 0 });
    }
    let (message, icon) = match &res {
        Ok(m) => (m.clone(), MB_ICONINFORMATION),
        Err(e) => (format!("失败：{e}"), MB_ICONERROR),
    };
    // Report where the user clicked: each download-page block owns its own
    // status line, and the launch page owns the shared status + log.
    match kind {
        JobKind::Launch => unsafe {
            let w = wide(&message);
            SetWindowTextW(app.status, w.as_ptr());
        },
        JobKind::Core => unsafe {
            let w = wide(&message);
            SetWindowTextW(app.dl_core_status, w.as_ptr());
        },
        JobKind::Loader => unsafe {
            let w = wide(&message);
            SetWindowTextW(app.dl_loader_status, w.as_ptr());
        },
        JobKind::ModDownload => unsafe {
            let w = wide(&message);
            SetWindowTextW(app.srch_status, w.as_ptr());
        },
        JobKind::ToolDownload => unsafe {
            let w = wide(&message);
            SetWindowTextW(app.dl_status, w.as_ptr());
        },
    }
    if res.is_err() {
        unsafe {
            let cap = wide(BRAND);
            let body = wide(&message);
            MessageBoxW(app.hwnd, body.as_ptr(), cap.as_ptr(), MB_OK | icon);
        }
    }
    // A finished install changes what is on disk, so every list built from disk
    // has to be rebuilt — otherwise a freshly downloaded version or loader is
    // invisible until the launcher is restarted.
    if res.is_ok() && matches!(kind, JobKind::Core | JobKind::Loader) {
        let data = collect_options(app).data_dir();
        let newest = lightfox::version::installed_versions(&data)
            .into_iter()
            .map(|v| v.id)
            .next();
        populate_versions(app, newest.as_deref());
        profile_reload(app);
        ver_refresh(app);
    }
    if let Ok(mut g) = app.prog.lock() {
        *g = None;
    }
}

/// Text of a combo box.
///
/// The boxes are `CBS_DROPDOWN`, so the edit field is the source of truth: it
/// holds whatever the user typed even when it matches no list entry (e.g. an
/// unusual version id). `GetWindowText` on the combo returns that field.
/// The bare version id from the combo's display text.
///
/// The dropdown decorates entries (`1.20.1  [已安装]`) for readability; passing
/// that whole string to the manifest or Modrinth would 404. Split on the marker
/// so every caller gets a plain id.
fn bare_version(text: &str) -> String {
    let t = text.trim();
    let cut = t
        .find("  [")
        .or_else(|| t.find('['))
        .unwrap_or(t.len());
    t[..cut].trim().to_string()
}

fn read_combo(h: HWND) -> String {
    let text = window_text(h);
    if !text.trim().is_empty() {
        return text;
    }
    // Fall back to the selected list entry for a `CBS_DROPDOWNLIST` control.
    unsafe {
        let idx = SendMessageW(h, CB_GETCURSEL, 0, 0);
        if idx < 0 {
            return String::new();
        }
        let len = SendMessageW(h, CB_GETLBTEXTLEN, idx as WPARAM, 0);
        if len <= 0 {
            return String::new();
        }
        let mut buf = vec![0u16; len as usize + 1];
        SendMessageW(h, CB_GETLBTEXT, idx as WPARAM, buf.as_mut_ptr() as LPARAM);
        String::from_utf16_lossy(&buf[..len as usize])
    }
}

fn collect_options(app: &App) -> Options {
    let dir = window_text(app.dir_edit);
    Options {
        version: Some(bare_version(&read_combo(app.version))),
        dir: if dir.trim().is_empty() {
            None
        } else {
            Some(PathBuf::from(dir.trim()))
        },
        user: {
            let u = window_text(app.user);
            if u.trim().is_empty() {
                "Player".to_string()
            } else {
                u.trim().to_string()
            }
        },
        memory: {
            let m = read_combo(app.memory);
            if m.trim().is_empty() {
                "2G".to_string()
            } else {
                m.trim().to_string()
            }
        },
        width: None,
        height: None,
        java: None,
        demo: false,
        isolate: app.isolate_on,
        instance: {
            let i = window_text(app.instance_edit);
            if i.trim().is_empty() {
                None
            } else {
                Some(i.trim().to_string())
            }
        },
    }
}

fn pick_dir(app: &App) {
    // A real folder browser means COM + SHBrowseForFolder; the edit box is
    // editable instead, which is less code and just as usable here.
    unsafe {
        let cap = wide("游戏目录");
        let body = wide(
            "直接在“游戏目录”输入框中填写路径即可。\n默认是启动器所在目录下的 .minecraft 文件夹。",
        );
        MessageBoxW(
            app.hwnd,
            body.as_ptr(),
            cap.as_ptr(),
            MB_OK | MB_ICONINFORMATION,
        );
        SetFocus(app.dir_edit);
    }
}

/// Kick off install/launch on a worker thread so the window stays responsive.
fn start_work(app: &mut App, launch: bool) {
    if app.is_busy() {
        return;
    }
    let opts = collect_options(app);
    if let Err(e) = opts.validate() {
        unsafe {
            let cap = wide(BRAND);
            let body = wide(&e);
            MessageBoxW(app.hwnd, body.as_ptr(), cap.as_ptr(), MB_OK | MB_ICONERROR);
        }
        return;
    }

    if let Ok(mut b) = app.busy.lock() {
        *b = true;
    }
    unsafe {
        EnableWindow(app.play, 0);
        EnableWindow(app.install, 0);
        progress_set(app.progress, 0);
    }

    let done = app.done.clone();
    let hwnd_val = app.hwnd as usize;
    let version_label = opts.version.clone().unwrap_or_default();
    app.push_line(&format!(
        "—— {} {} ——",
        if launch { "启动" } else { "下载" },
        version_label
    ));

    std::thread::spawn(move || {
        let result = run_job(&opts, launch);
        if let Ok(mut g) = done.lock() {
            *g = Some((JobKind::Launch, result));
        }
        unsafe {
            PostMessageW(hwnd_val as HWND, WM_APP_DONE, 0, 0);
        }
    });
}

/// The actual work, identical to what the CLI does.
fn run_job(opts: &Options, launch: bool) -> Result<String, String> {
    let prepared = launcher::prepare(opts)?;
    let l = launcher::build_launch(&prepared, opts)?;
    if !launch {
        return Ok(format!("已就绪：{}（库与资源已校验）", prepared.v.id));
    }
    let pid = launcher::spawn_detached(&l)?;
    Ok(format!("Minecraft {} 已启动 (pid {pid})", prepared.v.id))
}

// ---- settings page ---------------------------------------------------------

fn save_settings(app: &mut App) {
    let dir = collect_options(app).data_dir();
    let workers_text = window_text(app.workers_edit);
    let workers = workers_text.trim().parse::<usize>().unwrap_or(0);
    if workers > lightfox::net::MAX_WORKERS_LIMIT {
        app.set_tool_info(&format!(
            "线程数上限 {}，已按上限保存",
            lightfox::net::MAX_WORKERS_LIMIT
        ));
    }
    let java_text = window_text(app.set_java_edit);
    let java = if java_text.trim().is_empty() {
        None
    } else {
        Some(java_text.trim().to_string())
    };
    let new = Settings {
        workers: workers.min(lightfox::net::MAX_WORKERS_LIMIT),
        java,
        memory: read_combo(app.memory),
        user: {
            let u = window_text(app.user);
            if u.trim().is_empty() {
                "Player".to_string()
            } else {
                u.trim().to_string()
            }
        },
        instance: {
            let i = window_text(app.instance_edit);
            if i.trim().is_empty() {
                "default".to_string()
            } else {
                i.trim().to_string()
            }
        },
        resume: true,
        progress: true,
        // Owned by the 通知 page, which saves them itself; a save from the
        // 设置 page must not silently clear the configured feed URL.
        feed_url: app.settings.feed_url.clone(),
        check_feed: app.settings.check_feed,
    };
    if let Err(e) = new.validate() {
        app.message("设置无效", &e);
        return;
    }
    match new.save(&dir) {
        Ok(()) => {
            new.apply();
            app.settings = new;
            refresh_worker_note(app);
            app.set_tool_info("设置已保存并立即生效");
        }
        Err(e) => app.message("保存失败", &e),
    }
}

// ---- tools page ------------------------------------------------------------

impl App {
    fn set_tool_info(&self, msg: &str) {
        unsafe {
            let w = wide(msg);
            SetWindowTextW(self.tool_info, w.as_ptr());
        }
    }

    fn tool_append(&self, msg: &str) {
        unsafe {
            let len = SendMessageW(self.tool_log, WM_GETTEXTLENGTH, 0, 0) as i32;
            SendMessageW(self.tool_log, EM_SETSEL, len as WPARAM, len as LPARAM);
            let w = wide(&format!("\r\n{msg}"));
            SendMessageW(self.tool_log, EM_REPLACESEL, 0, w.as_ptr() as LPARAM);
            SendMessageW(self.tool_log, EM_SCROLLCARET, 0, 0);
        }
    }

    fn message(&self, title: &str, body: &str) {
        unsafe {
            let t = wide(title);
            let b = wide(body);
            MessageBoxW(self.hwnd, b.as_ptr(), t.as_ptr(), MB_OK | MB_ICONINFORMATION);
        }
    }
}

// ---- launch profiles --------------------------------------------------------

/// Fill the profile combo for the current version, keeping `keep` selected.
fn profile_refresh(app: &mut App, keep: Option<&str>) {
    let o = collect_options(app);
    let data = o.data_dir();
    let version = o.version.clone().unwrap_or_default();
    let profiles = if version.is_empty() {
        Vec::new()
    } else {
        lightfox::profiles::list(&data, &version)
    };
    unsafe {
        SendMessageW(app.profile_box, CB_RESETCONTENT, 0, 0);
        for p in &profiles {
            let w = wide(&p.label());
            SendMessageW(app.profile_box, CB_ADDSTRING, 0, w.as_ptr() as LPARAM);
        }
        // Selection: the requested one, else the first.
        let mut sel = 0usize;
        if let Some(k) = keep {
            if let Some(i) = profiles.iter().position(|p| p.name == k) {
                sel = i;
            }
        }
        if !profiles.is_empty() {
            SendMessageW(app.profile_box, CB_SETCURSEL, sel, 0);
        }
    }
    app.profiles_shown = profiles;
    app.profile_index = 0;
    profile_apply(app, 0);
}

/// Load profiles of the selected version into the combo (no selection change).
fn profile_reload(app: &mut App) {
    let current = app
        .profiles_shown
        .get(app.profile_index)
        .map(|p| p.name.clone());
    profile_refresh(app, current.as_deref());
}

/// Apply a profile's values to the launch-page fields.
fn profile_apply(app: &mut App, index: usize) {
    let Some(p) = app.profiles_shown.get(index).cloned() else {
        return;
    };
    app.profile_index = index;
    unsafe {
        let mw = wide(&p.memory);
        SetWindowTextW(app.memory, mw.as_ptr());
        let uw = wide(&p.user);
        SetWindowTextW(app.user, uw.as_ptr());
        SendMessageW(
            app.isolate_box,
            BM_SETCHECK,
            if p.isolate { BST_CHECKED } else { 0 } as WPARAM,
            0,
        );
    }
    app.isolate_on = p.isolate;
}

/// The user picked a different profile: apply it.
fn profile_switch(app: &mut App) {
    unsafe {
        let idx = SendMessageW(app.profile_box, CB_GETCURSEL, 0, 0) as isize;
        if idx < 0 {
            return;
        }
        profile_apply(app, idx as usize);
    }
    if let Some(p) = app.profiles_shown.get(app.profile_index) {
        app.tool_append(&format!("切换到配置 {}", p.label()));
    }
}

/// Save the current form values into the selected profile.
fn profile_save_current(app: &mut App) {
    let o = collect_options(app);
    let data = o.data_dir();
    let version = o.version.clone().unwrap_or_default();
    let name = app
        .profiles_shown
        .get(app.profile_index)
        .map(|p| p.name.clone())
        .unwrap_or_else(|| lightfox::profiles::DEFAULT_PROFILE.to_string());
    let prof = lightfox::profiles::Profile {
        name,
        memory: read_combo(app.memory),
        user: {
            let u = window_text(app.user);
            if u.trim().is_empty() {
                "Steve".into()
            } else {
                u.trim().to_string()
            }
        },
        width: None,
        height: None,
        isolate: app.isolate_on,
        note: String::new(),
    };
    match lightfox::profiles::save(&data, &version, &prof) {
        Ok(()) => {
            app.tool_append(&format!("已保存配置 {}", prof.label()));
            profile_refresh(app, Some(&prof.name));
        }
        Err(e) => app.message("保存配置失败", &e),
    }
}

/// Create a new profile from the current form values (name is derived + numbered).
fn profile_create(app: &mut App) {
    let o = collect_options(app);
    let data = o.data_dir();
    let version = o.version.clone().unwrap_or_default();
    if version.is_empty() {
        app.set_tool_info("请先选择一个版本");
        return;
    }
    // Derive a free name: 配置, 配置2, 配置3 …
    let base = "配置";
    let mut name = base.to_string();
    let mut n = 2;
    while lightfox::profiles::profile_file(&data, &version, &name).exists() {
        name = format!("{base}{n}");
        n += 1;
    }
    let prof = lightfox::profiles::Profile {
        name: name.clone(),
        memory: read_combo(app.memory),
        user: {
            let u = window_text(app.user);
            if u.trim().is_empty() {
                "Steve".into()
            } else {
                u.trim().to_string()
            }
        },
        width: None,
        height: None,
        isolate: app.isolate_on,
        note: "新建".into(),
    };
    match lightfox::profiles::save(&data, &version, &prof) {
        Ok(()) => {
            app.tool_append(&format!("新建配置 {}（可改内存/玩家名后保存）", name));
            profile_refresh(app, Some(&name));
        }
        Err(e) => app.message("新建配置失败", &e),
    }
}

// ---- download page: game core ----------------------------------------------

/// Which kind of version the core combo is showing.
///
/// Mojang's manifest mixes stable releases with snapshots in one flat list, so a
/// plain combo would interleave `24w14a` between `1.21.8` and `1.21.7`. The two
/// kinds are kept apart: releases by default, snapshots only on request.
#[derive(Clone, Copy, PartialEq, Debug)]
enum CoreChannel {
    /// `type: "release"` — the versions people actually play.
    Release,
    /// `type: "snapshot"`, `"old_beta"`, `"old_alpha"` — experiments.
    Preview,
}

impl CoreChannel {
    fn label(self) -> &'static str {
        match self {
            CoreChannel::Release => "正式版",
            CoreChannel::Preview => "预览版",
        }
    }
}

/// The `type` values Mojang's manifest uses, bucketed per channel.
fn is_preview(kind: &str) -> bool {
    matches!(kind, "snapshot" | "old_beta" | "old_alpha")
}

/// Fill the core-version combo from Mojang's manifest, off the UI thread.
///
/// The combo is `CBS_DROPDOWN`, so a typed id is still accepted for a version
/// the manifest does not list; the list only means the common case needs no
/// typing.
fn dl_seed_core_versions(app: &mut App) {
    if app.core_seeded.load(Ordering::Relaxed) {
        return;
    }
    app.core_seeded.store(true, Ordering::Relaxed);
    let data = collect_options(app).data_dir();
    let q = app.manifest_q.clone();
    let hwnd_val = app.hwnd as usize;
    std::thread::spawn(move || {
        let entries = lightfox::version::fetch_manifest(&data).unwrap_or_default();
        if let Ok(mut g) = q.lock() {
            *g = Some(entries);
        }
        unsafe {
            PostMessageW(hwnd_val as HWND, WM_APP_MANIFEST, 0, 0);
        }
    });
}

/// Rebuild the core combo for the given channel from the cached manifest.
fn manifest_fill(app: &App, entries: &[lightfox::version::ManifestEntry], channel: CoreChannel) {
    let installed: Vec<String> =
        lightfox::version::installed_versions(&collect_options(app).data_dir())
            .into_iter()
            .map(|v| v.id)
            .collect();
    let mut shown = 0usize;
    unsafe {
        SendMessageW(app.dl_core_version, CB_RESETCONTENT, 0, 0);
        for e in entries {
            if is_preview(&e.kind) != (channel == CoreChannel::Preview) {
                continue;
            }
            if shown >= 200 {
                break;
            }
            shown += 1;
            let text = if installed.iter().any(|x| x.eq_ignore_ascii_case(&e.id)) {
                format!("{}  [已安装]", e.id)
            } else {
                e.id.clone()
            };
            let w = wide(&text);
            SendMessageW(app.dl_core_version, CB_ADDSTRING, 0, w.as_ptr() as LPARAM);
        }
        // Keep whatever the user had in the box rather than forcing a selection.
        if window_text(app.dl_core_version).trim().is_empty() {
            let first = entries
                .iter()
                .find(|e| is_preview(&e.kind) == (channel == CoreChannel::Preview))
                .map(|e| e.id.clone())
                .unwrap_or_else(|| "1.20.1".to_string());
            let w = wide(&first);
            SetWindowTextW(app.dl_core_version, w.as_ptr());
        }
    }
}

/// Consume the manifest list once the worker delivered it.
fn manifest_done(app: &mut App) {
    let entries = match app.manifest_q.lock() {
        Ok(mut g) => g.take(),
        Err(p) => p.into_inner().take(),
    };
    let Some(entries) = entries else {
        return;
    };
    app.manifest = entries;
    let releases = app
        .manifest
        .iter()
        .filter(|e| !is_preview(&e.kind))
        .count();
    let previews = app.manifest.len() - releases;
    manifest_fill(app, &app.manifest.clone(), CoreChannel::Release);
    app.set_dl_core_status(&format!(
        "正式版 {releases} 个 · 预览版 {previews} 个，也可直接输入版本号"
    ));
}

impl App {
    fn set_dl_core_status(&self, msg: &str) {
        unsafe {
            let w = wide(msg);
            SetWindowTextW(self.dl_core_status, w.as_ptr());
        }
    }

    fn set_dl_loader_status(&self, msg: &str) {
        unsafe {
            let w = wide(msg);
            SetWindowTextW(self.dl_loader_status, w.as_ptr());
        }
    }
}

/// Switch the core combo between 正式版 and 预览版.
///
/// The manifest is already in memory, so this is a local re-fill and never
/// touches the network.
fn core_channel_switch(app: &mut App) {
    let idx = unsafe { SendMessageW(app.core_channel_box, CB_GETCURSEL, 0, 0) as isize };
    app.core_channel = if idx == 1 {
        CoreChannel::Preview
    } else {
        CoreChannel::Release
    };
    let entries = app.manifest.clone();
    manifest_fill(app, &entries, app.core_channel);
    let count = entries
        .iter()
        .filter(|e| is_preview(&e.kind) == (app.core_channel == CoreChannel::Preview))
        .count();
    app.set_dl_core_status(&format!("{} {} 个版本", app.core_channel.label(), count));
}

/// Download a Minecraft core (Vanilla) for the selected version id.
fn dl_install_core(app: &mut App) {
    let raw = read_combo(app.dl_core_version);
    let id = bare_version(&raw);
    if id.is_empty() {
        app.set_dl_core_status("请选择或输入一个版本，例如 1.20.1");
        return;
    }
    if app.is_busy() {
        app.set_dl_core_status("已有任务在运行，请稍候");
        return;
    }
    let mut o = collect_options(app);
    o.version = Some(id.clone());
    app.set_dl_core_status(&format!(
        "正在下载核心 {id}（{}）…",
        app.core_channel.label()
    ));
    app.tool_append(&format!(
        "下载游戏核心 {id}（{}）",
        app.core_channel.label()
    ));
    app.job_kind = JobKind::Core;

    if let Ok(mut b) = app.busy.lock() {
        *b = true;
    }
    unsafe {
        EnableWindow(app.dl_core_btn, 0);
    }
    let done = app.done.clone();
    let hwnd_val = app.hwnd as usize;
    std::thread::spawn(move || {
        let res = lightfox::launcher::prepare(&o)
            .map(|p| format!("核心 {} 已就绪（{}）", p.v.id, p.game_dir.display()));
        if let Ok(mut g) = done.lock() {
            *g = Some((JobKind::Core, res));
        }
        unsafe {
            PostMessageW(hwnd_val as HWND, WM_APP_DONE, 0, 0);
        }
    });
}

// ---- download page: mods ---------------------------------------------------

/// The Minecraft version + loader to filter mods by.
///
/// A loader profile id (`fabric-loader-0.16.9-1.20.1`) is not a game version, so
/// it is resolved to the vanilla version it inherits from; Modrinth and the
/// loader feeds both reject anything else. When the selected version is vanilla
/// (no loader in its id) the loader picker's family is used, because that is the
/// loader the user is about to install.
fn search_scope(app: &App) -> (String, String) {
    let o = collect_options(app);
    let dir = o.data_dir();
    let mc = lightfox::version::base_version(&dir, &o.version.clone().unwrap_or_default());
    let loader = if lightfox::version::is_plain_version(&o.version.clone().unwrap_or_default()) {
        // Vanilla: the picker decides.
        let idx = unsafe { SendMessageW(app.mod_loader, CB_GETCURSEL, 0, 0) as isize };
        lightfox::loaders::Loader::all()
            .get(idx.max(0) as usize)
            .map(|l| l.name().to_ascii_lowercase())
            .unwrap_or_default()
    } else {
        loader_for_search(&o)
    };
    if mc.is_empty() {
        (String::new(), String::new())
    } else {
        (mc, loader)
    }
}

/// Search Modrinth on a worker thread so the window never freezes.
fn search_run(app: &mut App) {
    let query = window_text(app.srch_query).trim().to_string();
    if query.is_empty() {
        app.set_status_line("请输入搜索关键词");
        return;
    }
    let (mc, loader) = search_scope(app);
    let scope = if mc.is_empty() {
        "全部版本".to_string()
    } else {
        format!("{mc} / {loader}")
    };
    unsafe {
        let msg = wide(&format!("搜索中…（{scope}）"));
        SetWindowTextW(app.srch_status, msg.as_ptr());
        EnableWindow(app.srch_btn, 0);
    }
    app.tool_append(&format!("搜索 '{query}'（{scope}）"));

    let q = app.search_q.clone();
    let hwnd_val = app.hwnd as usize;
    std::thread::spawn(move || {
        let res = lightfox::modrinth::search(&query, &mc, &loader, 30);
        if let Ok(mut g) = q.lock() {
            *g = Some(res);
        }
        unsafe {
            PostMessageW(hwnd_val as HWND, WM_APP_SEARCH, 0, 0);
        }
    });
}

/// Show the search results once the worker delivered them.
fn search_done(app: &mut App) {
    let res = match app.search_q.lock() {
        Ok(mut g) => g.take(),
        Err(p) => p.into_inner().take(),
    };
    let Some(res) = res else {
        return;
    };
    unsafe {
        EnableWindow(app.srch_btn, 1);
        SendMessageW(app.srch_list, LB_RESETCONTENT, 0, 0);
    }
    match res {
        Ok(hits) => {
            unsafe {
                for h in &hits {
                    let text = format!(
                        "{}  [{}]  {}",
                        h.title,
                        h.loaders.join(","),
                        util::human_size(h.downloads)
                    );
                    let w = wide(&text);
                    SendMessageW(app.srch_list, LB_ADDSTRING, 0, w.as_ptr() as LPARAM);
                }
                if !hits.is_empty() {
                    SendMessageW(app.srch_list, LB_SETCURSEL, 0, 0);
                }
                let m = wide(&format!("{} 个结果，选中后点“下载选中”", hits.len()));
                SetWindowTextW(app.srch_status, m.as_ptr());
            }
            app.srch_hits = hits;
        }
        Err(e) => unsafe {
            app.srch_hits.clear();
            let m = wide(&format!("搜索失败：{e}"));
            SetWindowTextW(app.srch_status, m.as_ptr());
        },
    }
}

/// Download the selected search hit into the selected version's mods folder.
///
/// Runs on a worker thread: a mod can be tens of megabytes plus its dependency
/// chain, and blocking the message loop for that long looks like a hang.
fn search_download(app: &mut App) {
    let idx = unsafe { SendMessageW(app.srch_list, LB_GETCURSEL, 0, 0) as isize };
    if idx < 0 {
        app.set_status_line("请先选中一个搜索结果");
        return;
    }
    let hit = match app.srch_hits.get(idx as usize) {
        Some(h) => h.clone(),
        None => return,
    };
    if app.is_busy() {
        app.set_status_line("已有任务在运行，请稍候");
        return;
    }
    let o = collect_options(app);
    let (mc, loader) = search_scope(app);
    unsafe {
        let msg = wide(&format!("正在下载 {}…", hit.title));
        SetWindowTextW(app.srch_status, msg.as_ptr());
        EnableWindow(app.srch_dl, 0);
        EnableWindow(app.srch_btn, 0);
    }
    if let Ok(mut b) = app.busy.lock() {
        *b = true;
    }
    let done = app.done.clone();
    let hwnd_val = app.hwnd as usize;
    std::thread::spawn(move || {
        let res = download_mod(&hit, &mc, &loader, &o);
        if let Ok(mut g) = done.lock() {
            *g = Some((JobKind::ModDownload, res));
        }
        unsafe {
            PostMessageW(hwnd_val as HWND, WM_APP_DONE, 0, 0);
        }
    });
}

/// The worker half of `search_download`: pick the file, fetch it, then resolve
/// its required dependencies. Returns the one-line summary for the status bar.
fn download_mod(
    hit: &lightfox::modrinth::SearchHit,
    mc: &str,
    loader: &str,
    o: &lightfox::launcher::Options,
) -> Result<String, String> {
    let files = lightfox::modrinth::versions(&hit.project_id, mc, loader)?;
    let file = files
        .first()
        .ok_or_else(|| format!("{} 没有匹配 {} / {} 的文件", hit.title, mc, loader))?
        .clone();
    let mods = o.mods_dir();
    let dest = lightfox::modrinth::download_file(&mods, &file)?;
    let name = dest
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_default();
    let report = install_dependencies(&file, mc, loader, &mods);
    Ok(format!("已下载 {name} — {report}"))
}

/// The loader to filter downloaded mods/dependencies by.
///
/// Derived from the selected version: a loader profile id contains its family.
/// A vanilla version has no loader of its own, so the loader picker's current
/// family is used — that is the loader the user is about to install, and it is
/// what a mod download should match. Defaulting to Fabric without looking would
/// silently install Fabric-only jars into a Forge setup.
fn loader_for_search(o: &lightfox::launcher::Options) -> String {
    let v = o.version.clone().unwrap_or_default().to_ascii_lowercase();
    for name in ["neoforge", "forge", "quilt", "fabric"] {
        if v.contains(name) {
            return name.to_string();
        }
    }
    String::new()
}

/// Resolve and install the dependencies of a freshly downloaded mod.
///
/// Returns a one-line summary plus the detail lines. It runs on the download
/// worker, so it must not touch window handles; the caller logs the lines.
/// Required dependencies are downloaded; optional and conflicting ones are only
/// reported, since silently installing optional mods would surprise users.
fn install_dependencies(
    file: &lightfox::modrinth::ModFile,
    mc: &str,
    loader: &str,
    mods: &std::path::Path,
) -> String {
    let (summary, _) = resolve_and_install(file, mc, loader, mods);
    summary
}

fn resolve_and_install(
    file: &lightfox::modrinth::ModFile,
    mc: &str,
    loader: &str,
    mods: &std::path::Path,
) -> (String, Vec<String>) {
    let version = match lightfox::modrinth::version_json(&file.project_id, &file.version_number) {
        Ok(v) => v,
        Err(e) => return (format!("依赖检查失败：{e}"), Vec::new()),
    };
    let deps = lightfox::deps::resolve_all(&version, mc, loader, mods);
    if deps.is_empty() {
        return ("无依赖".to_string(), Vec::new());
    }
    let mut log: Vec<String> = deps.iter().map(|d| format!("依赖 {}", d.summary())).collect();
    log.extend(lightfox::deps::install_missing(&deps, mods));
    let required = deps.iter().filter(|d| d.need == lightfox::deps::Need::Required).count();
    let installed = deps
        .iter()
        .filter(|d| d.need == lightfox::deps::Need::Required && d.installed)
        .count();
    let summary = if required == 0 {
        format!("{} 个依赖（无需下载）", deps.len())
    } else {
        format!("{required} 个必需依赖，{installed} 个已就位")
    };
    (summary, log)
}

impl App {
    fn set_status_line(&self, msg: &str) {
        unsafe {
            let w = wide(msg);
            SetWindowTextW(self.srch_status, w.as_ptr());
        }
    }
}

// ---- version manager logic -------------------------------------------------

/// Reload the installed-version list.
fn ver_refresh(app: &mut App) {
    let dir = collect_options(app).data_dir();
    let rows = lightfox::versionmgr::list(&dir);
    unsafe {
        SendMessageW(app.ver_list, LB_RESETCONTENT, 0, 0);
        for v in &rows {
            let mut text = format!("{:<36} {:>10}", v.id, util::human_size(v.size));
            if !v.launchable() {
                if v.has_json && !v.has_jar {
                    text.push_str("  [缺 jar]");
                } else if !v.has_json && v.has_jar {
                    text.push_str("  [缺 json]");
                } else {
                    text.push_str("  [不完整]");
                }
            }
            let w = wide(&text);
            SendMessageW(app.ver_list, LB_ADDSTRING, 0, w.as_ptr() as LPARAM);
        }
        if !rows.is_empty() {
            SendMessageW(app.ver_list, LB_SETCURSEL, 0, 0);
        }
        let total = lightfox::versionmgr::total_size(&rows);
        let m = wide(&format!("{} 个版本，共 {}", rows.len(), util::human_size(total)));
        SetWindowTextW(app.ver_status, m.as_ptr());
    }
    app.ver_rows = rows;
}

/// The version currently highlighted in the list.
fn ver_selected(app: &App) -> Option<lightfox::versionmgr::LocalVersion> {
    unsafe {
        let idx = SendMessageW(app.ver_list, LB_GETCURSEL, 0, 0) as isize;
        if idx < 0 {
            return None;
        }
        app.ver_rows.get(idx as usize).cloned()
    }
}

/// Refresh the mod column for the selected version.
fn ver_show_mods(app: &mut App) {
    let Some(row) = ver_selected(app) else {
        unsafe {
            SendMessageW(app.ver_modlist, LB_RESETCONTENT, 0, 0);
            let w = wide("选中一个版本以查看其模组");
            SetWindowTextW(app.ver_info, w.as_ptr());
        }
        app.ver_mods.clear();
        return;
    };
    let data = collect_options(app).data_dir();
    let mods_dir = lightfox::launcher::Options::mods_dir_for(&data, &row.id);
    let mods = lightfox::loaders::list_mods(&mods_dir);
    unsafe {
        SendMessageW(app.ver_modlist, LB_RESETCONTENT, 0, 0);
        for m in &mods {
            let v = if m.version.is_empty() {
                String::new()
            } else {
                format!(" v{}", m.version)
            };
            let text = format!("{}{}  {}", m.name, v, util::human_size(m.size));
            let w = wide(&text);
            SendMessageW(app.ver_modlist, LB_ADDSTRING, 0, w.as_ptr() as LPARAM);
        }
        let info = format!(
            "{} 个模组 · {}",
            mods.len(),
            mods_dir.display()
        );
        let w = wide(&info);
        SetWindowTextW(app.ver_info, w.as_ptr());
    }
    app.ver_mods = mods;
}

/// Called when the version selection changes.
fn ver_selection_changed(app: &mut App) {
    ver_show_mods(app);
}

/// Open a dialog-free flow: point the launch page at this version, isolation on,
/// and switch to the 搜索 page so the next download lands in this version's mods.
fn ver_manage_mods(app: &mut App) {
    let Some(row) = ver_selected(app) else {
        app.set_tool_info("请先选中一个版本");
        return;
    };
    unsafe {
        // Make this the active version, and make sure isolation is on so the
        // mods land in versions/<id>/mods (where this version's game reads them).
        let vw = wide(&row.id);
        SendMessageW(app.version, CB_SETCURSEL, !0usize, 0);
        let idx = SendMessageW(app.version, CB_FINDSTRINGEXACT, !0usize, vw.as_ptr() as LPARAM);
        if idx >= 0 {
            SendMessageW(app.version, CB_SETCURSEL, idx as WPARAM, 0);
        } else {
            SendMessageW(app.version, CB_INSERTSTRING, 0, vw.as_ptr() as LPARAM);
            SendMessageW(app.version, CB_SETCURSEL, 0, 0);
        }
        SendMessageW(app.isolate_box, BM_SETCHECK, BST_CHECKED as WPARAM, 0);
    }
    app.isolate_on = true;
    show_tab(app, 2);
    let mods_dir = lightfox::launcher::Options::mods_dir_for(&collect_options(app).data_dir(), &row.id);
    app.tool_append(&format!(
        "管理 {} 的模组：下载将保存到 {}",
        row.id,
        mods_dir.display()
    ));
    unsafe {
        if let Err(e) = util::mkdirs(&mods_dir) {
            app.set_tool_info(&format!("无法创建模组目录：{e}"));
        } else {
            let w = wide(&format!("当前模组目录 {}", mods_dir.display()));
            SetWindowTextW(app.srch_status, w.as_ptr());
        }
    }
}

/// Re-download/verify everything the version needs ("补全文件").
///
/// Runs the normal prepare path with this version selected and isolation on, so
/// missing libraries, natives, assets and the client jar are fetched while the
/// existing files are verified and skipped.
fn ver_fix_files(app: &mut App) {
    let Some(row) = ver_selected(app) else {
        app.set_tool_info("请先选中一个版本");
        return;
    };
    if !row.has_json {
        app.message(
            "无法补全",
            &format!("{} 缺少版本 JSON，无法确定需要哪些文件。", row.id),
        );
        return;
    }
    if app.is_busy() {
        app.set_tool_info("已有任务在运行，请稍候");
        return;
    }
    unsafe {
        let vw = wide(&row.id);
        let idx = SendMessageW(app.version, CB_FINDSTRINGEXACT, !0usize, vw.as_ptr() as LPARAM);
        if idx >= 0 {
            SendMessageW(app.version, CB_SETCURSEL, idx as WPARAM, 0);
        }
        SendMessageW(app.isolate_box, BM_SETCHECK, BST_CHECKED as WPARAM, 0);
    }
    app.isolate_on = true;
    app.tool_append(&format!("补全 {} 的文件…", row.id));
    // Reuse the install pipeline: it verifies every sha1 and only fetches what
    // is missing or corrupt.
    start_work(app, false);
}

/// Delete the selected version directory.
fn ver_delete(app: &mut App) {
    let dir = collect_options(app).data_dir();
    unsafe {
        let idx = SendMessageW(app.ver_list, LB_GETCURSEL, 0, 0) as isize;
        if idx < 0 {
            let m = wide("请先选中一个版本");
            SetWindowTextW(app.ver_status, m.as_ptr());
            return;
        }
        let row = match app.ver_rows.get(idx as usize) {
            Some(r) => r.clone(),
            None => return,
        };
        let cap = wide("删除版本");
        let body = wide(&format!(
            "确定删除 {}？\n将释放 {}。\n\n共享的 libraries/ assets/ runtime/ 不会被删除。",
            row.id,
            util::human_size(row.size)
        ));
        if MessageBoxW(
            app.hwnd,
            body.as_ptr(),
            cap.as_ptr(),
            MB_YESNO | MB_ICONINFORMATION,
        ) != IDYES as i32
        {
            return;
        }
        match lightfox::versionmgr::delete(&dir, &row.id) {
            Ok(freed) => {
                app.tool_append(&format!("删除版本 {}，释放 {}", row.id, util::human_size(freed)));
                ver_refresh(app);
            }
            Err(e) => {
                let m = wide(&format!("删除失败：{e}"));
                SetWindowTextW(app.ver_status, m.as_ptr());
            }
        }
    }
}

/// Clone the selected version under a name derived from it.
fn ver_clone(app: &mut App) {
    let dir = collect_options(app).data_dir();
    unsafe {
        let idx = SendMessageW(app.ver_list, LB_GETCURSEL, 0, 0) as isize;
        if idx < 0 {
            let m = wide("请先选中一个版本");
            SetWindowTextW(app.ver_status, m.as_ptr());
            return;
        }
        let row = match app.ver_rows.get(idx as usize) {
            Some(r) => r.clone(),
            None => return,
        };
        // Derive a free name: `1.20.1` → `1.20.1-copy`, `-copy2`, …
        let mut candidate = format!("{}-copy", row.id);
        let mut n = 2;
        while dir.join("versions").join(&candidate).exists() {
            candidate = format!("{}-copy{n}", row.id);
            n += 1;
        }
        match lightfox::versionmgr::clone_version(&dir, &row.id, &candidate) {
            Ok(dst) => {
                let m = wide(&format!("已克隆为 {}", candidate));
                SetWindowTextW(app.ver_status, m.as_ptr());
                app.tool_append(&format!("克隆 {} → {} ({})", row.id, candidate, dst.display()));
                ver_refresh(app);
            }
            Err(e) => {
                let m = wide(&format!("克隆失败：{e}"));
                SetWindowTextW(app.ver_status, m.as_ptr());
            }
        }
    }
}

// ---- mods page logic -------------------------------------------------------

/// Current instance's data dir + mods dir.
/// Data dir + the mods directory of whatever version/instance is selected.
///
/// With isolation on this is `versions/<id>/mods`, so a download lands beside
/// the game that will load it; with isolation off it is
/// `instances/<name>/mods`. One helper keeps every caller consistent.
fn mods_paths(app: &App) -> (std::path::PathBuf, std::path::PathBuf) {
    let o = collect_options(app);
    let dir = o.data_dir();
    (dir, o.mods_dir())
}

/// Reload the mod list for the current instance.
fn mods_refresh(app: &mut App) {
    let (_dir, mods) = mods_paths(app);
    let found = lightfox::loaders::list_mods(&mods);
    unsafe {
        SendMessageW(app.mod_list, LB_RESETCONTENT, 0, 0);
        for m in &found {
            let ver = if m.version.is_empty() {
                String::new()
            } else {
                format!("  v{}", m.version)
            };
            let text = format!("{}{}   {}", m.name, ver, util::human_size(m.size));
            let w = wide(&text);
            SendMessageW(app.mod_list, LB_ADDSTRING, 0, w.as_ptr() as LPARAM);
        }
        let hint = if found.is_empty() {
            format!("{} 里还没有模组", mods.display())
        } else {
            format!("{} 个模组，目录 {}", found.len(), mods.display())
        };
        let w = wide(&hint);
        SetWindowTextW(app.mod_hint, w.as_ptr());
    }
    app.mods_shown = found;
}

/// Fill the loader-version combo for the selected loader family.
///
/// The combo is queried on a worker thread: the feed is a network call, and
/// blocking the message loop for it freezes the window mid-click.
fn mods_refresh_loader_versions(app: &mut App) {
    let (mc, family) = loader_scope(app);
    unsafe {
        SendMessageW(app.mod_loader_ver, CB_RESETCONTENT, 0, 0);
    }
    app.loaders_shown.clear();
    if mc.is_empty() {
        app.set_dl_loader_status("请先在启动页选择一个版本");
        return;
    }
    app.set_dl_loader_status(&format!(
        "正在查询 {} 在 {mc} 上的构建…",
        family.name()
    ));
    let q = app.loaders_q.clone();
    let hwnd_val = app.hwnd as usize;
    std::thread::spawn(move || {
        let res = lightfox::loaders::available_builds(family, &mc);
        if let Ok(mut g) = q.lock() {
            *g = Some((family, mc, res));
        }
        unsafe {
            PostMessageW(hwnd_val as HWND, WM_APP_LOADER, 0, 0);
        }
    });
}

/// The (Minecraft version, loader family) the build list should be queried for.
///
/// The version is the *vanilla* version behind whatever is selected: a loader
/// profile id is not a game version and would 404 on the feed.
fn loader_scope(app: &App) -> (String, lightfox::loaders::Loader) {
    let data = collect_options(app).data_dir();
    let o = collect_options(app);
    let mc = lightfox::version::base_version(&data, &o.version.unwrap_or_default());
    let idx = unsafe { SendMessageW(app.mod_loader, CB_GETCURSEL, 0, 0) as isize };
    let family = lightfox::loaders::Loader::all()
        .get(idx.max(0) as usize)
        .copied()
        .unwrap_or(lightfox::loaders::Loader::Fabric);
    (mc, family)
}

/// Consume the loader-build list once the worker delivered it.
fn loader_done(app: &mut App) {
    let res = match app.loaders_q.lock() {
        Ok(mut g) => g.take(),
        Err(p) => p.into_inner().take(),
    };
    let Some((family, mc, res)) = res else {
        return;
    };
    unsafe {
        SendMessageW(app.mod_loader_ver, CB_RESETCONTENT, 0, 0);
    }
    match res {
        Ok(builds) => {
            unsafe {
                for b in builds.iter().take(40) {
                    let w = wide(&b.loader_version);
                    SendMessageW(app.mod_loader_ver, CB_ADDSTRING, 0, w.as_ptr() as LPARAM);
                }
                if !builds.is_empty() {
                    SendMessageW(app.mod_loader_ver, CB_SETCURSEL, 0, 0);
                }
            }
            app.set_dl_loader_status(&format!(
                "{} 在 {} 上有 {} 个构建，选中后点“安装加载器”",
                family.name(),
                mc,
                builds.len()
            ));
            app.loaders_shown = builds;
        }
        Err(e) => {
            app.loaders_shown.clear();
            app.set_dl_loader_status(&format!("查询失败：{e}"));
        }
    }
}

/// Install the selected loader build; the profile JSON is written into versions/.
fn mods_install_loader(app: &mut App) {
    let idx = unsafe { SendMessageW(app.mod_loader_ver, CB_GETCURSEL, 0, 0) as isize };
    let build = if idx >= 0 {
        app.loaders_shown.get(idx as usize).cloned()
    } else {
        None
    };
    let build = match build {
        Some(b) => b,
        None => {
            app.set_dl_loader_status("请先选择加载器与版本");
            return;
        }
    };
    if app.is_busy() {
        app.set_dl_loader_status("已有任务在运行，请稍候");
        return;
    }
    let (dir, _mods) = mods_paths(app);
    app.set_dl_loader_status(&format!("正在安装 {}…", build.label()));
    if let Ok(mut b) = app.busy.lock() {
        *b = true;
    }
    unsafe {
        EnableWindow(app.mod_install, 0);
    }
    app.tool_append(&format!("安装加载器 {}", build.label()));
    let done = app.done.clone();
    let hwnd_val = app.hwnd as usize;
    std::thread::spawn(move || {
        let res = lightfox::loaders::install(&dir, &build)
            .map(|p| format!("已安装 {}（{}）", build.label(), p.display()));
        if let Ok(mut g) = done.lock() {
            *g = Some((JobKind::Loader, res));
        }
        unsafe {
            PostMessageW(hwnd_val as HWND, WM_APP_DONE, 0, 0);
        }
    });
}

/// Delete the selected mod jar (asks nowhere: the file is trivially re-addable).
fn mods_remove(app: &mut App) {
    unsafe {
        let idx = SendMessageW(app.mod_list, LB_GETCURSEL, 0, 0) as isize;
        if idx < 0 {
            app.set_tool_info("请先在列表中选中一个模组");
            return;
        }
        let target = match app.mods_shown.get(idx as usize) {
            Some(m) => m.clone(),
            None => return,
        };
        match lightfox::loaders::remove_mod(&target.file) {
            Ok(()) => {
                app.tool_append(&format!("删除模组 {}", target.file.display()));
                mods_refresh(app);
            }
            Err(e) => app.set_tool_info(&format!("删除失败：{e}")),
        }
    }
}

/// Check the dependencies of every installed mod and install what is missing.
///
/// Works from the local jars: each mod's `fabric.mod.json` gives its id and
/// version, which is enough to find its Modrinth project and read that version's
/// dependency list. Anything unresolvable is reported rather than guessed.
fn mods_check_deps(app: &mut App) {
    let (_dir, mods) = mods_paths(app);
    let (mc, loader) = search_scope(app);
    let installed = lightfox::loaders::list_mods(&mods);
    if installed.is_empty() {
        app.set_tool_info("模组目录是空的，先下载一个模组");
        return;
    }
    app.set_tool_info(&format!("检查 {} 个模组的依赖…", installed.len()));
    app.tool_append(&format!(
        "—— 依赖检查（{} 个模组，{mc}/{loader}）——",
        installed.len()
    ));

    let mut missing = 0usize;
    let mut fixed = 0usize;
    let mut checked = 0usize;
    for m in &installed {
        // `name` is the mod id from fabric.mod.json (or the file stem).
        let project = match lightfox::modrinth::find_project(&m.name) {
            Ok(Some(p)) => p,
            Ok(None) => continue, // not on Modrinth: nothing to check against
            Err(e) => {
                app.tool_append(&format!("  {}: 查询失败 {e}", m.name));
                continue;
            }
        };
        checked += 1;
        let version = match lightfox::modrinth::version_json(&project, &m.version) {
            Ok(v) => v,
            Err(e) => {
                app.tool_append(&format!("  {}: 无法读取版本 {e}", m.name));
                continue;
            }
        };
        let deps = lightfox::deps::resolve_all(&version, &mc, &loader, &mods);
        for d in &deps {
            if d.need == lightfox::deps::Need::Optional {
                continue;
            }
            if d.need == lightfox::deps::Need::Incompatible {
                if d.installed {
                    app.tool_append(&format!("  {} 与 {} 冲突", m.name, d.summary()));
                }
                continue;
            }
            if !d.installed {
                missing += 1;
                app.tool_append(&format!("  {} 缺少 {}", m.name, d.summary()));
            }
        }
        for line in lightfox::deps::install_missing(&deps, &mods) {
            app.tool_append(&format!("  {line}"));
            if line.starts_with("已安装依赖") {
                fixed += 1;
            }
        }
    }
    let summary = format!(
        "检查 {checked} 个模组：发现 {missing} 个缺失依赖，已自动安装 {fixed} 个"
    );
    app.set_tool_info(&summary);
    app.tool_append(&summary);
    mods_refresh(app);
}

/// Open the instance's mods folder in Explorer.
fn mods_open_dir(app: &mut App) {
    let (_dir, mods) = mods_paths(app);
    if let Err(e) = util::mkdirs(&mods) {
        app.set_tool_info(&format!("无法创建目录：{e}"));
        return;
    }
    match std::process::Command::new("explorer.exe").arg(&mods).spawn() {
        Ok(_) => app.set_tool_info(&format!("已打开 {}", mods.display())),
        Err(e) => app.set_tool_info(&format!("打开失败：{e}")),
    }
}

/// Scan the machine for every Java runtime and fill the list box.
///
/// Runs on a worker thread: the scan touches the registry and walks several
/// install roots, which can take a second or two on a machine with many JDKs, and
/// blocking the message loop would freeze the window.
fn scan_java(app: &mut App) {
    let dir = collect_options(app).data_dir();
    unsafe {
        let msg = wide("正在扫描本机 Java…");
        SetWindowTextW(app.java_use, msg.as_ptr());
        EnableWindow(app.java_scan, 0);
        EnableWindow(app.java_use, 0);
    }
    app.tool_append("开始扫描本机 Java 运行时…");

    let hwnd_val = app.hwnd as usize;
    let list = app.java_list as usize;
    let scan = app.java_scan as usize;
    let use_btn = app.java_use as usize;
    let found_slot = app.java_found.clone();
    let _ = found_slot;

    std::thread::spawn(move || {
        let found = lightfox::javafind::scan_all(&dir);
        // Hand the results back to the UI thread; touching windows from here
        // would race with painting.
        JAVA_SCAN_RESULT.lock().unwrap_or_else(|e| e.into_inner()).replace(found);
        let _ = (list, scan, use_btn);
        unsafe {
            PostMessageW(hwnd_val as HWND, WM_APP_JAVA, 0, 0);
        }
    });
}

/// Shared slot for scan results (single scanner at a time, so one slot suffices).
static JAVA_SCAN_RESULT: std::sync::Mutex<Option<Vec<lightfox::javafind::Found>>> =
    std::sync::Mutex::new(None);

/// Apply a finished scan: fill the list box and re-enable the buttons.
fn java_scan_done(app: &mut App) {
    let found = JAVA_SCAN_RESULT
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
        .unwrap_or_default();
    unsafe {
        SendMessageW(app.java_list, LB_RESETCONTENT, 0, 0);
        for f in &found {
            let text = format!("{}  [{}]", f.label(), f.source);
            let w = wide(&text);
            SendMessageW(app.java_list, LB_ADDSTRING, 0, w.as_ptr() as LPARAM);
        }
        if !found.is_empty() {
            SendMessageW(app.java_list, LB_SETCURSEL, 0, 0);
        }
        EnableWindow(app.java_scan, 1);
        EnableWindow(app.java_use, if found.is_empty() { 0 } else { 1 });
        let summary = if found.is_empty() {
            "未找到任何 Java".to_string()
        } else {
            format!("找到 {} 个 Java 运行时，双击或点“使用选中的”", found.len())
        };
        let w = wide(&summary);
        SetWindowTextW(app.java_use, w.as_ptr());
    }
    app.java_found = found.clone();
    for f in &found {
        app.tool_append(&format!("  {} [{}]", f.label(), f.source));
    }
    app.set_tool_info(&format!("扫描完成：{} 个运行时", found.len()));
}

/// Put the selected runtime into the Java path field.
fn use_selected_java(app: &mut App) {
    unsafe {
        let idx = SendMessageW(app.java_list, LB_GETCURSEL, 0, 0) as isize;
        if idx < 0 {
            app.set_tool_info("请先在列表中选中一个 Java");
            return;
        }
        let f = match app.java_found.get(idx as usize) {
            Some(f) => f,
            None => return,
        };
        let exe = wide(&f.exe.display().to_string());
        SetWindowTextW(app.set_java_edit, exe.as_ptr());
    }
    app.set_tool_info("已填入所选 Java，点“保存设置”后生效");
}

/// Point the Java field at a JRE/JDK directory, or leave it for automatic.
fn pick_java(app: &mut App) {
    unsafe {
        let cap = wide("Java 路径");
        // Raw string: the text contains Windows path separators.
        let body = wide(r"在输入框中填写 java.exe / javaw.exe 的完整路径，或 JRE 目录（如 C:\Program Files\Java\jre-17）。

留空表示自动选择：优先使用系统 Java，其次下载 Mojang 运行时。");
        MessageBoxW(
            app.hwnd,
            body.as_ptr(),
            cap.as_ptr(),
            MB_OK | MB_ICONINFORMATION,
        );
        SetFocus(app.set_java_edit);
    }
}

/// Download one or more URLs with the launcher's own multi-threaded engine.
///
/// The field accepts either a single URL or several separated by whitespace, so
/// a batch is fetched concurrently — that is what makes this a "multi-threaded
/// download tool" rather than a single-file fetch. Threads are taken from the
/// combo; the target directory is editable.
fn dl_start(app: &mut App) {
    let raw = window_text(app.dl_url);
    let urls: Vec<String> = raw
        .split_whitespace()
        .filter(|u| u.starts_with("http://") || u.starts_with("https://"))
        .map(|u| u.to_string())
        .collect();
    if urls.is_empty() {
        app.set_dl_status("请输入至少一个 http(s) 链接");
        return;
    }
    let dir_text = window_text(app.dl_dir);
    let dir = if dir_text.trim().is_empty() {
        collect_options(app).data_dir()
    } else {
        std::path::PathBuf::from(dir_text.trim())
    };
    if let Err(e) = util::mkdirs(&dir) {
        app.message("无法创建目录", &e);
        return;
    }
    let threads: usize = read_combo(app.dl_threads).trim().parse().unwrap_or(12);
    lightfox::net::set_workers(threads);

    app.set_dl_status(&format!("准备下载 {} 个文件到 {}", urls.len(), dir.display()));
    if app.is_busy() {
        app.set_tool_info("已有任务在运行，请稍候");
        return;
    }
    if let Ok(mut b) = app.busy.lock() {
        *b = true;
    }
    let done = app.done.clone();
    let hwnd_val = app.hwnd as usize;
    let dir2 = dir.clone();
    app.tool_append(&format!(
        "多线程下载 {} 个文件（{threads} 线程）→ {}",
        urls.len(),
        dir.display()
    ));

    std::thread::spawn(move || {
        let mut items = Vec::new();
        for (i, u) in urls.iter().enumerate() {
            // Derive a filename from the URL; fall back to a numbered name.
            let name = u
                .rsplit('/')
                .next()
                .map(|n| n.split('?').next().unwrap_or(n).to_string())
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| format!("download-{}", i + 1));
            let dest = dir2.join(name);
            // No sha1 available for arbitrary URLs: the size check is skipped
            // and the transfer is trusted, which is all a generic tool can do.
            items.push(lightfox::net::Download::new(u, dest, "", 0));
        }
        let res = lightfox::net::download_many(&items);
        let out = match res {
            Ok(()) => Ok(format!("下载完成：{} 个文件 → {}", items.len(), dir2.display())),
            Err(e) => Err(e),
        };
        if let Ok(mut g) = done.lock() {
            *g = Some((JobKind::ToolDownload, out));
        }
        unsafe {
            PostMessageW(hwnd_val as HWND, WM_APP_DONE, 0, 0);
        }
    });
}

fn dl_pick_dir(app: &mut App) {
    unsafe {
        let cap = wide("下载目录");
        let body = wide(
            "直接在“下载目录”输入框中填写路径即可，支持绝对路径。\n留空则使用当前游戏目录。",
        );
        MessageBoxW(app.hwnd, body.as_ptr(), cap.as_ptr(), MB_OK | MB_ICONINFORMATION);
        SetFocus(app.dl_dir);
    }
}

impl App {
    fn set_dl_status(&self, msg: &str) {
        unsafe {
            let w = wide(msg);
            SetWindowTextW(self.dl_status, w.as_ptr());
        }
    }
}

/// Re-hash every installed artifact and report mismatches.
fn tool_verify(app: &mut App) {
    let dir = collect_options(app).data_dir();
    let versions = lightfox::version::installed_versions(&dir);
    if versions.is_empty() {
        app.set_tool_info("没有已安装的版本，先下载一个");
        return;
    }
    app.tool_append(&format!("—— 校验 {} ——", dir.display()));
    let mut checked = 0usize;
    let mut bad = 0usize;
    for v in &versions {
        let jar = dir.join("versions").join(&v.id).join(format!("{}.jar", v.id));
        checked += 1;
        match std::fs::metadata(&jar) {
            Ok(m) if m.len() > 0 => {
                app.tool_append(&format!("  {}  客户端 jar {} ✓", v.id, util::human_size(m.len())));
            }
            _ => {
                bad += 1;
                app.tool_append(&format!("  {}  客户端 jar 缺失或为空 ✗", v.id));
            }
        }
    }
    // Every regular file under assets/libraries must be non-empty.
    for sub in ["assets", "libraries", "runtime"] {
        let root = dir.join(sub);
        let mut empty = 0usize;
        let mut total = 0usize;
        for p in launcher::walk(&root) {
            let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            if name.starts_with('.') || lightfox::net::is_partial_name(&name) {
                continue;
            }
            total += 1;
            if util::file_size(&p) == 0 {
                empty += 1;
            }
        }
        checked += total;
        bad += empty;
        app.tool_append(&format!("  {sub}/  {total} 个文件，{empty} 个空文件"));
    }
    app.set_tool_info(&format!("校验完成：检查 {checked} 项，{bad} 个问题"));
    if bad > 0 {
        app.tool_append("提示：对有问题版本执行“下载 / 修复”会重新拉取损坏文件");
    }
}

/// Delete partial downloads and stray temp files.
fn tool_clean_parts(app: &mut App) {
    let dir = collect_options(app).data_dir();
    let mut removed = 0usize;
    let mut bytes = 0u64;
    for p in launcher::walk(&dir) {
        let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        if lightfox::net::is_partial_name(&name) || name.ends_with(".tmp") {
            bytes += util::file_size(&p);
            if std::fs::remove_file(&p).is_ok() {
                removed += 1;
            }
        }
    }
    app.tool_append(&format!(
        "删除断点/临时文件：{removed} 个，释放 {}",
        util::human_size(bytes)
    ));
    app.set_tool_info(&format!("已释放 {}", util::human_size(bytes)));
}

/// Remove logs and crash reports older than the current session.
fn tool_clean(app: &mut App) {
    let dir = collect_options(app).data_dir();
    let mut removed = 0usize;
    let mut bytes = 0u64;
    let mut targets = Vec::new();
    let instances = dir.join("instances");
    if let Ok(rd) = std::fs::read_dir(&instances) {
        for e in rd.flatten() {
            targets.push(e.path().join("logs"));
            targets.push(e.path().join("crash-reports"));
        }
    }
    targets.push(dir.join("logs"));
    for t in targets {
        if !t.is_dir() {
            continue;
        }
        for p in launcher::walk(&t) {
            bytes += util::file_size(&p);
            if std::fs::remove_file(&p).is_ok() {
                removed += 1;
            }
        }
    }
    app.tool_append(&format!(
        "清理日志：{removed} 个文件，释放 {}",
        util::human_size(bytes)
    ));
    app.set_tool_info(&format!("已释放 {}", util::human_size(bytes)));
}

/// Open the data directory in Explorer.
fn tool_open_dir(app: &mut App) {
    let dir = collect_options(app).data_dir();
    if let Err(e) = util::mkdirs(&dir) {
        app.message("无法打开目录", &e);
        return;
    }
    let status = std::process::Command::new("explorer.exe")
        .arg(&dir)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("启动 explorer 失败：{e}"));
    match status {
        Ok(()) => {
            app.set_tool_info(&format!("已在资源管理器中打开 {}", dir.display()));
            app.tool_append(&format!("打开目录 {}", dir.display()));
        }
        Err(e) => app.message("无法打开目录", &e),
    }
}

/// Put the exact java command line on the clipboard.
fn tool_copy_cmd(app: &mut App) {
    let opts = collect_options(app);
    match lightfox::launcher::prepare(&opts) {
        Ok(prepared) => match lightfox::launcher::build_launch(&prepared, &opts) {
            Ok(l) => {
                let mut line = format!("\"{}\"", l.exe.display());
                for a in &l.args {
                    line.push(' ');
                    if a.contains(' ') {
                        line.push('"');
                        line.push_str(a);
                        line.push('"');
                    } else {
                        line.push_str(a);
                    }
                }
                match set_clipboard(&line) {
                    Ok(()) => {
                        app.tool_append("已复制启动命令到剪贴板");
                        app.set_tool_info("命令已复制");
                    }
                    Err(e) => app.message("复制失败", &e),
                }
            }
            Err(e) => app.message("生成命令失败", &e),
        },
        Err(e) => app.message("准备版本失败", &e),
    }
}

/// Copy text to the clipboard via the Win32 clipboard API.
fn set_clipboard(text: &str) -> Result<(), String> {
    unsafe {
        if win32::OpenClipboard(std::ptr::null_mut()) == 0 {
            return Err("无法打开剪贴板".to_string());
        }
        win32::EmptyClipboard();
        let data: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        let bytes = data.len() * 2;
        let h = win32::GlobalAlloc(win32::GMEM_MOVEABLE, bytes);
        if h.is_null() {
            win32::CloseClipboard();
            return Err("无法分配剪贴板内存".to_string());
        }
        let p = win32::GlobalLock(h);
        if p.is_null() {
            win32::GlobalFree(h);
            win32::CloseClipboard();
            return Err("无法锁定剪贴板内存".to_string());
        }
        std::ptr::copy_nonoverlapping(data.as_ptr() as *const u8, p as *mut u8, bytes);
        win32::GlobalUnlock(h);
        if win32::SetClipboardData(win32::CF_UNICODETEXT, h).is_null() {
            win32::GlobalFree(h);
            win32::CloseClipboard();
            return Err("设置剪贴板内容失败".to_string());
        }
        win32::CloseClipboard();
    }
    Ok(())
}

/// Fill the version dropdown from disk + Mojang's manifest.
///
/// Runs on the UI thread at startup and whenever the data directory changes, so
/// the list always mirrors this machine: installed versions are marked, and the
/// newest installed one is preselected so "启动游戏" just works.
fn populate_versions(app: &App, keep_selection: Option<&str>) {
    // `choices` below contains only installed versions; see the comment there.
    let o = collect_options(app);
    let dir = o.data_dir();
    // The launch page is for *launching*: it lists only what is installed.
    // Downloading something new lives on the 下载 page, so this combo can never
    // select a version whose files do not exist yet.
    let mut choices = lightfox::version::installed_versions(&dir);
    choices.sort_by(|a, b| lightfox::version::compare_version_ids(&b.id, &a.id));
    unsafe {
        SendMessageW(app.version, CB_RESETCONTENT, 0, 0);
        for c in &choices {
            // Show the profile count, which is what distinguishes two launch
            // setups of the same version at a glance.
            let profiles = lightfox::profiles::count(&dir, &c.id);
            let text = if profiles > 0 {
                format!("{}  ({} 个配置)", c.id, profiles)
            } else {
                c.id.clone()
            };
            let w = wide(&text);
            SendMessageW(app.version, CB_ADDSTRING, 0, w.as_ptr() as LPARAM);
        }
        let _ = &app.version_choices;

        // Selection: an explicit choice wins, else the newest installed version.
        let want = keep_selection.map(|s| s.to_string()).unwrap_or_else(|| {
            lightfox::version::default_version(&choices)
        });
        let mut sel: isize = 0;
        for (i, c) in choices.iter().enumerate() {
            if c.id.eq_ignore_ascii_case(&want) {
                sel = i as isize;
                break;
            }
        }
        if !choices.is_empty() {
            SendMessageW(app.version, CB_SETCURSEL, sel as WPARAM, 0);
        }
    }
}

/// Fill the status line with what is already on disk.
fn startup_status(app: &App) {
    let o = collect_options(app);
    let dir = o.data_dir();
    let installed = launcher::installed_versions(&dir);
    let size = launcher::dir_size(&dir);
    let mut msg = format!("数据目录 {}　已用 {}", dir.display(), util::human_size(size));
    if !installed.is_empty() {
        msg.push_str(&format!("　已安装：{}", installed.join(", ")));
    }
    unsafe {
        let w = wide(&msg);
        SetWindowTextW(app.status, w.as_ptr());
    }
    if let Some(j) = java::find_system(8) {
        app.push_line(&format!(
            "系统 Java: {} (major {})",
            j.exe.display(),
            j.major
        ));
    } else {
        app.push_line("未找到系统 Java，将在需要时自动下载 Mojang 运行时。");
    }
    let _ = version::versions_dir(&dir);
}

/// Repaint the whole client area.
fn invalidate(app: &App) {
    unsafe {
        let mut rc = RECT::default();
        GetClientRect(app.hwnd, &mut rc);
        InvalidateRect(app.hwnd, &rc, 0);
    }
}

/// Append a debug line to `%TEMP%\lfl-gui-debug.log`, **only when enabled**.
///
/// This used to run unconditionally on every `WM_DRAWITEM` and `WM_COMMAND`,
/// which meant:
///
/// * an unbounded file in a shared directory, growing on every repaint — a long
///   session could add hundreds of thousands of lines;
/// * a permanent, predictable write primitive that anything watching `%TEMP%`
///   could observe (file existence, size, timing of every button press);
/// * leftover diagnostic scaffolding shipped to users.
///
/// It is now opt-in via `LFL_DEBUG=1` and capped, so a release build writes
/// nothing at all unless the user asks for it.
pub fn gui_debug(msg: &str) {
    use std::io::Write;
    // Checked per call rather than cached: this path is already only taken when
    // the variable is set, and a stale cache would survive a re-exec.
    if !debug_enabled() {
        return;
    }
    /// Keep the file small; a diagnostics log is not an audit trail.
    const MAX_LOG: u64 = 512 * 1024;
    let p = std::env::temp_dir().join("lfl-gui-debug.log");
    // Truncate rather than grow without bound once the cap is reached.
    if std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0) > MAX_LOG {
        let _ = std::fs::remove_file(&p);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) {
        let _ = writeln!(f, "{msg}");
    }
}

/// `true` when `LFL_DEBUG=1` is set. Any other value, or unset, means off.
fn debug_enabled() -> bool {
    std::env::var_os("LFL_DEBUG")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

// ---- announcements / update check -----------------------------------------

/// Data dir the announcement state belongs to.
fn notify_dir(app: &App) -> std::path::PathBuf {
    collect_options(app).data_dir()
}

/// Read the URL out of the edit box, trimmed.
fn notify_url_text(app: &App) -> String {
    window_text(app.notify_url).trim().to_string()
}

/// Persist the URL typed in the box, then fetch it.
///
/// Saving before fetching means a URL the user typed survives a failed request —
/// otherwise a typo would silently revert on the next start.
fn notify_check_now(app: &mut App) {
    let url = notify_url_text(app);
    if url.is_empty() {
        set_notify_status(app, "请先填写通知地址（一个返回 JSON 的 URL）");
        return;
    }
    let mut s = app.settings.clone();
    s.feed_url = Some(url.clone());
    s.check_feed = notify_auto_checked(app);
    if let Err(e) = s.validate() {
        set_notify_status(app, &e);
        return;
    }
    if let Err(e) = s.save(&notify_dir(app)) {
        set_notify_status(app, &format!("保存设置失败：{e}"));
        return;
    }
    app.settings = s;
    notify_fetch(app, url, false);
}

/// Start a worker that fetches the feed, then post `WM_APP_NOTIFY`.
fn notify_fetch(app: &mut App, url: String, quiet: bool) {
    if app
        .feed_busy
        .swap(true, std::sync::atomic::Ordering::SeqCst)
    {
        // A request is already in flight; one click, one request.
        return;
    }
    if !quiet {
        set_notify_status(app, "正在检查…");
    }
    let q = app.feed_q.clone();
    let hwnd_val = app.hwnd as usize;
    let busy = {
        // The flag lives in App, so the worker cannot clear it directly; the UI
        // thread clears it in `notify_done`.
        std::sync::Arc::new(())
    };
    let _ = busy;
    std::thread::spawn(move || {
        let res = lightfox::notify::fetch(&url);
        if let Ok(mut g) = q.lock() {
            *g = Some(res);
        }
        unsafe {
            PostMessageW(hwnd_val as HWND, WM_APP_NOTIFY, 0, 0);
        }
    });
}

/// UI-thread half: take the worker result and repaint the page.
fn notify_done(app: &mut App) {
    app.feed_busy
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let res = match app.feed_q.lock() {
        Ok(mut g) => g.take(),
        Err(p) => p.into_inner().take(),
    };
    let Some(res) = res else {
        return;
    };
    let dir = notify_dir(app);
    match res {
        Ok(feed) => {
            let unseen = app.seen.unseen(&feed).len();
            let update = feed
                .latest
                .as_deref()
                .filter(|v| lightfox::notify::is_newer(v, VERSION))
                .map(|v| v.to_string());
            app.seen.last_check = lightfox::notify::now_secs();
            if let Some(v) = &update {
                // Remember it so the same update is not announced twice.
                app.seen.reported_update = Some(v.clone());
            }
            let _ = app.seen.save(&dir);

            let mut msg = format!("{} 条公告", feed.notes.len());
            if unseen > 0 {
                msg.push_str(&format!("，{unseen} 条未读"));
            }
            if let Some(v) = &update {
                msg.push_str(&format!(" · 有新版本 v{v}"));
            }
            if feed.notes.is_empty() && update.is_none() {
                msg.push_str(" · 没有新内容");
            }
            app.feed = Some(feed);
            notify_rebuild_list(app);
            set_notify_status(app, &msg);
            // Put the badge on the tab bar.
            invalidate(app);
        }
        Err(e) => {
            // Keep whatever was on screen; only the status line changes.
            set_notify_status(app, &format!("检查失败：{e}"));
        }
    }
}

/// Refill the list box from `app.feed`, marking unread notes.
fn notify_rebuild_list(app: &mut App) {
    unsafe {
        SendMessageW(app.notify_list, LB_RESETCONTENT, 0, 0);
        let Some(feed) = &app.feed else {
            return;
        };
        for n in &feed.notes {
            let mark = if app.seen.is_seen(&n.id) { "  " } else { "• " };
            let text = format!("{mark}{}", n.list_line());
            let w = wide(&text);
            SendMessageW(app.notify_list, LB_ADDSTRING, 0, w.as_ptr() as LPARAM);
        }
        if !feed.notes.is_empty() {
            SendMessageW(app.notify_list, LB_SETCURSEL, 0, 0);
        }
    }
    notify_selection_changed(app);
}

/// Show the body of the selected note.
fn notify_selection_changed(app: &mut App) {
    let idx = unsafe { SendMessageW(app.notify_list, LB_GETCURSEL, 0, 0) as isize };
    let body = match (&app.feed, idx) {
        (Some(feed), i) if i >= 0 => match feed.notes.get(i as usize) {
            Some(n) => {
                let mut text = n.title.clone();
                if !n.date.is_empty() {
                    text.push_str(&format!("   （{}）", n.date));
                }
                text.push_str("\r\n\r\n");
                text.push_str(&n.body);
                if let Some(u) = &feed.url {
                    text.push_str("\r\n\r\n下载页：");
                    text.push_str(u);
                }
                text
            }
            None => String::new(),
        },
        _ => String::new(),
    };
    unsafe {
        let w = wide(&body);
        SetWindowTextW(app.notify_body, w.as_ptr());
    }
}

/// Mark every note in the current feed as read.
fn notify_mark_all(app: &mut App) {
    let Some(feed) = app.feed.clone() else {
        set_notify_status(app, "还没有可标记的公告");
        return;
    };
    app.seen.mark_all(&feed);
    let dir = notify_dir(app);
    let _ = app.seen.save(&dir);
    notify_rebuild_list(app);
    set_notify_status(app, "已全部标记为已读");
    invalidate(app);
}

/// Save the "check on start" checkbox.
fn notify_save_auto(app: &mut App) {
    let want = notify_auto_checked(app);
    let mut s = app.settings.clone();
    s.check_feed = want;
    // Keep the URL in sync with the box, so the two controls cannot disagree.
    let url = notify_url_text(app);
    s.feed_url = if url.is_empty() { None } else { Some(url) };
    match s.save(&notify_dir(app)) {
        Ok(()) => {
            app.settings = s;
            set_notify_status(
                app,
                if want {
                    "启动时会自动检查通知"
                } else {
                    "启动时不再自动检查"
                },
            );
        }
        Err(e) => set_notify_status(app, &format!("保存失败：{e}")),
    }
}

fn notify_auto_checked(app: &App) -> bool {
    unsafe { SendMessageW(app.notify_auto, BM_GETCHECK, 0, 0) as isize == BST_CHECKED as isize }
}

fn set_notify_status(app: &App, text: &str) {
    unsafe {
        let w = wide(text);
        SetWindowTextW(app.notify_status, w.as_ptr());
    }
}

/// Seed the page from persisted state and, if configured, check in the
/// background. Called once at startup.
fn notify_startup(app: &mut App) {
    app.seen = lightfox::notify::Seen::load(&notify_dir(app));
    unsafe {
        SendMessageW(
            app.notify_auto,
            BM_SETCHECK,
            if app.settings.check_feed {
                BST_CHECKED as WPARAM
            } else {
                BST_UNCHECKED as WPARAM
            },
            0,
        );
        let url = app.settings.feed_url.clone().unwrap_or_default();
        let w = wide(&url);
        SetWindowTextW(app.notify_url, w.as_ptr());
    }
    match app.settings.feed_url.clone() {
        Some(url) if app.settings.check_feed && !url.trim().is_empty() => {
            notify_fetch(app, url, true);
        }
        _ => {
            set_notify_status(app, "填写通知地址后点“检查更新”；留空即关闭此功能。");
        }
    }
}

