// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025 LightFoxLauncher contributors
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Minimal Win32 / GDI / common-controls bindings used by the GUI.
//!
//! Only the handful of functions the launcher window actually needs. Every
//! declaration matches the documented `windows.h` signature; all strings are
//! UTF-16 and every handle-returning call is checked by the caller.
//!
//! The bindings are deliberately a coherent slice of the documented API rather
//! than only the symbols currently reached: pruning each unused constant would
//! make the module a random-looking subset that is harder to extend correctly,
//! and `strip` removes unreferenced constants from the binary anyway.

#![allow(non_snake_case, non_camel_case_types)]
#![allow(dead_code)]

use std::ffi::c_void;

pub type HWND = *mut c_void;
pub type HINSTANCE = *mut c_void;
pub type HMENU = *mut c_void;
pub type HICON = *mut c_void;
pub type HBRUSH = *mut c_void;
pub type HFONT = *mut c_void;
pub type HDC = *mut c_void;
pub type HBITMAP = *mut c_void;
pub type HGDIOBJ = *mut c_void;
pub type LPVOID = *mut c_void;
pub type WPARAM = usize;
pub type LPARAM = isize;
pub type LRESULT = isize;
pub type UINT = u32;
pub type DWORD = u32;
pub type BOOL = i32;
pub type ATOM = u16;

/// Window procedure signature.
pub type WNDPROC = unsafe extern "system" fn(HWND, UINT, WPARAM, LPARAM) -> LRESULT;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct RECT {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct POINT {
    pub x: i32,
    pub y: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct SIZE {
    pub cx: i32,
    pub cy: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct PAINTSTRUCT {
    pub hdc: HDC,
    pub f_erase: BOOL,
    pub rc_paint: RECT,
    pub f_restore: BOOL,
    pub f_inc_update: BOOL,
    pub reserved: [u8; 32],
}

#[repr(C)]
pub struct WNDCLASSEXW {
    pub cb_size: UINT,
    pub style: UINT,
    pub lpfn_wnd_proc: Option<WNDPROC>,
    pub cb_cls_extra: i32,
    pub cb_wnd_extra: i32,
    pub h_instance: HINSTANCE,
    pub h_icon: HICON,
    pub h_cursor: *mut c_void,
    pub hbr_background: HBRUSH,
    pub lpsz_menu_name: *const u16,
    pub lpsz_class_name: *const u16,
    pub h_icon_sm: HICON,
}

impl Default for WNDCLASSEXW {
    fn default() -> Self {
        WNDCLASSEXW {
            cb_size: std::mem::size_of::<WNDCLASSEXW>() as UINT,
            style: 0,
            lpfn_wnd_proc: None,
            cb_cls_extra: 0,
            cb_wnd_extra: 0,
            h_instance: std::ptr::null_mut(),
            h_icon: std::ptr::null_mut(),
            h_cursor: std::ptr::null_mut(),
            hbr_background: std::ptr::null_mut(),
            lpsz_menu_name: std::ptr::null(),
            lpsz_class_name: std::ptr::null(),
            h_icon_sm: std::ptr::null_mut(),
        }
    }
}

#[repr(C)]
pub struct MSG {
    pub hwnd: HWND,
    pub message: UINT,
    pub w_param: WPARAM,
    pub l_param: LPARAM,
    pub time: DWORD,
    pub pt: POINT,
}

// ---- constants -------------------------------------------------------------

pub const WS_OVERLAPPEDWINDOW: DWORD = 0x00CF_0000;
pub const WS_CHILD: DWORD = 0x4000_0000;
pub const WS_VISIBLE: DWORD = 0x1000_0000;
pub const WS_VSCROLL: DWORD = 0x0020_0000;
pub const WS_TABSTOP: DWORD = 0x0001_0000;
pub const WS_BORDER: DWORD = 0x0080_0000;
pub const WS_EX_CLIENTEDGE: DWORD = 0x0000_0200;
pub const WS_EX_TOOLWINDOW: DWORD = 0x0000_0080;
pub const ES_AUTOHSCROLL: DWORD = 0x0080;
pub const ES_READONLY: DWORD = 0x0800;
pub const ES_MULTILINE: DWORD = 0x0004;
pub const ES_AUTOVSCROLL: DWORD = 0x0040;

pub const CW_USEDEFAULT: i32 = -2147483648;
pub const SW_SHOW: i32 = 5;
pub const SW_SHOWNORMAL: i32 = 1;
pub const SW_MINIMIZE: i32 = 6;
pub const SW_RESTORE: i32 = 9;

pub const CS_HREDRAW: UINT = 0x0002;
pub const CS_VREDRAW: UINT = 0x0001;

pub const COLOR_WINDOW: UINT = 5;
pub const COLOR_WINDOWTEXT: UINT = 8;
pub const COLOR_BTNFACE: UINT = 15;
pub const COLOR_HIGHLIGHT: UINT = 13;

pub const IDC_ARROW: usize = 32512;

pub const WM_CREATE: UINT = 0x0001;
pub const WM_DESTROY: UINT = 0x0002;
pub const WM_SIZE: UINT = 0x0005;
pub const WM_PAINT: UINT = 0x000F;
pub const WM_CLOSE: UINT = 0x0010;
pub const WM_QUIT: UINT = 0x0012;
pub const WM_ERASEBKGND: UINT = 0x0014;
pub const WM_KEYDOWN: UINT = 0x0100;
pub const WM_LBUTTONDOWN: UINT = 0x0201;
pub const WM_LBUTTONUP: UINT = 0x0202;
pub const WM_SETCURSOR: UINT = 0x0020;
pub const WM_GETMINMAXINFO: UINT = 0x0024;
pub const WM_CTLCOLORSTATIC: UINT = 0x0138;
/// Sent before a list box paints, so its background can be themed.
pub const WM_CTLCOLORLISTBOX: UINT = 0x0134;
pub const WM_CTLCOLORBTN: UINT = 0x0135;
pub const WM_CTLCOLOREDIT: UINT = 0x0133;
pub const WM_COMMAND: UINT = 0x0111;
pub const WM_SETFONT: UINT = 0x0030;
pub const WM_TIMER: UINT = 0x0113;
pub const WM_GETTEXT: UINT = 0x000D;
pub const WM_GETTEXTLENGTH: UINT = 0x000E;
pub const WM_SETTEXT: UINT = 0x000C;
pub const WM_APP: UINT = 0x8000;

pub const BST_CHECKED: isize = 1;
/// Unchecked state for `BM_SETCHECK`.
pub const BST_UNCHECKED: isize = 0;
pub const BM_GETCHECK: UINT = 0x00F0;
pub const BM_SETCHECK: UINT = 0x00F1;

pub const LB_ADDSTRING: UINT = 0x0180;
pub const LB_RESETCONTENT: UINT = 0x0184;
pub const LB_SETCURSEL: UINT = 0x0186;
pub const LB_GETCURSEL: UINT = 0x0188;
pub const LB_GETTEXT: UINT = 0x0189;
pub const LB_GETTEXTLEN: UINT = 0x018A;
pub const LB_ERR: isize = -1;
pub const LBN_SELCHANGE: u16 = 1;
pub const BS_AUTOCHECKBOX: DWORD = 0x0003;
/// Owner-drawn: the parent paints it via `WM_DRAWITEM`.
pub const BS_OWNERDRAW: DWORD = 0x000B;
pub const LBS_NOINTEGRALHEIGHT: DWORD = 0x0000_0100;

/// `GWL_STYLE`, read to add `BS_OWNERDRAW` to a button's existing style.
pub const GWL_STYLE: i32 = -16;

pub const CB_ADDSTRING: UINT = 0x0143;
pub const CB_SETCURSEL: UINT = 0x014E;
pub const CB_GETCURSEL: UINT = 0x0147;
pub const CB_GETLBTEXT: UINT = 0x0148;
pub const CB_GETLBTEXTLEN: UINT = 0x0149;
pub const CB_FINDSTRINGEXACT: UINT = 0x0158;
pub const CB_INSERTSTRING: UINT = 0x014A;
pub const CB_LIMITTEXT: UINT = 0x0141;
pub const CB_RESETCONTENT: UINT = 0x014B;
/// Sets the height of a combo box's selection field (index -1) or list items.
pub const CB_SETITEMHEIGHT: UINT = 0x0153;
pub const CB_ERR: isize = -1;

// Combo-box styles. `CBS_DROPDOWNLIST` (0x3) is read-only, which would stop the
// user from typing a version or heap size that is not in the preset list;
// `CBS_DROPDOWN` (0x2) keeps the edit field and still shows the list.
pub const CBS_DROPDOWN: DWORD = 0x0002;
pub const CBS_DROPDOWNLIST: DWORD = 0x0003;
pub const CBS_HASSTRINGS: DWORD = 0x0200;

pub const EM_SETSEL: UINT = 0x00B1;
pub const EM_REPLACESEL: UINT = 0x00C2;
pub const EM_LINESCROLL: UINT = 0x00B6;
pub const EM_GETLINECOUNT: UINT = 0x00BA;
pub const EM_SCROLLCARET: UINT = 0x00B7;

pub const CBN_SELCHANGE: u16 = 1;
pub const BN_CLICKED: u16 = 0;

pub const MB_ICONERROR: UINT = 0x0000_0010;
pub const MB_ICONINFORMATION: UINT = 0x0000_0040;
pub const MB_OK: UINT = 0x0000_0000;
pub const MB_YESNO: UINT = 0x0000_0004;
/// Put the focus and the Enter key on the **second** button (否 in a YESNO box).
/// Used for consent prompts where the safe answer must be the default.
pub const MB_DEFBUTTON2: UINT = 0x0000_0100;
pub const IDYES: isize = 6;

pub const DT_LEFT: UINT = 0x0000;
pub const DT_CENTER: UINT = 0x0001;
pub const DT_RIGHT: UINT = 0x0002;
pub const DT_VCENTER: UINT = 0x0004;
pub const DT_SINGLELINE: UINT = 0x0020;
pub const DT_NOPREFIX: UINT = 0x0800;
pub const DT_END_ELLIPSIS: UINT = 0x8000;
pub const DT_WORDBREAK: UINT = 0x0010;

pub const FW_NORMAL: i32 = 400;
pub const FW_BOLD: i32 = 700;
pub const DEFAULT_CHARSET: DWORD = 1;
pub const CLEARTYPE_QUALITY: u32 = 5;
pub const TRANSPARENT: i32 = 1;
pub const PS_SOLID: i32 = 0;

pub const INFINITE: DWORD = 0xFFFF_FFFF;
pub const PM_REMOVE: UINT = 0x0001;
pub const SM_CXSCREEN: i32 = 0;
pub const SM_CYSCREEN: i32 = 1;
pub const GWLP_USERDATA: i32 = -21;
pub const SWP_NOSIZE: UINT = 0x0001;
pub const SWP_NOMOVE: UINT = 0x0002;
pub const SWP_NOZORDER: UINT = 0x0004;
pub const SWP_NOACTIVATE: UINT = 0x0010;
pub const SWP_SHOWWINDOW: UINT = 0x0040;
pub const SW_HIDE: i32 = 0;

pub const WM_SETICON: UINT = 0x0080;
pub const ICON_BIG: DWORD = 1;
pub const IMAGE_ICON: UINT = 1;
pub const LR_LOADFROMFILE: UINT = 0x0010;
pub const LR_DEFAULTSIZE: UINT = 0x0040;
pub const LR_SHARED: UINT = 0x8000;

/// `CreateDIBSection` colour-table usage: colours are literal RGB, not palette.
pub const DIB_RGB_COLORS: UINT = 0;
/// Uncompressed DIB (`biCompression`).
pub const BI_RGB: u32 = 0;

pub const BD: i32 = 0x0055; // GDI hatch/pen helper is not used, kept for clarity

// ---- user32 / kernel32 -----------------------------------------------------

#[link(name = "user32")]
extern "system" {
    pub fn RegisterClassExW(c: *const WNDCLASSEXW) -> ATOM;
    pub fn CreateWindowExW(
        ex_style: DWORD,
        class_name: *const u16,
        window_name: *const u16,
        style: DWORD,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        parent: HWND,
        menu: HMENU,
        instance: HINSTANCE,
        param: LPVOID,
    ) -> HWND;
    pub fn DefWindowProcW(hwnd: HWND, msg: UINT, w: WPARAM, l: LPARAM) -> LRESULT;
    pub fn ShowWindow(hwnd: HWND, cmd: i32) -> BOOL;
    pub fn UpdateWindow(hwnd: HWND) -> BOOL;
    pub fn DestroyWindow(hwnd: HWND) -> BOOL;
    pub fn PostQuitMessage(code: i32);
    pub fn GetMessageW(msg: *mut MSG, hwnd: HWND, min: UINT, max: UINT) -> BOOL;
    pub fn TranslateMessage(msg: *const MSG) -> BOOL;
    pub fn DispatchMessageW(msg: *const MSG) -> LRESULT;
    pub fn PostMessageW(hwnd: HWND, msg: UINT, w: WPARAM, l: LPARAM) -> BOOL;
    pub fn SendMessageW(hwnd: HWND, msg: UINT, w: WPARAM, l: LPARAM) -> LRESULT;
    pub fn SetWindowTextW(hwnd: HWND, text: *const u16) -> BOOL;
    pub fn GetWindowTextW(hwnd: HWND, buf: *mut u16, max: i32) -> i32;
    pub fn GetWindowTextLengthW(hwnd: HWND) -> i32;
    pub fn GetClientRect(hwnd: HWND, r: *mut RECT) -> BOOL;
    pub fn GetWindowRect(hwnd: HWND, r: *mut RECT) -> BOOL;
    pub fn MoveWindow(hwnd: HWND, x: i32, y: i32, w: i32, h: i32, repaint: BOOL) -> BOOL;
    pub fn SetWindowPos(hwnd: HWND, after: HWND, x: i32, y: i32, cx: i32, cy: i32, flags: UINT)
        -> BOOL;
    pub fn InvalidateRect(hwnd: HWND, r: *const RECT, erase: BOOL) -> BOOL;
    pub fn BeginPaint(hwnd: HWND, ps: *mut PAINTSTRUCT) -> HDC;
    pub fn EndPaint(hwnd: HWND, ps: *const PAINTSTRUCT) -> BOOL;
    pub fn GetDC(hwnd: HWND) -> HDC;
    pub fn ReleaseDC(hwnd: HWND, hdc: HDC) -> i32;
    pub fn FillRect(hdc: HDC, r: *const RECT, brush: HBRUSH) -> i32;
    pub fn DrawTextW(hdc: HDC, text: *const u16, len: i32, r: *mut RECT, fmt: UINT) -> i32;
    pub fn SetTimer(hwnd: HWND, id: usize, elapse: UINT, func: *mut c_void) -> usize;
    pub fn KillTimer(hwnd: HWND, id: usize) -> BOOL;
    pub fn MessageBoxW(hwnd: HWND, text: *const u16, caption: *const u16, kind: UINT) -> i32;
    pub fn LoadCursorW(instance: HINSTANCE, name: usize) -> *mut c_void;
    pub fn LoadIconW(instance: HINSTANCE, name: usize) -> HICON;
    pub fn LoadImageW(instance: HINSTANCE, name: *const u16, kind: UINT, cx: i32, cy: i32, load: UINT)
        -> *mut c_void;
    pub fn SetForegroundWindow(hwnd: HWND) -> BOOL;
    pub fn IsIconic(hwnd: HWND) -> BOOL;
    pub fn GetSystemMetrics(index: i32) -> i32;
    pub fn SetFocus(hwnd: HWND) -> HWND;
    pub fn EnableWindow(hwnd: HWND, enable: BOOL) -> BOOL;
    pub fn GetKeyState(key: i32) -> i16;
    pub fn SetWindowLongPtrW(hwnd: HWND, index: i32, value: isize) -> isize;
    pub fn GetWindowLongPtrW(hwnd: HWND, index: i32) -> isize;
    pub fn AdjustWindowRectEx(r: *mut RECT, style: DWORD, menu: BOOL, ex_style: DWORD) -> BOOL;
}

#[link(name = "kernel32")]
extern "system" {
    pub fn GetModuleHandleW(name: *const u16) -> HINSTANCE;
    pub fn GetLastError() -> DWORD;
    pub fn Sleep(ms: DWORD);
}

// ---- gdi32 -----------------------------------------------------------------

#[link(name = "gdi32")]
extern "system" {
    pub fn GetStockObject(index: i32) -> HGDIOBJ;
    pub fn CreateCompatibleDC(hdc: HDC) -> HDC;
    pub fn DeleteDC(hdc: HDC) -> BOOL;
    pub fn SetRect(r: *mut RECT, l: i32, t: i32, rr: i32, b: i32) -> BOOL;
    pub fn GetTextExtentPoint32W(hdc: HDC, s: *const u16, len: i32, sz: *mut SIZE) -> BOOL;
    pub fn TextOutW(hdc: HDC, x: i32, y: i32, s: *const u16, len: i32) -> BOOL;
}

/// `NULL_BRUSH` stock object: fill a rectangle without painting its inside.
pub const NULL_BRUSH: i32 = 5;
/// `HOLLOW_BRUSH`
pub const HOLLOW_BRUSH: i32 = 4;
/// `DC_BRUSH`, colour set with `SetDCBrushColor`.
pub const DC_BRUSH: i32 = 18;
/// `DC_PEN`
pub const DC_PEN: i32 = 19;

#[link(name = "gdi32")]
extern "system" {
    pub fn SetDCBrushColor(hdc: HDC, color: u32) -> u32;
    pub fn SetDCPenColor(hdc: HDC, color: u32) -> u32;
    pub fn FrameRect(hdc: HDC, r: *const RECT, brush: HBRUSH) -> i32;
}

#[link(name = "gdi32")]
extern "system" {
    pub fn CreateFontW(
        height: i32,
        width: i32,
        escapement: i32,
        orientation: i32,
        weight: i32,
        italic: DWORD,
        underline: DWORD,
        strike_out: DWORD,
        charset: DWORD,
        out_precision: DWORD,
        clip_precision: DWORD,
        quality: u32,
        pitch_and_family: DWORD,
        face: *const u16,
    ) -> HFONT;
    /// A device-independent bitmap the caller can paint into (icon colour plane).
    pub fn CreateCompatibleBitmap(hdc: HDC, w: i32, h: i32) -> HBITMAP;
    /// A 1-bpp monochrome bitmap (icon mask plane).
    pub fn CreateBitmap(w: i32, h: i32, planes: u32, bits: u32, bits_ptr: *const c_void) -> HBITMAP;
    pub fn CreateSolidBrush(color: u32) -> HBRUSH;
    pub fn CreatePen(style: i32, width: i32, color: u32) -> HGDIOBJ;
    pub fn DeleteObject(obj: HGDIOBJ) -> BOOL;
    pub fn SelectObject(hdc: HDC, obj: HGDIOBJ) -> HGDIOBJ;
    pub fn SetTextColor(hdc: HDC, color: u32) -> u32;
    pub fn SetBkColor(hdc: HDC, color: u32) -> u32;
    pub fn SetBkMode(hdc: HDC, mode: i32) -> i32;
    pub fn Rectangle(hdc: HDC, l: i32, t: i32, r: i32, b: i32) -> BOOL;
    pub fn RoundRect(hdc: HDC, l: i32, t: i32, r: i32, b: i32, ew: i32, eh: i32) -> BOOL;
    pub fn MoveToEx(hdc: HDC, x: i32, y: i32, prev: *mut POINT) -> BOOL;
    pub fn LineTo(hdc: HDC, x: i32, y: i32) -> BOOL;
    /// Closed filled polygon from a point array (the fox glyph is built from these).
    pub fn Polygon(hdc: HDC, pts: *const POINT, count: i32) -> BOOL;
    /// Open polyline (outline, not filled).
    pub fn Polyline(hdc: HDC, pts: *const POINT, count: i32) -> BOOL;
    /// Filled ellipse.
    pub fn Ellipse(hdc: HDC, l: i32, t: i32, r: i32, b: i32) -> BOOL;
    /// A DIB section the caller can write pixels into; used to build the icon.
    pub fn CreateDIBSection(
        hdc: HDC,
        info: *const BITMAPINFO,
        usage: UINT,
        bits: *mut *mut c_void,
        section: *mut c_void,
        offset: DWORD,
    ) -> HBITMAP;
    /// Pack a mask+colour bitmap into an `HICON`.
    pub fn CreateIconIndirect(icon: *const ICONINFO) -> HICON;
}

/// Windows colour helpers: `RGB` is just `0x00BBGGRR`.
pub const fn rgb(r: u8, g: u8, b: u8) -> u32 {
    (r as u32) | ((g as u32) << 8) | ((b as u32) << 16)
}

// ---- common controls -------------------------------------------------------

#[link(name = "comctl32")]
extern "system" {
    pub fn InitCommonControlsEx(icc: *const INITCOMMONCONTROLSEX) -> BOOL;
}

#[repr(C)]
pub struct INITCOMMONCONTROLSEX {
    pub dw_size: DWORD,
    pub dw_icc: DWORD,
}

pub const ICC_PROGRESS_CLASS: DWORD = 0x0000_0020;

/// Register the common-control classes the window uses (progress bar).
pub fn init_common_controls() {
    unsafe {
        let icc = INITCOMMONCONTROLSEX {
            dw_size: std::mem::size_of::<INITCOMMONCONTROLSEX>() as DWORD,
            dw_icc: ICC_PROGRESS_CLASS,
        };
        InitCommonControlsEx(&icc);
    }
}

pub const PBM_SETRANGE32: UINT = WM_APP + 6;
pub const PBM_SETPOS: UINT = WM_APP + 2;
pub const PBM_SETSTATE: UINT = WM_APP + 16;
/// Progress-bar trough colour (`CCM_SETBKCOLOR`).
pub const PBM_SETBKCOLOR: UINT = 0x2001;
/// Progress-bar filled-bar colour (`PBM_SETBARCOLOR`, `WM_USER + 9`).
pub const PBM_SETBARCOLOR: UINT = 0x0409;
pub const PBST_NORMAL: usize = 0x0001;

/// `PBM_SETPOS` takes an `i32` percentage; keep the helper obvious at call sites.
pub fn progress_set(hwnd: HWND, pct: u32) {
    unsafe {
        SendMessageW(hwnd, PBM_SETPOS, pct.min(100) as WPARAM, 0);
    }
}

/// UTF-16 + NUL for the Win32 wide APIs.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Read the text of a window as a `String` (best effort, truncating NULs).
pub fn window_text(hwnd: HWND) -> String {
    unsafe {
        let len = GetWindowTextLengthW(hwnd);
        if len <= 0 {
            return String::new();
        }
        let mut buf = vec![0u16; len as usize + 1];
        let n = GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
        if n <= 0 {
            return String::new();
        }
        String::from_utf16_lossy(&buf[..n as usize])
    }
}

// ---- clipboard -------------------------------------------------------------

pub const CF_UNICODETEXT: u32 = 13;
pub const GMEM_MOVEABLE: u32 = 0x0002;

#[link(name = "user32")]
extern "system" {
    pub fn OpenClipboard(hwnd: HWND) -> BOOL;
    pub fn EmptyClipboard() -> BOOL;
    pub fn CloseClipboard() -> BOOL;
    pub fn SetClipboardData(format: u32, mem: *mut c_void) -> *mut c_void;
}

#[link(name = "kernel32")]
extern "system" {
    pub fn GlobalAlloc(flags: u32, bytes: usize) -> *mut c_void;
    pub fn GlobalLock(mem: *mut c_void) -> *mut c_void;
    pub fn GlobalUnlock(mem: *mut c_void) -> BOOL;
    pub fn GlobalFree(mem: *mut c_void) -> *mut c_void;
}

// ---- owner-drawn buttons -----------------------------------------------------

pub const WM_DRAWITEM: UINT = 0x002B;

/// `DRAWITEMSTRUCT`, the minimum the button painter reads.
#[repr(C)]
pub struct DRAWITEMSTRUCT {
    pub ctl_type: UINT,
    pub ctl_id: UINT,
    pub item_id: UINT,
    pub item_action: UINT,
    pub item_state: UINT,
    pub hwnd_item: HWND,
    pub hdc: HDC,
    pub rc_item: RECT,
    pub item_data: usize,
}

pub const ODT_BUTTON: UINT = 4;
pub const ODS_SELECTED: UINT = 0x0001;
pub const ODS_DISABLED: UINT = 0x0004;
pub const ODS_FOCUS: UINT = 0x0010;

// ---- icon building --------------------------------------------------------

#[repr(C)]
pub struct BITMAPINFOHEADER {
    pub bi_size: u32,
    pub bi_width: i32,
    pub bi_height: i32,
    pub bi_planes: u16,
    pub bi_bit_count: u16,
    pub bi_compression: u32,
    pub bi_size_image: u32,
    pub bi_x_pels_per_meter: i32,
    pub bi_y_pels_per_meter: i32,
    pub bi_clr_used: u32,
    pub bi_clr_important: u32,
}

#[repr(C)]
pub struct BITMAPINFO {
    pub bmi_header: BITMAPINFOHEADER,
    pub bmi_colors: [u32; 1],
}

#[repr(C)]
pub struct ICONINFO {
    pub f_icon: BOOL,
    pub x_hotspot: DWORD,
    pub y_hotspot: DWORD,
    pub hbm_mask: HBITMAP,
    pub hbm_color: HBITMAP,
}

/// The palette a button is painted with, handed to the paint routine.
///
/// One instance lives for the window's lifetime and is read on every
/// `WM_DRAWITEM`. Kept as plain integers so nothing allocates on the paint path.
#[derive(Clone, Copy)]
pub struct BtnTheme {
    pub bg: u32,
    pub card: u32,
    pub raised: u32,
    pub ink: u32,
    pub dim: u32,
    pub line: u32,
    pub accent: u32,
    pub accent_hi: u32,
    pub radius: i32,
}
