//! GPU 2D drawing: D3D11 device → D2D device context → DXGI composition swap chain,
//! shown through DirectComposition so the window can be per-pixel transparent over the
//! DWM acrylic backdrop. Coordinates are in DIPs (the context's DPI does the scaling).

use std::cell::RefCell;
use std::collections::HashMap;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::DirectComposition::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::core::{Interface, Result, w};
use windows_numerics::Vector2;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgba(pub f32, pub f32, pub f32, pub f32);

impl Rgba {
    pub const fn hex(h: u32, a: f32) -> Rgba {
        Rgba(((h >> 16) & 255) as f32 / 255.0, ((h >> 8) & 255) as f32 / 255.0, (h & 255) as f32 / 255.0, a)
    }
    pub fn alpha(self, k: f32) -> Rgba {
        Rgba(self.0, self.1, self.2, self.3 * k)
    }
    fn d2d(self) -> D2D1_COLOR_F {
        D2D1_COLOR_F { r: self.0, g: self.1, b: self.2, a: self.3 }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

/// Device-independent objects shared by every window on the UI thread.
pub struct Gfx {
    pub d3d: ID3D11Device,
    pub d2d_factory: ID2D1Factory1,
    pub d2d_device: ID2D1Device,
    pub dcomp: IDCompositionDevice,
    pub dwrite: IDWriteFactory,
    dxgi_factory: IDXGIFactory2,
    round_stroke: ID2D1StrokeStyle,
    formats: RefCell<HashMap<(u32, u32), IDWriteTextFormat>>,
    tabular: IDWriteTypography,
}

impl Gfx {
    pub fn new() -> Result<Gfx> {
        unsafe {
            let mut d3d = None;
            let flags = D3D11_CREATE_DEVICE_BGRA_SUPPORT;
            let want_hw = std::env::var("CUW_GPU").as_deref() == Ok("hw");
            let hw = if want_hw {
                D3D11CreateDevice(None, D3D_DRIVER_TYPE_HARDWARE, Default::default(), flags, None,
                                  D3D11_SDK_VERSION, Some(&mut d3d), None, None)
            } else {
                Err(windows::core::Error::empty())
            };
            if hw.is_err() {
                D3D11CreateDevice(None, D3D_DRIVER_TYPE_WARP, Default::default(), flags, None,
                                  D3D11_SDK_VERSION, Some(&mut d3d), None, None)?;
            }
            let d3d: ID3D11Device = d3d.unwrap();
            let dxgi: IDXGIDevice = d3d.cast()?;
            let opts = D2D1_FACTORY_OPTIONS::default();
            let d2d_factory: ID2D1Factory1 = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, Some(&opts))?;
            let d2d_device = d2d_factory.CreateDevice(&dxgi)?;
            let dcomp: IDCompositionDevice = DCompositionCreateDevice(&dxgi)?;
            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let dxgi_factory: IDXGIFactory2 = CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0))?;
            let round_stroke = ID2D1Factory::CreateStrokeStyle(&d2d_factory,
                &D2D1_STROKE_STYLE_PROPERTIES {
                    startCap: D2D1_CAP_STYLE_ROUND, endCap: D2D1_CAP_STYLE_ROUND, dashCap: D2D1_CAP_STYLE_ROUND,
                    lineJoin: D2D1_LINE_JOIN_ROUND, miterLimit: 10.0, dashStyle: D2D1_DASH_STYLE_SOLID, dashOffset: 0.0,
                },
                None,
            )?;
            let tabular = dwrite.CreateTypography()?;
            tabular.AddFontFeature(DWRITE_FONT_FEATURE { nameTag: DWRITE_FONT_FEATURE_TAG_TABULAR_FIGURES, parameter: 1 })?;
            Ok(Gfx { d3d, d2d_factory, d2d_device, dcomp, dwrite, dxgi_factory, round_stroke,
                     formats: RefCell::new(HashMap::new()), tabular })
        }
    }

    /// A flat-capped dashed stroke. `dashes` are in DIPs for the given line width
    /// (like SVG stroke-dasharray); Direct2D wants them in multiples of the width.
    pub fn dashed(&self, dashes: &[f32], width: f32) -> Option<ID2D1StrokeStyle> {
        let rel: Vec<f32> = dashes.iter().map(|d| d / width.max(0.1)).collect();
        unsafe {
            ID2D1Factory::CreateStrokeStyle(
                &self.d2d_factory,
                &D2D1_STROKE_STYLE_PROPERTIES {
                    startCap: D2D1_CAP_STYLE_FLAT, endCap: D2D1_CAP_STYLE_FLAT, dashCap: D2D1_CAP_STYLE_FLAT,
                    lineJoin: D2D1_LINE_JOIN_ROUND, miterLimit: 10.0, dashStyle: D2D1_DASH_STYLE_CUSTOM, dashOffset: 0.0,
                },
                Some(&rel),
            )
            .ok()
        }
    }

    fn format(&self, size: f32, weight: u32) -> Result<IDWriteTextFormat> {
        let key = (size.to_bits(), weight);
        if let Some(f) = self.formats.borrow().get(&key) {
            return Ok(f.clone());
        }
        let f = unsafe {
            let f = self.dwrite.CreateTextFormat(
                w!("Segoe UI Variable Text"), None, DWRITE_FONT_WEIGHT(weight as i32), DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL, size, w!("en-us"),
            )?;
            f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
            f
        };
        self.formats.borrow_mut().insert(key, f.clone());
        Ok(f)
    }

    /// A text layout with tabular figures, so changing digits don't shift the text.
    pub fn layout(&self, text: &str, size: f32, weight: u32) -> Result<IDWriteTextLayout> {
        let wide: Vec<u16> = text.encode_utf16().collect();
        unsafe {
            let l = self.dwrite.CreateTextLayout(&wide, &self.format(size, weight)?, 4096.0, 4096.0)?;
            l.SetTypography(&self.tabular, DWRITE_TEXT_RANGE { startPosition: 0, length: wide.len() as u32 })?;
            Ok(l)
        }
    }

    /// Height of word-wrapped text (same settings as Canvas::text_wrapped).
    pub fn wrapped_height(&self, text: &str, size: f32, weight: u32, w: f32, line: f32) -> f32 {
        self.layout(text, size, weight)
            .and_then(|l| unsafe {
                l.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP)?;
                l.SetMaxWidth(w.max(1.0))?;
                l.SetLineSpacing(DWRITE_LINE_SPACING_METHOD_UNIFORM, line, line * 0.8)?;
                let mut m = DWRITE_TEXT_METRICS::default();
                l.GetMetrics(&mut m)?;
                Ok(m.height)
            })
            .unwrap_or(line)
    }

    pub fn text_width(&self, text: &str, size: f32, weight: u32) -> f32 {
        self.layout(text, size, weight)
            .and_then(|l| unsafe {
                let mut m = DWRITE_TEXT_METRICS::default();
                l.GetMetrics(&mut m)?;
                Ok(m.widthIncludingTrailingWhitespace)
            })
            .unwrap_or(0.0)
    }
}

/// One window's swap chain + drawing context.
pub struct Surface {
    pub dc: ID2D1DeviceContext,
    swap: IDXGISwapChain1,
    _target: IDCompositionTarget,
    pub visual: IDCompositionVisual,
    pub w: u32,
    pub h: u32,
    pub scale: f32,
}

fn composition_swap(g: &Gfx, w: u32, h: u32) -> Result<IDXGISwapChain1> {
    let desc = DXGI_SWAP_CHAIN_DESC1 {
        Width: w.max(1), Height: h.max(1), Format: DXGI_FORMAT_B8G8R8A8_UNORM, Stereo: false.into(),
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 }, BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
        BufferCount: 2, Scaling: DXGI_SCALING_STRETCH, SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
        AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED, Flags: 0,
    };
    unsafe { g.dxgi_factory.CreateSwapChainForComposition(&g.d3d, &desc, None) }
}

/// Point a device context at a swap chain's back buffer, at the given scale.
fn bind_target(dc: &ID2D1DeviceContext, swap: &IDXGISwapChain1, scale: f32) -> Result<()> {
    unsafe {
        let surface: IDXGISurface = swap.GetBuffer(0)?;
        let dpi = 96.0 * scale;
        let props = D2D1_BITMAP_PROPERTIES1 {
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
            dpiX: dpi, dpiY: dpi,
            bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
            colorContext: std::mem::ManuallyDrop::new(None),
        };
        let bmp = dc.CreateBitmapFromDxgiSurface(&surface, Some(&props))?;
        dc.SetTarget(&bmp);
        dc.SetDpi(dpi, dpi);
        dc.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
    }
    Ok(())
}

impl Surface {
    pub fn new(g: &Gfx, hwnd: HWND, w: u32, h: u32, scale: f32) -> Result<Surface> {
        unsafe {
            let swap = composition_swap(g, w, h)?;
            let dc = g.d2d_device.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)?;
            let target = g.dcomp.CreateTargetForHwnd(hwnd, true)?;
            let visual = g.dcomp.CreateVisual()?;
            visual.SetContent(&swap)?;
            target.SetRoot(&visual)?;
            g.dcomp.Commit()?;
            let mut s = Surface { dc, swap, _target: target, visual, w: 0, h: 0, scale };
            s.bind(w.max(1), h.max(1), scale)?;
            Ok(s)
        }
    }

    fn bind(&mut self, w: u32, h: u32, scale: f32) -> Result<()> {
        unsafe {
            self.dc.SetTarget(None);
            if self.w != 0 {
                self.swap.ResizeBuffers(0, w, h, DXGI_FORMAT_UNKNOWN, DXGI_SWAP_CHAIN_FLAG(0))?;
            }
        }
        bind_target(&self.dc, &self.swap, scale)?;
        self.w = w;
        self.h = h;
        self.scale = scale;
        Ok(())
    }

    pub fn resize(&mut self, w: u32, h: u32, scale: f32) -> Result<()> {
        if w.max(1) == self.w && h.max(1) == self.h && scale == self.scale {
            return Ok(());
        }
        self.bind(w.max(1), h.max(1), scale)
    }

    pub fn begin<'a>(&'a self, g: &'a Gfx) -> Canvas<'a> {
        unsafe {
            self.dc.BeginDraw();
            self.dc.Clear(Some(&D2D1_COLOR_F::default()));
        }
        Canvas { g, dc: &self.dc, alpha: 1.0 }
    }

    pub fn present(&self) -> Result<()> {
        unsafe {
            self.dc.EndDraw(None, None)?;
            self.swap.Present(1, DXGI_PRESENT(0)).ok()
        }
    }

    /// The card's open animation from card.html: fade in over 0.25 s and slide up 8 DIP
    /// over 0.3 s, ease-out. A DirectComposition animation, so it costs no redraws.
    pub fn animate_in(&self, g: &Gfx) -> Result<()> {
        unsafe {
            // ease-out cubic as a single cubic segment: v(t) = a + b t + c t^2 + d t^3
            let fade = g.dcomp.CreateAnimation()?;
            let d = 0.25f32;
            fade.AddCubic(0.0, 0.0, 3.0 / d, -3.0 / (d * d), 1.0 / (d * d * d))?;
            fade.End(d as f64, 1.0)?;
            let slide = g.dcomp.CreateAnimation()?;
            let d2 = 0.3f32;
            let dist = 8.0 * self.scale;
            slide.AddCubic(0.0, dist, -3.0 * dist / d2, 3.0 * dist / (d2 * d2), -dist / (d2 * d2 * d2))?;
            slide.End(d2 as f64, 0.0)?;
            let fx = g.dcomp.CreateEffectGroup()?;
            fx.SetOpacity(&fade)?;
            self.visual.SetEffect(&fx)?;
            self.visual.SetOffsetY(&slide)?;
            g.dcomp.Commit()
        }
    }
}

/// A small marker (a dot) with a pulsing halo, as two child layers over a surface.
/// The pulse — the halo growing from r 4 to 12 while fading from 70% to 0 every 1.8 s,
/// ease-out — is a DirectComposition animation that Windows runs itself, so it costs
/// this process nothing per frame.
pub struct Halo {
    halo: IDCompositionVisual,
    dot: IDCompositionVisual,
    halo_swap: IDXGISwapChain1,
    dot_swap: IDXGISwapChain1,
    dc: ID2D1DeviceContext,
    scale_t: IDCompositionScaleTransform,
    effect: IDCompositionEffectGroup,
    key: String,
    pulsing: Option<bool>,
}

const HALO_R: f32 = 12.0; // largest halo radius (DIP)
const DOT_BOX: f32 = 12.0; // dot layer size (DIP)

impl Halo {
    pub fn new(g: &Gfx, parent: &IDCompositionVisual) -> Result<Halo> {
        unsafe {
            let halo_swap = composition_swap(g, 64, 64)?;
            let dot_swap = composition_swap(g, 32, 32)?;
            let halo = g.dcomp.CreateVisual()?;
            let dot = g.dcomp.CreateVisual()?;
            halo.SetContent(&halo_swap)?;
            dot.SetContent(&dot_swap)?;
            parent.AddVisual(&halo, true, None)?;
            parent.AddVisual(&dot, true, None)?;
            let scale_t = g.dcomp.CreateScaleTransform()?;
            halo.SetTransform(&scale_t)?;
            let effect = g.dcomp.CreateEffectGroup()?;
            halo.SetEffect(&effect)?;
            let dc = g.d2d_device.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)?;
            Ok(Halo { halo, dot, halo_swap, dot_swap, dc, scale_t, effect, key: String::new(), pulsing: None })
        }
    }

    fn paint(&self, g: &Gfx, swap: &IDXGISwapChain1, px: u32, scale: f32, f: impl FnOnce(&Canvas)) -> Result<()> {
        unsafe {
            self.dc.SetTarget(None);
            swap.ResizeBuffers(0, px.max(1), px.max(1), DXGI_FORMAT_UNKNOWN, DXGI_SWAP_CHAIN_FLAG(0))?;
            bind_target(&self.dc, swap, scale)?;
            self.dc.BeginDraw();
            self.dc.Clear(Some(&D2D1_COLOR_F::default()));
            f(&Canvas { g, dc: &self.dc, alpha: 1.0 });
            self.dc.EndDraw(None, None)?;
            self.dc.SetTarget(None);
            swap.Present(0, DXGI_PRESENT(0)).ok()
        }
    }

    /// Show the marker at (x, y) DIP. `halo` = the pulsing ring colour (None: no pulse).
    pub fn show(&mut self, g: &Gfx, x: f32, y: f32, scale: f32, ring: Rgba, fill: Rgba, halo: Option<Rgba>) -> Result<()> {
        let key = format!("{scale}|{ring:?}|{fill:?}|{halo:?}");
        let hp = (2.0 * HALO_R * scale).ceil() as u32 + 2;
        let dp = (DOT_BOX * scale).ceil() as u32 + 2;
        if key != self.key {
            self.key = key;
            let c = halo.unwrap_or(fill);
            self.paint(g, &self.halo_swap, hp, scale, |cv| {
                let r = hp as f32 / scale / 2.0;
                cv.fill_circle(r, r, HALO_R, c);
            })?;
            self.paint(g, &self.dot_swap, dp, scale, |cv| {
                let r = dp as f32 / scale / 2.0;
                cv.fill_circle(r, r, 4.75, ring);
                cv.fill_circle(r, r, 4.0, fill);
            })?;
        }
        unsafe {
            let (hx, hy) = ((x * scale - hp as f32 / 2.0).round(), (y * scale - hp as f32 / 2.0).round());
            self.halo.SetOffsetX2(hx)?;
            self.halo.SetOffsetY2(hy)?;
            self.scale_t.SetCenterX2(hp as f32 / 2.0)?;
            self.scale_t.SetCenterY2(hp as f32 / 2.0)?;
            self.dot.SetOffsetX2((x * scale - dp as f32 / 2.0).round())?;
            self.dot.SetOffsetY2((y * scale - dp as f32 / 2.0).round())?;
            let want = halo.is_some();
            if self.pulsing != Some(want) {
                self.pulsing = Some(want);
                if want {
                    // k = t/1.8, e = 1-(1-k)^2 = 2k - k^2
                    // radius 4 + 8e  -> scale (4 + 8e)/12 = 1/3 + (16/12)k - (8/12)k^2
                    // opacity 0.7(1-e) = 0.7 - 1.4k + 0.7k^2
                    let d = 1.8f32;
                    let sc = g.dcomp.CreateAnimation()?;
                    sc.AddCubic(0.0, 1.0 / 3.0, 16.0 / 12.0 / d, -(8.0 / 12.0) / (d * d), 0.0)?;
                    sc.AddRepeat(d as f64, d as f64)?;
                    let op = g.dcomp.CreateAnimation()?;
                    op.AddCubic(0.0, 0.7, -1.4 / d, 0.7 / (d * d), 0.0)?;
                    op.AddRepeat(d as f64, d as f64)?;
                    self.scale_t.SetScaleX(&sc)?;
                    self.scale_t.SetScaleY(&sc)?;
                    self.effect.SetOpacity(&op)?;
                } else {
                    self.effect.SetOpacity2(0.0)?;
                }
            }
            self.dot.SetEffect(None)?;
            g.dcomp.Commit()?;
        }
        Ok(())
    }

    /// Hide both layers (e.g. no timeline).
    pub fn hide(&mut self, g: &Gfx) -> Result<()> {
        unsafe {
            self.halo.SetOffsetX2(-10000.0)?;
            self.dot.SetOffsetX2(-10000.0)?;
            g.dcomp.Commit()
        }
    }
}

/// Immediate drawing helpers. `alpha` multiplies every colour (used for idle dimming).
pub struct Canvas<'a> {
    pub g: &'a Gfx,
    pub dc: &'a ID2D1DeviceContext,
    pub alpha: f32,
}

pub fn pt(x: f32, y: f32) -> Vector2 {
    Vector2 { X: x, Y: y }
}

impl<'a> Canvas<'a> {
    fn solid(&self, c: Rgba) -> Option<ID2D1SolidColorBrush> {
        unsafe { self.dc.CreateSolidColorBrush(&c.alpha(self.alpha).d2d(), None).ok() }
    }

    pub fn linear(&self, x0: f32, y0: f32, x1: f32, y1: f32, stops: &[(f32, Rgba)]) -> Option<ID2D1Brush> {
        let st: Vec<D2D1_GRADIENT_STOP> =
            stops.iter().map(|(p, c)| D2D1_GRADIENT_STOP { position: *p, color: c.alpha(self.alpha).d2d() }).collect();
        unsafe {
            let coll = ID2D1RenderTarget::CreateGradientStopCollection(self.dc, &st, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP).ok()?;
            let b = self.dc.CreateLinearGradientBrush(
                &D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES { startPoint: pt(x0, y0), endPoint: pt(x1, y1) }, None, &coll).ok()?;
            b.cast().ok()
        }
    }

    pub fn radial(&self, cx: f32, cy: f32, rx: f32, ry: f32, stops: &[(f32, Rgba)]) -> Option<ID2D1Brush> {
        let st: Vec<D2D1_GRADIENT_STOP> =
            stops.iter().map(|(p, c)| D2D1_GRADIENT_STOP { position: *p, color: c.alpha(self.alpha).d2d() }).collect();
        unsafe {
            let coll = ID2D1RenderTarget::CreateGradientStopCollection(self.dc, &st, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP).ok()?;
            let b = self.dc.CreateRadialGradientBrush(
                &D2D1_RADIAL_GRADIENT_BRUSH_PROPERTIES { center: pt(cx, cy), gradientOriginOffset: pt(0.0, 0.0), radiusX: rx, radiusY: ry },
                None, &coll).ok()?;
            b.cast().ok()
        }
    }

    pub fn solid_brush(&self, c: Rgba) -> Option<ID2D1Brush> {
        self.solid(c).and_then(|b| b.cast().ok())
    }

    pub fn fill_rect_brush(&self, x: f32, y: f32, w: f32, h: f32, r: f32, b: &ID2D1Brush) {
        let rr = D2D1_ROUNDED_RECT { rect: D2D_RECT_F { left: x, top: y, right: x + w, bottom: y + h }, radiusX: r, radiusY: r };
        unsafe { self.dc.FillRoundedRectangle(&rr, b) }
    }

    pub fn fill_rect(&self, x: f32, y: f32, w: f32, h: f32, r: f32, c: Rgba) {
        if let Some(b) = self.solid_brush(c) {
            self.fill_rect_brush(x, y, w, h, r, &b);
        }
    }

    pub fn fill_circle(&self, cx: f32, cy: f32, r: f32, c: Rgba) {
        if let Some(b) = self.solid(c) {
            let e = D2D1_ELLIPSE { point: pt(cx, cy), radiusX: r, radiusY: r };
            unsafe { self.dc.FillEllipse(&e, &b) }
        }
    }

    pub fn stroke_circle(&self, cx: f32, cy: f32, r: f32, width: f32, c: Rgba) {
        if let Some(b) = self.solid(c) {
            let e = D2D1_ELLIPSE { point: pt(cx, cy), radiusX: r, radiusY: r };
            unsafe { self.dc.DrawEllipse(&e, &b, width, None) }
        }
    }

    pub fn line(&self, x0: f32, y0: f32, x1: f32, y1: f32, width: f32, b: &ID2D1Brush) {
        unsafe { self.dc.DrawLine(pt(x0, y0), pt(x1, y1), b, width, &self.g.round_stroke) }
    }

    /// Arc from 12 o'clock, clockwise, `frac` of a full turn, round caps.
    pub fn arc(&self, cx: f32, cy: f32, r: f32, frac: f32, width: f32, b: &ID2D1Brush) {
        let frac = frac.clamp(0.0, 1.0);
        if frac <= 0.0005 {
            return;
        }
        if frac >= 0.9995 {
            let e = D2D1_ELLIPSE { point: pt(cx, cy), radiusX: r, radiusY: r };
            unsafe { self.dc.DrawEllipse(&e, b, width, None) };
            return;
        }
        let a = frac * std::f32::consts::TAU;
        let end = pt(cx + r * a.sin(), cy - r * a.cos());
        unsafe {
            let Ok(path) = self.g.d2d_factory.CreatePathGeometry() else { return };
            let Ok(sink) = path.Open() else { return };
            sink.BeginFigure(pt(cx, cy - r), D2D1_FIGURE_BEGIN_HOLLOW);
            sink.AddArc(&D2D1_ARC_SEGMENT {
                point: end, size: D2D_SIZE_F { width: r, height: r }, rotationAngle: 0.0,
                sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
                arcSize: if frac > 0.5 { D2D1_ARC_SIZE_LARGE } else { D2D1_ARC_SIZE_SMALL },
            });
            sink.EndFigure(D2D1_FIGURE_END_OPEN);
            let _ = sink.Close();
            self.dc.DrawGeometry(&path, b, width, &self.g.round_stroke);
        }
    }

    pub fn line_styled(&self, x0: f32, y0: f32, x1: f32, y1: f32, width: f32, b: &ID2D1Brush, style: Option<&ID2D1StrokeStyle>) {
        unsafe { self.dc.DrawLine(pt(x0, y0), pt(x1, y1), b, width, style) }
    }

    fn path(&self, pts: &[(f32, f32)], closed: bool) -> Option<ID2D1PathGeometry1> {
        if pts.len() < 2 {
            return None;
        }
        unsafe {
            let path = self.g.d2d_factory.CreatePathGeometry().ok()?;
            let sink = path.Open().ok()?;
            sink.BeginFigure(pt(pts[0].0, pts[0].1), if closed { D2D1_FIGURE_BEGIN_FILLED } else { D2D1_FIGURE_BEGIN_HOLLOW });
            for p in &pts[1..] {
                sink.AddLine(pt(p.0, p.1));
            }
            sink.EndFigure(if closed { D2D1_FIGURE_END_CLOSED } else { D2D1_FIGURE_END_OPEN });
            sink.Close().ok()?;
            Some(path)
        }
    }

    /// Polyline with round joins and caps.
    pub fn polyline(&self, pts: &[(f32, f32)], width: f32, b: &ID2D1Brush) {
        if let Some(p) = self.path(pts, false) {
            unsafe { self.dc.DrawGeometry(&p, b, width, &self.g.round_stroke) }
        }
    }

    pub fn fill_polygon(&self, pts: &[(f32, f32)], b: &ID2D1Brush) {
        if let Some(p) = self.path(pts, true) {
            unsafe { self.dc.FillGeometry(&p, b, None) }
        }
    }

    pub fn stroke_circle_brush(&self, cx: f32, cy: f32, r: f32, width: f32, b: &ID2D1Brush) {
        let e = D2D1_ELLIPSE { point: pt(cx, cy), radiusX: r, radiusY: r };
        unsafe { self.dc.DrawEllipse(&e, b, width, None) }
    }

    /// Word-wrapped text from (x, y) within width w; returns the height used.
    pub fn text_wrapped(&self, s: &str, size: f32, weight: u32, x: f32, y: f32, w: f32, line: f32, brush: &ID2D1Brush) -> f32 {
        let Ok(l) = self.g.layout(s, size, weight) else { return 0.0 };
        unsafe {
            let _ = l.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP);
            let _ = l.SetMaxWidth(w.max(1.0));
            let _ = l.SetLineSpacing(DWRITE_LINE_SPACING_METHOD_UNIFORM, line, line * 0.8);
            self.dc.DrawTextLayout(pt(x, y), &l, brush, D2D1_DRAW_TEXT_OPTIONS_NONE);
            let mut m = DWRITE_TEXT_METRICS::default();
            let _ = l.GetMetrics(&mut m);
            m.height
        }
    }

    /// Text in a box: horizontal alignment within [x, x+w], vertically centred in [y, y+h].
    /// `runs` lets parts of the string use another colour/weight: (start, len, colour, weight).
    pub fn text(&self, s: &str, size: f32, weight: u32, x: f32, y: f32, w: f32, h: f32, align: Align,
                brush: &ID2D1Brush, runs: &[(u32, u32, Rgba, u32)]) -> f32 {
        let sized: Vec<(u32, u32, Rgba, u32, f32)> = runs.iter().map(|r| (r.0, r.1, r.2, r.3, 0.0)).collect();
        self.text_sized(s, size, weight, x, y, w, h, align, brush, &sized)
    }

    /// Like `text`, but runs can also change the font size (0 = unchanged).
    pub fn text_sized(&self, s: &str, size: f32, weight: u32, x: f32, y: f32, w: f32, h: f32, align: Align,
                      brush: &ID2D1Brush, runs: &[(u32, u32, Rgba, u32, f32)]) -> f32 {
        let Ok(l) = self.g.layout(s, size, weight) else { return 0.0 };
        unsafe {
            let _ = l.SetMaxWidth(w.max(1.0));
            let _ = l.SetMaxHeight(h.max(1.0));
            let _ = l.SetTextAlignment(match align {
                Align::Left => DWRITE_TEXT_ALIGNMENT_LEADING,
                Align::Center => DWRITE_TEXT_ALIGNMENT_CENTER,
                Align::Right => DWRITE_TEXT_ALIGNMENT_TRAILING,
            });
            let _ = l.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
            for (start, len, c, wt, sz) in runs {
                let range = DWRITE_TEXT_RANGE { startPosition: *start, length: *len };
                if let Some(b) = self.solid(*c) {
                    let _ = l.SetDrawingEffect(&b, range);
                }
                let _ = l.SetFontWeight(DWRITE_FONT_WEIGHT(*wt as i32), range);
                if *sz > 0.0 {
                    let _ = l.SetFontSize(*sz, range);
                }
            }
            self.dc.DrawTextLayout(pt(x, y), &l, brush, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT);
            let mut m = DWRITE_TEXT_METRICS::default();
            let _ = l.GetMetrics(&mut m);
            m.widthIncludingTrailingWhitespace
        }
    }
}
