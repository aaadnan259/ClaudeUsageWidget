//! The pill above the taskbar: bars / ring / text styles, drawn with Direct2D.
//! A port of pill.html: same sizes, spacing, colours and states (stale, pending, error dot,
//! idle dimming, pulse at 90%+). Coordinates are DIPs.

use super::anim::{Tween, pulse};
use super::gfx::{Align, Canvas, Rgba};
use super::theme::Theme;
use crate::model::{FreshState, Level, Model, Row};
use std::time::Instant;

pub fn size_for(style: &str) -> (f32, f32) {
    match style {
        "ring" => (196.0, 52.0),
        "text" => (214.0, 40.0),
        _ => (236.0, 52.0),
    }
}

pub struct PillView {
    pub style: String,
    pub dim_setting: bool,
    pub hover: bool,
    pub acrylic: bool,
    bars: [Tween; 2],
    ring: Tween,
    dim: Tween,
    pulse_t0: Instant,
}

const W600: u32 = 600;
const W700: u32 = 700;

impl PillView {
    pub fn new() -> PillView {
        PillView {
            style: "bars".into(), dim_setting: true, hover: false, acrylic: true,
            bars: [Tween::new(1000), Tween::new(1000)], ring: Tween::new(1000), dim: Tween::new(250),
            pulse_t0: Instant::now(),
        }
    }

    /// Feed new values to the animations. Call whenever the model or hover changes.
    pub fn sync(&mut self, m: &Model) {
        for (i, r) in m.rows.iter().take(2).enumerate() {
            self.bars[i].set(r.pct.clamp(0.0, 100.0).max(3.0) as f32 / 100.0, true);
        }
        if let Some(r) = m.rows.first() {
            self.ring.set(r.pct.clamp(0.0, 100.0) as f32 / 100.0, true);
        }
        let dimmed = self.dim_setting && !self.hover;
        self.dim.set(if dimmed { 0.62 } else { 1.0 }, false);
    }

    pub fn pulsing(&self, m: &Model) -> bool {
        self.style == "ring" && m.rows.first().is_some_and(|r| r.pct >= 90.0)
    }

    pub fn animating(&self, m: &Model) -> bool {
        self.bars.iter().any(Tween::running) || self.ring.running() || self.dim.running() || self.pulsing(m)
    }

    /// Hover text (the HTML pill's body title).
    pub fn tooltip(m: &Model) -> String {
        match m.fresh.state {
            FreshState::Stale => format!("{} · {}", m.fresh.reason, m.fresh.label),
            _ => m.fresh.label.clone(),
        }
    }

    pub fn draw(&self, c: &mut Canvas, t: &Theme, m: &Model, w: f32, h: f32) {
        // Background: acrylic shows through; our tint + sheen go on top (opaque if no acrylic).
        if !self.acrylic {
            c.fill_rect(0.0, 0.0, w, h, 0.0, t.body);
        }
        let (dx, dy) = (155f32.to_radians().sin(), -155f32.to_radians().cos());
        let len = (w * dx).abs() + (h * dy).abs();
        let (cx, cy) = (w / 2.0, h / 2.0);
        if let Some(b) = c.linear(cx - dx * len / 2.0, cy - dy * len / 2.0, cx + dx * len / 2.0, cy + dy * len / 2.0,
                                  &[(0.0, t.tint_a), (1.0, t.tint_b)]) {
            c.fill_rect_brush(0.0, 0.0, w, h, 0.0, &b);
        }
        if let Some(b) = c.radial(0.15 * w, -0.1 * h, 1.2 * w, 0.6 * h, &[(0.0, t.sheen), (0.6, t.sheen.alpha(0.0))]) {
            c.fill_rect_brush(0.0, 0.0, w, h, 0.0, &b);
        }

        c.alpha = self.dim.value();
        let stale = m.fresh.state == FreshState::Stale;
        if m.rows.is_empty() {
            self.draw_message(c, t, m, w, h);
        } else {
            match self.style.as_str() {
                "ring" => self.draw_ring(c, t, &m.rows, w, h, stale),
                "text" => self.draw_text(c, t, &m.rows, w, h, stale),
                _ => self.draw_bars(c, t, &m.rows, w, h, stale),
            }
        }
        c.alpha = 1.0;
        if stale {
            c.fill_circle(w - 6.0 - 3.0, 5.0 + 3.0, 3.0, t.warn.1);
        }
    }

    fn spark(c: &Canvas, t: &Theme, x: f32, y: f32, size: f32) {
        let k = size / 24.0;
        let Some(b) = c.solid_brush(t.brand) else { return };
        for a in [0f32, 45.0, 90.0, 135.0] {
            let (s, co) = (a.to_radians().sin(), a.to_radians().cos());
            let (cx, cy) = (x + 12.0 * k, y + 12.0 * k);
            c.line(cx - 9.0 * k * s, cy + 9.0 * k * co, cx + 9.0 * k * s, cy - 9.0 * k * co, 2.6 * k, &b);
        }
    }

    /// Brush for a big number: level gradient normally, plain `sub` when stale.
    fn num_brush(c: &Canvas, t: &Theme, lv: Level, x: f32, y: f32, w: f32, h: f32, stale: bool, plain_ok: bool)
        -> Option<windows::Win32::Graphics::Direct2D::ID2D1Brush> {
        if stale {
            return c.solid_brush(t.sub);
        }
        if plain_ok && lv == Level::Ok {
            return c.solid_brush(t.fg);
        }
        let (c1, c2, _) = t.level(lv);
        // CSS linear-gradient(120deg, c1, c2) across the text box.
        c.linear(x, y + h * 0.2, x + w, y + h * 0.8, &[(0.0, c1), (1.0, c2)])
    }

    fn draw_bars(&self, c: &mut Canvas, t: &Theme, rows: &[Row], w: f32, h: f32, stale: bool) {
        let rows = &rows[..rows.len().min(2)];
        let line = 16.0;
        let gap = 7.0;
        let total = line * rows.len() as f32 + gap * (rows.len() as f32 - 1.0).max(0.0);
        let y0 = (h - total) / 2.0;
        // The reset column is "auto" (min 52px): the same width for both rows.
        let rw = rows.iter().map(|r| c.g.text_width(&r.reset_short, 11.5, 400)).fold(52.0f32, f32::max).ceil();
        let rx = w - 12.0 - rw;
        let px = rx - 8.0 - 40.0;
        let bx = 12.0 + 22.0 + 8.0;
        let bw = (px - 8.0 - bx).max(10.0);
        let fg = c.solid_brush(t.fg);
        let sub = c.solid_brush(t.sub);
        for (i, r) in rows.iter().enumerate() {
            let y = y0 + i as f32 * (line + gap);
            if let Some(b) = &fg {
                c.text(&r.short, 12.5, W600, 12.0, y, 22.0, line, Align::Left, b, &[]);
            }
            // bar
            let by = y + line / 2.0 - 3.0;
            c.fill_rect(bx, by, bw, 6.0, 3.0, t.track);
            let fw = bw * self.bars[i].value().clamp(0.0, 1.0);
            let (c1, c2, glow) = t.level(r.level);
            if r.pct_i.is_some() {
                if stale {
                    c.fill_rect(bx, by, fw, 6.0, 3.0, t.faint);
                } else {
                    c.fill_rect(bx - 1.5, by - 1.5, fw + 3.0, 9.0, 4.5, glow.alpha(0.22));
                    if let Some(b) = c.linear(bx, 0.0, bx + fw.max(1.0), 0.0, &[(0.0, c1), (1.0, c2)]) {
                        c.fill_rect_brush(bx, by, fw, 6.0, 3.0, &b);
                    }
                }
            }
            // percentage
            if let Some(b) = Self::num_brush(c, t, r.level, px, y, 40.0, line, stale, true) {
                c.text(&r.pct_text(), 12.5, W700, px, y, 40.0, line, Align::Right, &b, &[]);
            }
            if let Some(b) = &sub {
                c.text(&r.reset_short, 11.5, 400, rx, y, rw, line, Align::Right, b, &[]);
            }
        }
    }

    fn draw_ring(&self, c: &mut Canvas, t: &Theme, rows: &[Row], _w: f32, h: f32, stale: bool) {
        let five = &rows[0];
        let (cx, cy, r) = (12.0 + 17.0, h / 2.0, 13.5);
        let base = c.alpha;
        if self.style == "ring" && five.pct >= 90.0 {
            c.alpha = base * pulse(self.pulse_t0);
        }
        c.stroke_circle(cx, cy, r, 4.5, t.track);
        let frac = self.ring.value();
        let (c1, c2, glow) = t.level(five.level);
        if five.pct_i.is_some() {
            if stale {
                if let Some(b) = c.solid_brush(t.faint) {
                    c.arc(cx, cy, r, frac, 4.5, &b);
                }
            } else {
                if let Some(g) = c.solid_brush(glow.alpha(0.35)) {
                    c.arc(cx, cy, r, frac, 8.0, &g);
                }
                if let Some(b) = c.linear(cx - 17.0, cy - 17.0, cx + 17.0, cy + 17.0, &[(0.0, c1), (1.0, c2)]) {
                    c.arc(cx, cy, r, frac, 4.5, &b);
                }
            }
        }
        c.alpha = base;
        let big = five.pct_text();
        let bx = 12.0 + 34.0 + 10.0;
        let bw = c.g.text_width(&big, 19.0, W700).ceil() + 1.0;
        if let Some(b) = Self::num_brush(c, t, five.level, bx, 0.0, bw, h, stale, false) {
            c.text(&big, 19.0, W700, bx, 0.0, bw, h, Align::Left, &b, &[]);
        }
        let mx = bx + bw + 10.0;
        let lh = 14.4;
        let my = (h - 2.0 * lh) / 2.0;
        let l1 = format!("{} · {}", five.short, five.reset_short);
        let n = five.short.encode_utf16().count() as u32;
        if let Some(sub) = c.solid_brush(t.sub) {
            c.text(&l1, 11.5, 400, mx, my, 120.0, lh, Align::Left, &sub, &[(0, n, t.fg, W600)]);
            if let Some(wk) = rows.get(1) {
                c.text(&format!("{} {}", wk.short, wk.pct_text()), 11.5, 400, mx, my + lh, 120.0, lh, Align::Left, &sub, &[]);
            }
        }
    }

    fn draw_text(&self, c: &mut Canvas, t: &Theme, rows: &[Row], _w: f32, h: f32, stale: bool) {
        let five = &rows[0];
        Self::spark(c, t, 12.0, (h - 16.0) / 2.0, 16.0);
        let mut x = 12.0 + 16.0 + 8.0;
        let big = five.pct_text();
        let bw = c.g.text_width(&big, 14.0, W700).ceil() + 1.0;
        if let Some(b) = Self::num_brush(c, t, five.level, x, 0.0, bw, h, stale, false) {
            c.text(&big, 14.0, W700, x, 0.0, bw, h, Align::Left, &b, &[]);
        }
        x += bw + 8.0;
        let sub = c.solid_brush(t.sub);
        let fg = c.solid_brush(t.fg);
        if let Some(s) = &sub {
            let rw = c.text(&five.reset_short, 12.5, 400, x, 0.0, 120.0, h, Align::Left, s, &[]);
            x += rw + 8.0;
            if let Some(wk) = rows.get(1) {
                let dw = c.text("·", 12.5, 400, x, 0.0, 20.0, h, Align::Left, s, &[]);
                x += dw + 8.0;
                if let Some(f) = &fg {
                    let label = format!("{} {}", wk.short, wk.pct_text());
                    let start = wk.short.encode_utf16().count() as u32 + 1;
                    let len = label.encode_utf16().count() as u32 - start;
                    let col = if stale { t.sub } else { t.fg };
                    c.text(&label, 12.5, 400, x, 0.0, 120.0, h, Align::Left, f, &[(start, len, col, W700)]);
                }
            }
        }
    }

    fn draw_message(&self, c: &mut Canvas, t: &Theme, m: &Model, w: f32, h: f32) {
        let (title, sub) = match m.status.as_str() {
            "loading" => ("Claude usage", "Loading…".to_string()),
            "signin" => ("Sign in to see limits", "Run claude → /login".to_string()),
            "expired" => ("Sign-in expired", "Run claude → /login".to_string()),
            "error" => ("Claude usage", if m.error.is_empty() { "Can't reach Anthropic".to_string() } else { m.error.clone() }),
            _ => ("Claude usage", String::new()),
        };
        Self::spark(c, t, 12.0, (h - 16.0) / 2.0, 16.0);
        let x = 12.0 + 16.0 + 10.0;
        let tw = w - x - 12.0;
        let lh = 16.0;
        let y = (h - 2.0 * lh) / 2.0;
        if let Some(b) = c.solid_brush(t.fg) {
            c.text(title, 12.5, W600, x, y, tw, lh, Align::Left, &b, &[]);
        }
        if let Some(b) = c.solid_brush(t.sub) {
            c.text(&sub, 11.5, 400, x, y + lh, tw, lh, Align::Left, &b, &[]);
        }
    }
}

/// Fields that change what the pill shows (to skip redundant redraws).
pub fn pill_key(m: &Model, style: &str, dim: bool, light: bool) -> String {
    let rows: Vec<String> = m.rows.iter().take(2)
        .map(|r| format!("{}|{:?}|{}|{:?}|{}", r.short, r.pct_i, r.pct, r.level, r.reset_short)).collect();
    format!("{style}|{dim}|{light}|{}|{}|{:?}|{}|{}", m.status, m.error, m.fresh.state, m.fresh.label, rows.join(";"))
}

#[allow(dead_code)]
pub fn color_of(t: &Theme, l: Level) -> Rgba {
    t.level(l).1
}
