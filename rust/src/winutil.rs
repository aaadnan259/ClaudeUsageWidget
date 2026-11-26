//! Win32 helpers: placement, monitors, fullscreen detection, theme, window styling
//! (acrylic, rounded corners) and popup menus.

use std::ffi::c_void;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Registry::*;
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCSTR, PCWSTR, w};

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub l: i32,
    pub t: i32,
    pub r: i32,
    pub b: i32,
}

impl From<RECT> for Rect {
    fn from(r: RECT) -> Rect {
        Rect { l: r.left, t: r.top, r: r.right, b: r.bottom }
    }
}

pub fn cursor_pos() -> (i32, i32) {
    let mut p = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut p);
    }
    (p.x, p.y)
}

pub fn mouse_down(right_too: bool) -> bool {
    unsafe {
        let l = GetAsyncKeyState(0x01) as u16 & 0x8000 != 0;
        let r = right_too && GetAsyncKeyState(0x02) as u16 & 0x8000 != 0;
        l || r
    }
}

/// (monitor handle, monitor rect, work rect) for the monitor containing a point.
pub fn monitor_info(x: i32, y: i32) -> (HMONITOR, Rect, Rect) {
    unsafe {
        let m = MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(m, &mut mi);
        (m, mi.rcMonitor.into(), mi.rcWork.into())
    }
}

pub fn primary_work_area() -> Rect {
    let mut r = RECT::default();
    unsafe {
        let _ = SystemParametersInfoW(SPI_GETWORKAREA, 0, Some(&mut r as *mut RECT as *mut c_void), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
    }
    r.into()
}

pub fn virtual_screen() -> Rect {
    unsafe {
        let x = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let y = GetSystemMetrics(SM_YVIRTUALSCREEN);
        Rect { l: x, t: y, r: x + GetSystemMetrics(SM_CXVIRTUALSCREEN), b: y + GetSystemMetrics(SM_CYVIRTUALSCREEN) }
    }
}

pub fn window_rect(hwnd: HWND) -> Rect {
    let mut r = RECT::default();
    unsafe {
        let _ = GetWindowRect(hwnd, &mut r);
    }
    r.into()
}

pub fn dpi_scale(hwnd: HWND) -> f32 {
    let d = unsafe { GetDpiForWindow(hwnd) };
    if d == 0 { 1.0 } else { d as f32 / 96.0 }
}

/// Move (and optionally size) a window in physical px, topmost, without activating it.
pub fn place(hwnd: HWND, x: i32, y: i32, size: Option<(i32, i32)>) {
    let mut flags = SWP_NOACTIVATE;
    let (w, h) = size.unwrap_or((0, 0));
    if size.is_none() {
        flags |= SWP_NOSIZE;
    }
    unsafe {
        let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), x, y, w, h, flags);
    }
}

pub fn set_topmost(hwnd: HWND) {
    unsafe {
        let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
    }
}

fn dwm_set<T>(hwnd: HWND, attr: DWMWINDOWATTRIBUTE, v: &T) -> bool {
    unsafe { DwmSetWindowAttribute(hwnd, attr, v as *const T as *const c_void, std::mem::size_of::<T>() as u32).is_ok() }
}

/// Rounded corners, a subtle border, dark/light frame, and the live acrylic backdrop.
/// Returns false if the system backdrop isn't available (caller draws an opaque fill).
pub fn style_glass(hwnd: HWND, dark: bool) -> bool {
    dwm_set(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &DWMWCP_ROUND);
    let border: u32 = if dark { 0x0050_4848 } else { 0x00DA_DADA };
    dwm_set(hwnd, DWMWA_BORDER_COLOR, &border);
    let d: i32 = dark as i32;
    dwm_set(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &d);
    unsafe {
        let _ = DwmExtendFrameIntoClientArea(hwnd, &MARGINS { cxLeftWidth: -1, cxRightWidth: -1, cyTopHeight: -1, cyBottomHeight: -1 });
    }
    dwm_set(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, &DWMSBT_TRANSIENTWINDOW)
}

pub fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

pub fn foreground_is_fullscreen(own: &[HWND], on_monitor: HMONITOR) -> bool {
    unsafe {
        let h = GetForegroundWindow();
        if h.is_invalid() || own.contains(&h) {
            return false;
        }
        if matches!(class_name(h).as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd") {
            return false;
        }
        let mon = MonitorFromWindow(h, MONITOR_DEFAULTTONEAREST);
        if mon != on_monitor {
            return false;
        }
        let r = window_rect(h);
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(mon, &mut mi);
        let m = mi.rcMonitor;
        r.l <= m.left && r.t <= m.top && r.r >= m.right && r.b >= m.bottom
    }
}

fn reg_dword(path: PCWSTR, name: PCWSTR) -> Option<u32> {
    let mut v: u32 = 0;
    let mut size = 4u32;
    let ok = unsafe {
        RegGetValueW(HKEY_CURRENT_USER, path, name, RRF_RT_REG_DWORD, None, Some(&mut v as *mut u32 as *mut c_void), Some(&mut size))
    };
    ok.is_ok().then_some(v)
}

pub fn windows_uses_light_theme() -> bool {
    reg_dword(w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"), w!("SystemUsesLightTheme")) == Some(1)
}

pub enum MenuItem {
    Item { id: u32, text: String, checked: bool, enabled: bool },
    Sep,
}

pub fn item(id: u32, text: &str, checked: bool) -> MenuItem {
    MenuItem::Item { id, text: text.into(), checked, enabled: true }
}

/// Let Win32 popup menus follow the Windows dark/light setting. Uses uxtheme's
/// SetPreferredAppMode (ordinal 135, Windows 10 1903+); harmless if unavailable.
pub fn allow_dark_menus() {
    use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
    unsafe {
        let Ok(ux) = LoadLibraryW(w!("uxtheme.dll")) else { return };
        if let Some(f) = GetProcAddress(ux, PCSTR(135 as *const u8)) {
            let set_mode: extern "system" fn(i32) -> i32 = std::mem::transmute(f);
            set_mode(1); // AllowDark: follow the system theme
        }
        if let Some(f) = GetProcAddress(ux, PCSTR(136 as *const u8)) {
            let flush: extern "system" fn() = std::mem::transmute(f);
            flush();
        }
    }
}

/// Show a popup menu and return the chosen id (0 = dismissed). It opens at the cursor's x,
/// with its bottom at `bottom_y` (so it doesn't cover the pill) or at the cursor.
/// The owner must be a window on this thread; it's brought to the foreground first so the
/// menu closes when the user clicks elsewhere.
pub fn popup_menu(owner: HWND, items: &[MenuItem], bottom_y: Option<i32>) -> u32 {
    unsafe {
        let Ok(m) = CreatePopupMenu() else { return 0 };
        for it in items {
            match it {
                MenuItem::Sep => {
                    let _ = AppendMenuW(m, MF_SEPARATOR, 0, PCWSTR::null());
                }
                MenuItem::Item { id, text, checked, enabled } => {
                    let mut f = MF_STRING;
                    if *checked {
                        f |= MF_CHECKED;
                    }
                    if !*enabled {
                        f |= MF_GRAYED;
                    }
                    let t = wide(text);
                    let _ = AppendMenuW(m, f, *id as usize, PCWSTR(t.as_ptr()));
                }
            }
        }
        let (x, cy) = cursor_pos();
        let y = bottom_y.unwrap_or(cy);
        let _ = SetForegroundWindow(owner);
        let cmd = TrackPopupMenu(m, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN, x, y, None, owner, None);
        let _ = PostMessageW(Some(owner), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(m);
        cmd.0 as u32
    }
}

pub fn open_folder(path: &std::path::Path) {
    let _ = std::process::Command::new("explorer.exe").arg(path).spawn();
}
