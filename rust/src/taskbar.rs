//! The optional taskbar button: a minimised window whose icon is the ring with the session
//! %, with ITaskbarList3 progress (green / amber "paused" at 75% / red "error" at 90%).
//! Clicking it toggles the card; closing it quits.

use crate::logf;
use crate::tray::ring_icon;
use crate::ui::gfx::Gfx;
use crate::winutil::wide;
use windows::Win32::Foundation::*;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::UI::Shell::{ITaskbarList3, TBPF_ERROR, TBPF_NOPROGRESS, TBPF_NORMAL, TBPF_PAUSED, TaskbarList};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

pub const BUTTON_CLASS: PCWSTR = w!("ClaudeUsageWidgetButton");

pub struct Taskbar {
    pub hwnd: HWND,
    list: Option<ITaskbarList3>,
    icons: Vec<HICON>,
    key: String,
    last: Option<(Option<f64>, u32, String)>,
}

impl Taskbar {
    pub fn new(hinst: HINSTANCE, proc: WNDPROC) -> windows::core::Result<Taskbar> {
        unsafe {
            let wc = WNDCLASSW { lpfnWndProc: proc, hInstance: hinst, lpszClassName: BUTTON_CLASS, ..Default::default() };
            RegisterClassW(&wc);
            let hwnd = CreateWindowExW(WINDOW_EX_STYLE(0), BUTTON_CLASS, w!("Claude usage"), WS_OVERLAPPEDWINDOW,
                                       -32000, -32000, 120, 40, None, None, Some(hinst), None)?;
            let list: Option<ITaskbarList3> = match CoCreateInstance(&TaskbarList, None, CLSCTX_INPROC_SERVER) {
                Ok(l) => {
                    let l: ITaskbarList3 = l;
                    let _ = l.HrInit();
                    Some(l)
                }
                Err(e) => {
                    logf!("WARNING ITaskbarList3: {e}");
                    None
                }
            };
            let _ = ShowWindow(hwnd, SW_SHOWMINNOACTIVE);
            Ok(Taskbar { hwnd, list, icons: Vec::new(), key: String::new(), last: None })
        }
    }

    pub fn update(&mut self, g: &Gfx, pct: Option<f64>, color: u32, title: &str) {
        self.last = Some((pct, color, title.to_string()));
        self.apply(g, false);
    }

    /// Re-apply everything (the taskbar was recreated, e.g. Explorer restarted).
    pub fn reapply(&mut self, g: &Gfx) {
        self.apply(g, true);
    }

    fn apply(&mut self, g: &Gfx, force: bool) {
        let Some((pct, color, title)) = self.last.clone() else { return };
        unsafe {
            let t = wide(&title);
            let _ = SetWindowTextW(self.hwnd, PCWSTR(t.as_ptr()));
            let key = format!("{:?}|{color:06x}", pct.map(|p| p.round() as i64));
            if key != self.key || force {
                self.key = key;
                let big = ring_icon(g, pct, color, 32).ok();
                let small = ring_icon(g, pct, color, 16).ok();
                if let Some(b) = big {
                    SendMessageW(self.hwnd, WM_SETICON, Some(WPARAM(ICON_BIG as usize)), Some(LPARAM(b.0 as isize)));
                }
                if let Some(s) = small {
                    SendMessageW(self.hwnd, WM_SETICON, Some(WPARAM(ICON_SMALL as usize)), Some(LPARAM(s.0 as isize)));
                }
                for old in self.icons.drain(..) {
                    let _ = DestroyIcon(old);
                }
                self.icons.extend(big.into_iter().chain(small));
            }
            if let Some(l) = &self.list {
                match pct {
                    None => {
                        let _ = l.SetProgressState(self.hwnd, TBPF_NOPROGRESS);
                    }
                    Some(p) => {
                        let st = if p >= 90.0 { TBPF_ERROR } else if p >= 75.0 { TBPF_PAUSED } else { TBPF_NORMAL };
                        let _ = l.SetProgressState(self.hwnd, st);
                        let _ = l.SetProgressValue(self.hwnd, (p.max(1.0) as u64).min(100), 100);
                    }
                }
            }
        }
    }

    pub fn destroy(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}
