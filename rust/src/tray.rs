//! Notification-area icon: a ring with the session % in it, a tooltip, a menu and
//! balloon notifications. The icon is rendered with Direct2D into a WIC bitmap.

use crate::ui::gfx::{Gfx, pt};
use crate::winutil::wide;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{Result, w};

pub const WM_TRAY: u32 = WM_APP + 10;
const ICON_ID: u32 = 1;

pub struct Tray {
    hwnd: HWND,
    icon: Option<HICON>,
    key: String,
    tip: String,
    added: bool,
}

fn copy_wide(dst: &mut [u16], s: &str) {
    let w = wide(s);
    let n = w.len().min(dst.len() - 1);
    dst[..n].copy_from_slice(&w[..n]);
    dst[n] = 0;
}

impl Tray {
    pub fn new(hwnd: HWND) -> Tray {
        Tray { hwnd, icon: None, key: String::new(), tip: String::new(), added: false }
    }

    fn nid(&self) -> NOTIFYICONDATAW {
        NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: ICON_ID,
            uCallbackMessage: WM_TRAY,
            ..Default::default()
        }
    }

    fn apply(&mut self) {
        let mut nid = self.nid();
        nid.uFlags = NIF_MESSAGE | NIF_TIP | NIF_ICON | NIF_SHOWTIP;
        if let Some(i) = self.icon {
            nid.hIcon = i;
        }
        copy_wide(&mut nid.szTip, &self.tip);
        unsafe {
            if !self.added {
                self.added = Shell_NotifyIconW(NIM_ADD, &nid).as_bool();
                nid.Anonymous.uVersion = NOTIFYICON_VERSION_4;
                let _ = Shell_NotifyIconW(NIM_SETVERSION, &nid);
            } else if !Shell_NotifyIconW(NIM_MODIFY, &nid).as_bool() {
                self.added = Shell_NotifyIconW(NIM_ADD, &nid).as_bool();
            }
        }
    }

    /// Explorer restarted: the icon must be added again.
    pub fn readd(&mut self) {
        self.added = false;
        self.apply();
    }

    pub fn update(&mut self, g: &Gfx, pct: Option<f64>, color: u32, tip: &str) {
        let key = format!("{:?}|{color:06x}", pct.map(|p| p.round() as i64));
        if key != self.key {
            self.key = key;
            if let Ok(icon) = ring_icon(g, pct, color, 32)
                && let Some(old) = self.icon.replace(icon) {
                    unsafe {
                        let _ = DestroyIcon(old);
                    }
                }
            self.tip = tip.chars().take(120).collect();
            self.apply();
        } else if tip != self.tip {
            self.tip = tip.chars().take(120).collect();
            self.apply();
        }
    }

    pub fn notify(&self, title: &str, msg: &str) {
        let mut nid = self.nid();
        nid.uFlags = NIF_INFO;
        nid.dwInfoFlags = NIIF_NONE;
        copy_wide(&mut nid.szInfoTitle, title);
        copy_wide(&mut nid.szInfo, msg);
        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
        }
    }

    pub fn remove(&mut self) {
        if self.added {
            unsafe {
                let _ = Shell_NotifyIconW(NIM_DELETE, &self.nid());
            }
            self.added = false;
        }
    }
}

/// The ring icon: grey track, coloured arc for the session %, the number in the middle.
pub fn ring_icon(g: &Gfx, pct: Option<f64>, color: u32, size: u32) -> Result<HICON> {
    unsafe {
        let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
        let bmp = wic.CreateBitmap(size, size, &GUID_WICPixelFormat32bppPBGRA, WICBitmapCacheOnLoad)?;
        let props = D2D1_RENDER_TARGET_PROPERTIES {
            r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
            dpiX: 96.0, dpiY: 96.0, ..Default::default()
        };
        let rt = g.d2d_factory.CreateWicBitmapRenderTarget(&bmp, &props)?;
        rt.BeginDraw();
        rt.Clear(Some(&D2D1_COLOR_F::default()));
        let s = size as f32;
        let (pad, wd) = (s / 16.0, s / 8.0);
        let r = s / 2.0 - pad - wd / 2.0;
        let c = s / 2.0;
        let track = rt.CreateSolidColorBrush(&D2D1_COLOR_F { r: 90.0 / 255.0, g: 90.0 / 255.0, b: 96.0 / 255.0, a: 1.0 }, None)?;
        rt.DrawEllipse(&D2D1_ELLIPSE { point: pt(c, c), radiusX: r, radiusY: r }, &track, wd, None);
        if let Some(p) = pct {
            let col = D2D1_COLOR_F {
                r: ((color >> 16) & 255) as f32 / 255.0, g: ((color >> 8) & 255) as f32 / 255.0, b: (color & 255) as f32 / 255.0, a: 1.0,
            };
            let brush = rt.CreateSolidColorBrush(&col, None)?;
            let frac = (p.clamp(0.0, 100.0) / 100.0) as f32;
            if frac >= 0.999 {
                rt.DrawEllipse(&D2D1_ELLIPSE { point: pt(c, c), radiusX: r, radiusY: r }, &brush, wd, None);
            } else if frac > 0.0 {
                let path = g.d2d_factory.CreatePathGeometry()?;
                let sink = path.Open()?;
                let a = frac * std::f32::consts::TAU;
                sink.BeginFigure(pt(c, c - r), D2D1_FIGURE_BEGIN_HOLLOW);
                sink.AddArc(&D2D1_ARC_SEGMENT {
                    point: pt(c + r * a.sin(), c - r * a.cos()), size: D2D_SIZE_F { width: r, height: r }, rotationAngle: 0.0,
                    sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
                    arcSize: if frac > 0.5 { D2D1_ARC_SIZE_LARGE } else { D2D1_ARC_SIZE_SMALL },
                });
                sink.EndFigure(D2D1_FIGURE_END_OPEN);
                sink.Close()?;
                rt.DrawGeometry(&path, &brush, wd, None);
            }
            let txt = format!("{}", p.round() as i64);
            let fs = s * if txt.len() < 3 { 0.46 } else { 0.34 };
            let fmt = g.dwrite.CreateTextFormat(w!("Segoe UI"), None, DWRITE_FONT_WEIGHT_BOLD, DWRITE_FONT_STYLE_NORMAL,
                                                DWRITE_FONT_STRETCH_NORMAL, fs, w!("en-us"))?;
            fmt.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
            fmt.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
            let wtxt: Vec<u16> = txt.encode_utf16().collect();
            let black = rt.CreateSolidColorBrush(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.86 }, None)?;
            let white = rt.CreateSolidColorBrush(&D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a: 1.0 }, None)?;
            let rect = |dx: f32, dy: f32| D2D_RECT_F { left: dx, top: dy + 0.5, right: s + dx, bottom: s + dy + 0.5 };
            for (dx, dy) in [(-1.0, 0.0), (1.0, 0.0), (0.0, -1.0), (0.0, 1.0), (-1.0, -1.0), (1.0, 1.0), (-1.0, 1.0), (1.0, -1.0)] {
                rt.DrawText(&wtxt, &fmt, &rect(dx, dy), &black, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
            }
            rt.DrawText(&wtxt, &fmt, &rect(0.0, 0.0), &white, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
        }
        rt.EndDraw(None, None)?;
        let stride = size * 4;
        let mut px = vec![0u8; (stride * size) as usize];
        bmp.CopyPixels(std::ptr::null(), stride, &mut px)?;
        // Icons want straight (non-premultiplied) alpha.
        for p in px.as_chunks_mut::<4>().0 {
            let a = p[3] as u32;
            if a > 0 && a < 255 {
                for ch in &mut p[..3] {
                    *ch = ((*ch as u32 * 255 + a / 2) / a).min(255) as u8;
                }
            }
        }
        let mask = vec![0u8; (size * size / 8) as usize];
        CreateIcon(None, size as i32, size as i32, 1, 32, mask.as_ptr(), px.as_ptr())
    }
}
