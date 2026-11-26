//! The details card: header, limits (bars or rings), pace forecast, 5-hour timeline,
//! Claude Code activity and footer, or the settings panel. A port of card.html.
//! Coordinates are DIPs; the card is 360 wide and as tall as its content.

use super::anim::{Tween, pulse};
use super::gfx::{Align, Canvas, Gfx};
use super::theme::Theme;
use crate::model::{FcLevel, FreshState, Level, Model, Row};
use super::card_extra::{self, CardCtx};
use serde_json::Value;
use std::cell::{Cell, RefCell};
use std::time::Instant;
use windows::Win32::Graphics::Direct2D::ID2D1Brush;

pub const CARD_W: f32 = 360.0;
pub(crate) const PAD_X: f32 = 20.0;
const PAD_TOP: f32 = 18.0;
const PAD_BOTTOM: f32 = 14.0;
pub(crate) const CW: f32 = CARD_W - 2.0 * PAD_X; // 320
const BARS_ROW: f32 = 76.0;
const RINGS_H: f32 = 179.0;
const TL_W: f32 = 320.0;
const TL_H: f32 = 96.0;
const TL_TOP: f32 = 8.0;
const TL_BOT: f32 = 74.0;

const W500: u32 = 500;
const W600: u32 = 600;
const W650: u32 = 650;
const W700: u32 = 700;
const W800: u32 = 800;

/// Things on the card you can hover or click.
#[derive(Clone, Debug, PartialEq)]
pub enum Hit {
    View,
    Gear,
    WeekPrev,
    WeekNext,
    Cell(usize, usize),
    Set(&'static str, Value),
    Done,
    Folder,
    Quit,
    StatsTip,
    PeakTip,
    StaleTip,
}

/// A hit-test rectangle (x, y, w, h in DIPs) and what it is.
type HitRect = ((f32, f32, f32, f32), Hit);

pub struct CardView {
    pub view: String, // "bars" | "rings"
    pub acrylic: bool,
    pub settings_open: bool,
    /// Leave out the activity section when the full card wouldn't fit on the screen.
    pub compact: bool,
    pub hm_week: Cell<usize>,
    pub hover: RefCell<Option<Hit>>,
    pub cursor: Cell<Option<(f32, f32)>>,
    hits: RefCell<Vec<HitRect>>,
    arcs: Vec<Tween>,
    counts: Vec<Tween>,
    line: Tween,
    t0: Instant,
    /// Where the "last fetch" point was drawn (x, y, fresh, colour) for the halo overlay.
    point: Cell<Option<(f32, f32, bool, super::gfx::Rgba)>>,
}

fn empty_text(m: &Model) -> (&'static str, String) {
    match m.status.as_str() {
        "signin" => ("Not signed in yet", "Open a terminal, run claude and type /login once. Your real limits appear here.".into()),
        "expired" => ("Your sign-in expired", "Run claude and /login again.".into()),
        "loading" => ("Loading…", String::new()),
        _ => ("Can't reach Anthropic", m.error.clone()),
    }
}

impl CardView {
    pub fn new() -> CardView {
        CardView {
            view: "rings".into(), acrylic: true, settings_open: false, compact: false, hm_week: Cell::new(0),
            hover: RefCell::new(None), cursor: Cell::new(None), hits: RefCell::new(Vec::new()),
            arcs: Vec::new(), counts: Vec::new(),
            line: Tween::new(1200), t0: Instant::now(), point: Cell::new(None),
        }
    }

    pub(crate) fn add_hit(&self, r: (f32, f32, f32, f32), h: Hit) {
        self.hits.borrow_mut().push((r, h));
    }

    /// What's under (x, y) DIP, from the last draw (last added wins).
    pub fn hit_at(&self, x: f32, y: f32) -> Option<Hit> {
        self.hits.borrow().iter().rev()
            .find(|((rx, ry, rw, rh), _)| x >= *rx && x < rx + rw && y >= *ry && y < ry + rh)
            .map(|(_, h)| h.clone())
    }

    /// Rectangles of the hover-tooltip areas (stats tiles, last week's peak).
    pub fn tip_rects(&self) -> Vec<((f32, f32, f32, f32), Hit)> {
        self.hits.borrow().iter().filter(|(_, h)| matches!(h, Hit::StatsTip | Hit::PeakTip | Hit::StaleTip)).cloned().collect()
    }

    pub fn sync(&mut self, m: &Model) {
        let n = m.rows.len();
        while self.arcs.len() < n {
            self.arcs.push(Tween::new(1100));
            self.counts.push(Tween::new(900));
        }
        for (i, r) in m.rows.iter().enumerate() {
            let frac = if self.view == "bars" { (r.pct.clamp(0.0, 100.0).max(2.0) / 100.0) as f32 } else { (r.pct.clamp(0.0, 100.0) / 100.0) as f32 };
            self.arcs[i].set(frac, false);
            self.counts[i].set(r.pct_i.unwrap_or(0) as f32, false);
        }
        self.line.set(1.0, false);
    }

    /// The card just opened (or switched view): sweep, count up and draw the line in.
    pub fn replay(&mut self, m: &Model) {
        self.sync(m);
        for t in self.arcs.iter_mut().chain(self.counts.iter_mut()) {
            t.replay_from(0.0);
        }
        self.line.replay_from(0.0);
        self.t0 = Instant::now();
    }

    /// Animations that change the card's content (need a full redraw per frame).
    pub fn content_animating(&self, m: &Model) -> bool {
        let pulse = self.view == "rings" && m.rows.iter().any(|r| r.pct >= 90.0);
        pulse || self.line.running() || self.arcs.iter().chain(self.counts.iter()).any(Tween::running)
    }

    pub fn animating(&self, m: &Model) -> bool {
        self.content_animating(m)
    }

    /// Where the last-fetch point goes: (x, y, fresh, level colour). The app shows it with
    /// a compositor-animated halo layer (gfx::Halo).
    pub fn point(&self) -> Option<(f32, f32, bool, super::gfx::Rgba)> {
        self.point.get()
    }

    fn limits_h(&self, g: &Gfx, m: &Model) -> f32 {
        if m.rows.is_empty() {
            let (_, s) = empty_text(m);
            let sh = if s.is_empty() { 0.0 } else { 4.0 + g.wrapped_height(&s, 12.5, 400, CW, 18.75) };
            return 6.0 + 19.0 + sh + 2.0;
        }
        if self.view == "bars" { BARS_ROW * m.rows.len() as f32 } else { RINGS_H }
    }

    pub fn height(&self, g: &Gfx, m: &Model, ctx: &CardCtx) -> f32 {
        if self.settings_open {
            return PAD_TOP + 30.0 + 14.0 + card_extra::settings_h() + PAD_BOTTOM;
        }
        let mut y = PAD_TOP + 30.0 + 14.0 + self.limits_h(g, m);
        if m.forecast.is_some() {
            y += 12.0 + 18.0;
        }
        if m.timeline.is_some() {
            y += 16.0 + 15.0 + 6.0 + TL_H;
        }
        if !self.compact {
            y += card_extra::activity_h(ctx.stats);
        }
        y + 14.0 + 15.0 + PAD_BOTTOM
    }

    pub fn draw(&self, c: &mut Canvas, t: &Theme, m: &Model, ctx: &CardCtx) {
        self.hits.borrow_mut().clear();
        let h = self.height(c.g, m, ctx);
        // background
        if !self.acrylic {
            c.fill_rect(0.0, 0.0, CARD_W, h, 0.0, t.body);
        }
        let (dx, dy) = (155f32.to_radians().sin(), -155f32.to_radians().cos());
        let len = (CARD_W * dx).abs() + (h * dy).abs();
        let (cx, cy) = (CARD_W / 2.0, h / 2.0);
        if let Some(b) = c.linear(cx - dx * len / 2.0, cy - dy * len / 2.0, cx + dx * len / 2.0, cy + dy * len / 2.0,
                                  &[(0.0, t.tint_a), (1.0, t.tint_b)]) {
            c.fill_rect_brush(0.0, 0.0, CARD_W, h, 0.0, &b);
        }
        if let Some(b) = c.radial(0.15 * CARD_W, -0.1 * h, 1.2 * CARD_W, 0.6 * h, &[(0.0, t.sheen), (0.6, t.sheen.alpha(0.0))]) {
            c.fill_rect_brush(0.0, 0.0, CARD_W, h, 0.0, &b);
        }

        self.point.set(None);
        let mut y = PAD_TOP;
        self.draw_header(c, t, m, y);
        y += 30.0 + 14.0;
        if self.settings_open {
            self.draw_settings(c, t, ctx, y);
            return;
        }
        let stale = m.fresh.state == FreshState::Stale;
        c.alpha = if stale { 0.5 } else { 1.0 };
        if m.rows.is_empty() {
            self.draw_empty(c, t, m, y);
        } else if self.view == "bars" {
            for (i, r) in m.rows.iter().enumerate() {
                self.draw_bar_row(c, t, r, i, y + i as f32 * BARS_ROW, stale);
            }
        } else {
            self.draw_rings(c, t, &m.rows, y, stale);
        }
        c.alpha = 1.0;
        y += self.limits_h(c.g, m);
        if let Some(f) = &m.forecast {
            y += 12.0;
            self.draw_forecast(c, t, &f.text, f.level, y);
            y += 18.0;
        }
        if m.timeline.is_some() {
            y += 16.0;
            self.draw_timeline(c, t, m, y);
            y += 15.0 + 6.0 + TL_H;
        }
        if !self.compact {
            self.draw_activity(c, t, ctx, y);
            y += card_extra::activity_h(ctx.stats);
        }
        y += 14.0;
        self.draw_footer(c, t, m, y);
        self.draw_cell_tip(c, ctx, h);
    }

    // ------------------------------------------------------------ header
    fn draw_header(&self, c: &Canvas, t: &Theme, m: &Model, y: f32) {
        spark(c, t, PAD_X, y + 5.0, 20.0);
        let tx = PAD_X + 20.0 + 9.0;
        let Some(fg) = c.solid_brush(t.fg) else { return };
        let tw = c.text("Claude usage", 16.5, W700, tx, y, 200.0, 30.0, Align::Left, &fg, &[]);
        if let Some(plan) = &m.plan {
            let pw = c.g.text_width(plan, 11.0, W600).ceil() + 16.0;
            let (px, ch) = (tx + tw + 9.0, 18.5);
            c.fill_rect(px, y + (30.0 - ch) / 2.0, pw, ch, ch / 2.0, t.chip);
            if let Some(sub) = c.solid_brush(t.sub) {
                c.text(plan, 11.0, W600, px, y, pw, 30.0, Align::Center, &sub, &[]);
            }
        }
        // gear (settings), rightmost
        let (gx, gy) = (CARD_W - PAD_X - 30.0, y);
        let ghover = self.hover.borrow().as_ref() == Some(&Hit::Gear) || self.settings_open;
        if ghover {
            c.fill_rect(gx, gy, 30.0, 30.0, 9.0, t.chip);
        }
        if let Some(b) = c.solid_brush(if ghover { t.fg } else { t.sub }) {
            gear(c, gx + 15.0, gy + 15.0, &b);
        }
        self.add_hit((gx, gy, 30.0, 30.0), Hit::Gear);
        // view toggle, left of the gear
        let (bx, by, bw, bh) = (gx - 9.0 - 30.0, y, 30.0, 30.0);
        self.add_hit((bx, by, bw, bh), Hit::View);
        let vhover = self.hover.borrow().as_ref() == Some(&Hit::View);
        if vhover {
            c.fill_rect(bx, by, bw, bh, 9.0, t.chip);
        }
        let col = if vhover { t.fg } else { t.sub };
        let Some(b) = c.solid_brush(col) else { return };
        // icon shows the *other* view, 17px in a 24-unit box
        let k = 17.0 / 24.0;
        let (ox, oy) = (bx + (bw - 17.0) / 2.0, by + (bh - 17.0) / 2.0);
        let p = |x: f32, y: f32| (ox + x * k, oy + y * k);
        if self.view == "bars" {
            let (cx, cy) = p(12.0, 12.0);
            c.stroke_circle_brush(cx, cy, 8.0 * k, 2.2 * k, &b);
            c.arc(cx, cy, 8.0 * k, 0.25, 2.2 * k, &b);
        } else {
            for (y0, x1) in [(7.0, 20.0), (12.0, 15.0), (17.0, 18.0)] {
                let (a, ay) = p(4.0, y0);
                let (bx1, _) = p(x1, y0);
                c.line(a, ay, bx1, ay, 2.4 * k, &b);
            }
        }
    }

    fn draw_empty(&self, c: &Canvas, t: &Theme, m: &Model, y: f32) {
        let (title, sub) = empty_text(m);
        if let Some(b) = c.solid_brush(t.fg) {
            c.text(title, 14.0, W650, PAD_X, y + 6.0, CW, 19.0, Align::Left, &b, &[]);
        }
        if !sub.is_empty()
            && let Some(b) = c.solid_brush(t.sub) {
                c.text_wrapped(&sub, 12.5, 400, PAD_X, y + 6.0 + 19.0 + 4.0, CW, 18.75, &b);
            }
    }

    // ------------------------------------------------------------ limits
    fn grad_or_sub(c: &Canvas, t: &Theme, lv: Level, x: f32, y: f32, w: f32, h: f32, stale: bool) -> Option<ID2D1Brush> {
        if stale {
            return c.solid_brush(t.sub);
        }
        let (c1, c2, _) = t.level(lv);
        c.linear(x, y + h * 0.2, x + w, y + h * 0.8, &[(0.0, c1), (1.0, c2)])
    }

    fn count_text(&self, r: &Row, i: usize) -> Option<String> {
        r.pct_i.map(|_| format!("{}", self.counts.get(i).map(|t| t.value().round() as i64).unwrap_or(0)))
    }

    fn draw_bar_row(&self, c: &Canvas, t: &Theme, r: &Row, i: usize, y: f32, stale: bool) {
        let (c1, c2, glow) = t.level(r.level);
        // icon
        let ig = if stale { c.solid_brush(t.sub) } else { c.linear(PAD_X, y + 2.0, PAD_X + 18.0, y + 20.0, &[(0.0, c1), (1.0, c2)]) };
        if let Some(b) = ig {
            row_icon(c, &r.key, PAD_X, y + 2.0, 18.0, &b);
        }
        let tx = PAD_X + 26.0 + 8.0;
        if let Some(fg) = c.solid_brush(t.fg) {
            c.text(&r.title, 15.0, W700, tx, y, 180.0, 20.0, Align::Left, &fg, &[]);
        }
        if let Some(sub) = c.solid_brush(t.sub) {
            let s = match r.prev_peak {
                Some(p) => format!("{} · last week {p}%", r.sub),
                None => r.sub.clone(),
            };
            let sw = c.text(&s, 11.5, 400, tx, y + 20.0, 200.0, 15.5, Align::Left, &sub, &[]);
            if r.prev_peak.is_some() {
                self.add_hit((tx, y + 20.0, sw, 15.5), Hit::PeakTip);
            }
            c.text(&r.reset_card, 11.0, 400, PAD_X, y + 29.0, CW, 14.6, Align::Right, &sub, &[]);
        }
        // big number
        let (txt, pct_sign) = match self.count_text(r, i) {
            Some(n) => (format!("{n}%"), true),
            None => ("—".to_string(), false),
        };
        let w = c.g.text_width(&txt, 26.0, W800).ceil() + 2.0;
        if let Some(b) = Self::grad_or_sub(c, t, r.level, PAD_X + CW - w, y, w, 26.0, stale) {
            let _ = pct_sign;
            c.text(&txt, 26.0, W800, PAD_X, y, CW, 26.0, Align::Right, &b, &[]);
        }
        // bar
        let by = y + 44.0 + 10.0;
        c.fill_rect(PAD_X, by, CW, 8.0, 4.0, t.track);
        if r.pct_i.is_some() {
            let fw = CW * self.arcs.get(i).map(Tween::value).unwrap_or(0.0).clamp(0.0, 1.0);
            if stale {
                c.fill_rect(PAD_X, by, fw, 8.0, 4.0, t.faint);
            } else {
                c.fill_rect(PAD_X - 2.0, by - 2.0, fw + 4.0, 12.0, 6.0, glow.alpha(0.2));
                if let Some(b) = c.linear(PAD_X, 0.0, PAD_X + fw.max(1.0), 0.0, &[(0.0, c1), (1.0, c2)]) {
                    c.fill_rect_brush(PAD_X, by, fw, 8.0, 4.0, &b);
                }
            }
        }
    }

    fn draw_rings(&self, c: &mut Canvas, t: &Theme, rows: &[Row], y: f32, stale: bool) {
        let n = rows.len().max(1) as f32;
        let margin = ((CW - n * 140.0) / n / 2.0).max(0.0);
        let base = c.alpha;
        for (i, r) in rows.iter().enumerate() {
            let x = PAD_X + margin + i as f32 * (140.0 + 2.0 * margin);
            let (cx, cy) = (x + 70.0, y + 60.0);
            let (c1, c2, glow) = t.level(r.level);
            c.alpha = if r.pct >= 90.0 && !stale { base * pulse(self.t0) } else { base };
            c.stroke_circle(cx, cy, 51.0, 11.0, t.track);
            if r.pct_i.is_some() {
                let frac = self.arcs.get(i).map(Tween::value).unwrap_or(0.0);
                if stale {
                    if let Some(b) = c.solid_brush(t.faint) {
                        c.arc(cx, cy, 51.0, frac, 11.0, &b);
                    }
                } else {
                    if let Some(g) = c.solid_brush(glow.alpha(0.28)) {
                        c.arc(cx, cy, 51.0, frac, 17.0, &g);
                    }
                    if let Some(b) = c.linear(cx - 60.0, cy - 60.0, cx + 60.0, cy + 60.0, &[(0.0, c1), (1.0, c2)]) {
                        c.arc(cx, cy, 51.0, frac, 11.0, &b);
                    }
                }
            }
            c.alpha = base;
            // value: number 27px + "%" 15px at 80%, centred together on one baseline
            if let Some(fg) = c.solid_brush(t.fg) {
                match self.count_text(r, i) {
                    Some(nstr) => {
                        let s = format!("{nstr}%");
                        let nlen = nstr.encode_utf16().count() as u32;
                        c.text_sized(&s, 27.0, W700, x, y, 140.0, 120.0, Align::Center, &fg, &[(nlen, 1, t.fg.alpha(0.8), W600, 15.0)]);
                    }
                    None => {
                        c.text("—", 27.0, W700, x, y, 140.0, 120.0, Align::Center, &fg, &[]);
                    }
                }
                c.text(&r.title, 14.0, W650, x, y + 128.0, 140.0, 19.0, Align::Center, &fg, &[]);
            }
            if let Some(sub) = c.solid_brush(t.sub) {
                c.text(&r.reset_card, 11.5, 400, x, y + 149.0, 140.0, 15.0, Align::Center, &sub, &[]);
            }
            if let (Some(p), Some(faint)) = (r.prev_peak, c.solid_brush(t.faint)) {
                c.text(&format!("Last week peaked at {p}%"), 11.0, 400, x, y + 165.0, 140.0, 13.0, Align::Center, &faint, &[]);
                self.add_hit((x, y + 165.0, 140.0, 13.0), Hit::PeakTip);
            }
        }
    }

    // ------------------------------------------------------------ forecast
    fn draw_forecast(&self, c: &Canvas, t: &Theme, text: &str, lv: FcLevel, y: f32) {
        let muted = lv == FcLevel::Muted;
        let level = match lv { FcLevel::Warn => Level::Warn, FcLevel::Crit => Level::Crit, _ => Level::Ok };
        let (c1, c2, glow) = t.level(level);
        let (dx, dy) = (PAD_X + 3.5, y + 9.0);
        if muted {
            c.fill_circle(dx, dy, 3.5, t.faint);
        } else {
            c.fill_circle(dx, dy, 6.0, glow.alpha(0.3));
            c.fill_circle(dx, dy, 3.5, c2);
        }
        let tx = PAD_X + 7.0 + 7.0;
        let tw = c.g.text_width(text, 12.5, if muted { W500 } else { W600 }).ceil() + 2.0;
        let b = if muted { c.solid_brush(t.sub) } else { c.linear(tx, y, tx + tw, y + 18.0, &[(0.0, c1), (1.0, c2)]) };
        if let Some(b) = b {
            c.text(text, 12.5, if muted { W500 } else { W600 }, tx, y, CW - 14.0, 18.0, Align::Left, &b, &[]);
        }
    }

    // ------------------------------------------------------------ timeline
    fn draw_timeline(&self, c: &Canvas, t: &Theme, m: &Model, y: f32) {
        let Some(tl) = &m.timeline else { return };
        // header: label + legend
        if let Some(f) = c.solid_brush(t.faint) {
            c.text("5-HOUR WINDOW", 11.0, W600, PAD_X, y, 200.0, 15.0, Align::Left, &f, &[]);
        }
        let items: Vec<(&str, bool)> = if tl.proj.is_some() { vec![("Actual", false), ("Projected", true)] } else { vec![("Actual", false)] };
        let widths: Vec<f32> = items.iter().map(|(s, _)| c.g.text_width(s, 11.0, 400).ceil()).collect();
        let total: f32 = widths.iter().map(|w| 10.0 + 14.0 + 5.0 + w).sum();
        let mut lx = PAD_X + CW - total;
        let sub = c.solid_brush(t.sub);
        for ((label, dashed), w) in items.iter().zip(widths) {
            lx += 10.0;
            let ly = y + 8.0;
            if *dashed {
                if let (Some(b), Some(st)) = (&sub, c.g.dashed(&[3.0, 2.0], 1.5)) {
                    c.line_styled(lx, ly, lx + 14.0, ly, 1.5, b, Some(&st));
                }
            } else if let Some(b) = c.solid_brush(t.ok.1) {
                c.line_styled(lx, ly, lx + 14.0, ly, 2.5, &b, None);
            }
            lx += 14.0 + 5.0;
            if let Some(b) = &sub {
                c.text(label, 11.0, 400, lx, y, w + 2.0, 15.0, Align::Left, b, &[]);
            }
            lx += w;
        }

        // chart, in a 320 x 96 box
        let ox = PAD_X;
        let oy = y + 15.0 + 6.0;
        let fresh = m.fresh.state == FreshState::Fresh;
        let (c1, c2, glow) = t.level(tl.level);
        let xf = |x: f64| ox + x as f32 * TL_W;
        let yf = |p: f64| oy + TL_BOT - (TL_BOT - TL_TOP) * (p.min(100.0) as f32) / 100.0;
        if let Some(lb) = c.solid_brush(t.line) {
            for x in [0.2, 0.4, 0.6, 0.8] {
                c.line_styled(xf(x), oy + TL_TOP, xf(x), oy + TL_BOT, 1.0, &lb, None);
            }
        }
        if let (Some(fb), Some(st)) = (c.solid_brush(t.faint), c.g.dashed(&[2.0, 4.0], 1.0)) {
            c.line_styled(ox, oy + TL_TOP, ox + TL_W, oy + TL_TOP, 1.0, &fb, Some(&st));
        }
        if let Some(fb) = c.solid_brush(t.faint) {
            c.text("100%", 10.0, 400, ox, oy + TL_TOP - 2.0 - 10.0, TL_W, 13.0, Align::Right, &fb, &[]);
        }
        let pts: Vec<(f32, f32)> = tl.points.iter().map(|(x, p)| (xf(*x), yf(*p))).collect();
        if pts.len() >= 2 {
            // area under the actual line
            let mut area = pts.clone();
            area.push((pts[pts.len() - 1].0, oy + TL_BOT));
            area.push((ox, oy + TL_BOT));
            if let Some(b) = c.linear(0.0, oy + TL_TOP, 0.0, oy + TL_BOT, &[(0.0, c1.alpha(0.35)), (1.0, c1.alpha(0.0))]) {
                c.fill_polygon(&area, &b);
            }
            // the actual line draws in when the card opens
            let shown = partial(&pts, ease_line(self.line.value()));
            if let Some(g) = c.solid_brush(glow.alpha(0.22)) {
                c.polyline(&shown, 7.0, &g);
            }
            if let Some(b) = c.linear(ox, 0.0, ox + TL_W, 0.0, &[(0.0, c1), (1.0, c2)]) {
                c.polyline(&shown, 2.6, &b);
            }
        }
        let (lx, ly, nx) = (xf(tl.last_x), yf(tl.now_pct), xf(tl.now_x));
        // projection: an estimate, so thin, dashed and muted
        if let (Some(p), Some(sb)) = (tl.proj, c.solid_brush(t.sub)) {
            if let Some(st) = c.g.dashed(&[3.0, 4.0], 1.6) {
                c.line_styled(lx, ly, xf(p.0), yf(p.1), 1.6, &sb, Some(&st));
            }
            if tl.hit {
                c.stroke_circle_brush(xf(p.0), yf(100.0), 3.5, 1.4, &sb);
            }
        }
        if nx - lx > 3.0
            && let (Some(fb), Some(st)) = (c.solid_brush(t.faint), c.g.dashed(&[1.0, 3.0], 1.0)) {
                c.line_styled(nx, oy + TL_TOP, nx, oy + TL_BOT, 1.0, &fb, Some(&st));
            }
        // the point and its pulsing halo are compositor layers (gfx::Halo), placed by the app
        self.point.set(Some((lx, ly, fresh, c2)));
        // axis labels
        let ly2 = oy + TL_BOT + 16.0 - 10.0;
        if let Some(fb) = c.solid_brush(t.faint) {
            if nx - ox > 70.0 {
                c.text(&tl.start_label, 10.0, 400, ox, ly2, 100.0, 13.0, Align::Left, &fb, &[]);
            }
            if nx < ox + TL_W - 70.0 {
                c.text(&tl.end_label, 10.0, 400, ox + TL_W - 100.0, ly2, 100.0, 13.0, Align::Right, &fb, &[]);
            }
        }
        if let Some(sb) = c.solid_brush(t.sub) {
            let cxn = nx.clamp(ox + 14.0, ox + TL_W - 14.0);
            c.text("now", 10.0, 400, cxn - 30.0, ly2, 60.0, 13.0, Align::Center, &sb, &[]);
        }
    }

    // ------------------------------------------------------------ footer
    fn draw_footer(&self, c: &Canvas, t: &Theme, m: &Model, y: f32) {
        let stale = m.fresh.state == FreshState::Stale;
        let (txt, col) = if stale {
            (format!("{} · {}", m.fresh.label, m.fresh.reason), t.warn.1)
        } else {
            (m.fresh.label.clone(), t.sub)
        };
        if let Some(b) = c.solid_brush(col) {
            let w = c.text(&txt, 11.0, 400, PAD_X, y, 240.0, 15.0, Align::Left, &b, &[]);
            if stale {
                self.add_hit((PAD_X, y, w, 15.0), Hit::StaleTip);
            }
        }
        let ivl = format!("every {}", m.interval_label);
        let w = c.g.text_width(&ivl, 11.0, 400).ceil() + 1.0;
        if let Some(b) = c.solid_brush(t.sub) {
            c.text(&ivl, 11.0, 400, PAD_X + CW - w, y, w, 15.0, Align::Right, &b, &[]);
        }
        let dx = PAD_X + CW - w - 6.0 - 3.0;
        let (dot, glow) = if m.fresh.state == FreshState::Fresh { (t.ok.1, t.ok.2) } else { (t.warn.1, t.warn.2) };
        c.fill_circle(dx, y + 7.5, 6.0, glow.alpha(0.35));
        c.fill_circle(dx, y + 7.5, 3.0, dot);
    }
}

fn ease_line(v: f32) -> f32 {
    v.clamp(0.0, 1.0)
}

/// The first `frac` of a polyline, by length.
fn partial(pts: &[(f32, f32)], frac: f32) -> Vec<(f32, f32)> {
    if frac >= 0.999 || pts.len() < 2 {
        return pts.to_vec();
    }
    let seg = |a: (f32, f32), b: (f32, f32)| ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt();
    let total: f32 = pts.windows(2).map(|w| seg(w[0], w[1])).sum();
    let mut left = total * frac.max(0.0);
    let mut out = vec![pts[0]];
    for w in pts.windows(2) {
        let l = seg(w[0], w[1]);
        if l >= left {
            let k = if l > 0.0 { left / l } else { 0.0 };
            out.push((w[0].0 + (w[1].0 - w[0].0) * k, w[0].1 + (w[1].1 - w[0].1) * k));
            return out;
        }
        left -= l;
        out.push(w[1]);
    }
    out
}

/// A settings gear (17 px): a ring with eight teeth and a hub.
fn gear(c: &Canvas, cx: f32, cy: f32, b: &ID2D1Brush) {
    c.stroke_circle_brush(cx, cy, 5.0, 1.7, b);
    c.stroke_circle_brush(cx, cy, 2.2, 1.6, b);
    for i in 0..8 {
        let a = i as f32 * std::f32::consts::TAU / 8.0;
        let (s, co) = (a.sin(), a.cos());
        c.line(cx + 5.6 * s, cy - 5.6 * co, cx + 7.8 * s, cy - 7.8 * co, 2.2, b);
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

/// Per-limit icons from card.html (24-unit boxes): stopwatch, bar chart, arrow.
fn row_icon(c: &Canvas, key: &str, x: f32, y: f32, size: f32, b: &ID2D1Brush) {
    let k = size / 24.0;
    let p = |px: f32, py: f32| (x + px * k, y + py * k);
    match key {
        "five_hour" => {
            let (cx, cy) = p(12.0, 13.0);
            c.stroke_circle_brush(cx, cy, 8.0 * k, 2.2 * k, b);
            let (ax, ay) = p(12.0, 13.0);
            let (bx, by) = p(15.0, 10.0);
            c.line(ax, ay, bx, by, 2.2 * k, b);
            let (ax, ay) = p(9.0, 2.0);
            let (bx, by) = p(15.0, 2.0);
            c.line(ax, ay, bx, by, 2.2 * k, b);
        }
        "seven_day" => {
            for (rx, ry, rh) in [(4.0, 13.0, 7.0), (10.0, 9.0, 11.0), (16.0, 5.0, 15.0)] {
                let (px, py) = p(rx, ry);
                c.fill_rect_brush(px, py, 4.0 * k, rh * k, 1.0 * k, b);
            }
        }
        _ => {
            for (a, bb) in [((5.0, 19.0), (15.0, 9.0)), ((15.0, 3.0), (15.0, 9.0)), ((12.0, 6.0), (18.0, 6.0))] {
                let (ax, ay) = p(a.0, a.1);
                let (bx, by) = p(bb.0, bb.1);
                c.line(ax, ay, bx, by, 2.2 * k, b);
            }
        }
    }
}

/// Does a card of `height_dip` at `scale` need compact mode to fit `avail_px`?
pub fn needs_compact(height_dip: f32, scale: f32, avail_px: i32) -> bool {
    (height_dip * scale).ceil() as i32 > avail_px
}

#[cfg(test)]
mod tests {
    use super::{needs_compact, partial};

    #[test]
    fn compact_only_when_the_card_would_not_fit() {
        assert!(!needs_compact(690.0, 1.75, 1700)); // the dev machine: fits
        assert!(needs_compact(690.0, 1.25, 700)); // 1366x768 at 125%: too tall
    }

    #[test]
    fn partial_line_by_length() {
        let pts = [(0.0, 0.0), (10.0, 0.0), (10.0, 10.0)];
        assert_eq!(partial(&pts, 1.0).len(), 3);
        let half = partial(&pts, 0.5);
        assert_eq!(half.last().copied(), Some((10.0, 0.0)));
        let q = partial(&pts, 0.25);
        assert_eq!(q.last().copied(), Some((5.0, 0.0)));
    }
}
