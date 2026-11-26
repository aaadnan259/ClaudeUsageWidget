//! Card parts added in phase 3: the Claude Code activity section (stat tiles + 4-week
//! heatmap with paging and a hover tooltip) and the settings panel. Ports of card.html.

use super::card::{CW, CardView, Hit, PAD_X};
use super::gfx::{Align, Canvas, Rgba};
use super::theme::Theme;
use crate::data::logs::Stats;
use crate::model::fmt_tokens;
use serde_json::{Map, Value, json};

pub struct CardCtx<'a> {
    pub stats: Option<&'a Stats>,
    pub cfg: &'a Map<String, Value>,
    pub autostart: bool,
}

pub const STATS_TIP: &str = "Claude Code only, from local logs: input + output + cache-write tokens (cache reads excluded). \
Responses = model responses, including tool steps. Tokens/min = average over the last 10 minutes. \
claude.ai chats are not included here, but do count toward your plan limits above.";
pub const STALE_TIP: &str = "Showing the last known values until the next successful update.";
pub const PEAK_TIP: &str = "Highest weekly % the widget recorded in your previous weekly window (it only records while it is running).";

const W600: u32 = 600;
const W700: u32 = 700;
const TILE_H: f32 = 71.0;
const HM_LABEL: f32 = 26.0;
const HM_GAP: f32 = 2.0;

pub fn cell_size() -> f32 {
    (CW - HM_LABEL - HM_GAP * 24.0) / 24.0
}

/// Height of the activity section, from the divider down (excluding the footer margin).
pub fn activity_h(stats: Option<&Stats>) -> f32 {
    let head = 16.0 + 1.0 + 16.0 + 15.0 + 10.0;
    if stats.is_none() {
        return head + 34.0;
    }
    let hm = 7.0 * cell_size() + 6.0 * HM_GAP;
    head + TILE_H + 12.0 + 20.0 + 6.0 + hm + 4.0 + 13.0
}

/// Settings rows: (label, description, control).
enum Ctl {
    Seg(&'static str, &'static [(&'static str, &'static str)]),
    Switch(&'static str),
}

const ROWS: &[(&str, &str, Ctl)] = &[
    ("Pill style", "", Ctl::Seg("pill_style", &[("bars", "Bars"), ("ring", "Ring"), ("text", "Text")])),
    ("Theme", "", Ctl::Seg("theme", &[("auto", "Auto"), ("dark", "Dark"), ("light", "Light")])),
    ("Start with Windows", "", Ctl::Switch("autostart")),
    ("Alerts at 75% and 90%", "Session and weekly limits", Ctl::Switch("alerts")),
    ("Alert when a limit resets", "", Ctl::Switch("alert_on_reset")),
    ("Alert when on pace to hit the limit", "Within the next 30 minutes", Ctl::Switch("alert_on_forecast")),
    ("Fade the pill when idle", "", Ctl::Switch("dim_when_idle")),
    ("Hide during fullscreen apps", "", Ctl::Switch("hide_in_fullscreen")),
    ("Taskbar button", "Takes effect after restart", Ctl::Switch("taskbar_button")),
    ("Refresh limits every", "", Ctl::Seg("limits_refresh_seconds", &[("30", "30s"), ("60", "1m"), ("120", "2m"), ("300", "5m")])),
];

fn row_h(desc: &str, ctl: &Ctl) -> f32 {
    let content = if !desc.is_empty() { 33.0 } else if matches!(ctl, Ctl::Seg(..)) { 28.0 } else { 22.0 };
    10.0 + content + 10.0 + 1.0
}

pub fn settings_h() -> f32 {
    ROWS.iter().map(|(_, d, c)| row_h(d, c)).sum::<f32>() + 14.0 + 33.0
}

fn current(ctx: &CardCtx, key: &str) -> Value {
    match key {
        "autostart" => json!(ctx.autostart),
        "alerts" => json!(ctx.cfg.get("alert_thresholds").and_then(Value::as_array).is_some_and(|a| !a.is_empty())),
        "theme" => ctx.cfg.get("theme").cloned().unwrap_or(json!("auto")),
        k => ctx.cfg.get(k).cloned().unwrap_or(Value::Null),
    }
}

impl CardView {
    // ------------------------------------------------------------ activity
    pub(crate) fn draw_activity(&self, c: &Canvas, t: &Theme, ctx: &CardCtx, mut y: f32) {
        y += 16.0;
        c.fill_rect(PAD_X, y, CW, 1.0, 0.0, t.line);
        y += 1.0 + 16.0;
        if let Some(f) = c.solid_brush(t.faint) {
            c.text("CLAUDE CODE ACTIVITY", 11.0, W600, PAD_X, y, CW, 15.0, Align::Left, &f, &[]);
        }
        y += 15.0 + 10.0;
        let Some(s) = ctx.stats else {
            c.fill_rect(PAD_X, y, (CW - 16.0) / 3.0, 34.0, 12.0, t.chip);
            if let Some(b) = c.solid_brush(t.sub) {
                c.text("Reading logs…", 11.0, 400, PAD_X + 11.0, y, 100.0, 34.0, Align::Left, &b, &[]);
            }
            return;
        };
        // stat tiles
        let sess = if s.active > 0 { format!("{} active session{}", s.active, if s.active > 1 { "s" } else { "" }) } else { "idle".into() };
        let tiles = [
            (fmt_tokens(s.today), "tokens today", format!("{} responses", s.msgs)),
            (fmt_tokens(s.week), "tokens, 7 days", String::new()),
            (fmt_tokens(s.burn), "tokens/min", sess),
        ];
        let tw = (CW - 16.0) / 3.0;
        for (i, (v, k, k2)) in tiles.iter().enumerate() {
            let x = PAD_X + i as f32 * (tw + 8.0);
            c.fill_rect(x, y, tw, TILE_H, 12.0, t.chip);
            if let Some(b) = c.solid_brush(t.fg) {
                c.text(v, 17.0, W700, x + 11.0, y + 9.0, tw - 22.0, 22.6, Align::Left, &b, &[]);
            }
            if let Some(b) = c.solid_brush(t.sub) {
                c.text(k, 11.0, 400, x + 11.0, y + 9.0 + 23.6, tw - 18.0, 14.6, Align::Left, &b, &[]);
            }
            if let Some(b) = c.solid_brush(t.faint) {
                c.text(k2, 11.0, 400, x + 11.0, y + 9.0 + 38.2, tw - 18.0, 14.6, Align::Left, &b, &[]);
            }
        }
        self.add_hit((PAD_X, y, CW, TILE_H), Hit::StatsTip);
        y += TILE_H + 12.0;

        // heatmap header: week label, total, paging buttons
        let weeks = &s.weeks;
        let wi = self.hm_week.get().min(weeks.len().saturating_sub(1));
        let wk = &weeks[wi];
        let fg = c.solid_brush(t.fg);
        let lw = fg.as_ref().map(|b| c.text(&wk.label, 11.0, W600, PAD_X, y, 200.0, 20.0, Align::Left, b, &[])).unwrap_or(0.0);
        let tot = if wk.total <= 0.0 { "no activity logged".to_string() } else { format!("{} tokens", fmt_tokens(wk.total)) };
        if let Some(b) = c.solid_brush(t.faint) {
            c.text(&tot, 11.0, 400, PAD_X + lw + 4.0 + 4.0, y, 200.0, 20.0, Align::Left, &b, &[]);
        }
        let oldest = weeks.iter().enumerate().filter(|(_, w)| w.total > 0.0).map(|(i, _)| i).max().unwrap_or(0);
        let prev_ok = wi < oldest;
        let next_ok = wi > 0;
        for (j, (glyph, ok, hit)) in [("‹", prev_ok, Hit::WeekPrev), ("›", next_ok, Hit::WeekNext)].into_iter().enumerate() {
            let bx = PAD_X + CW - 22.0 * (2 - j) as f32 - 4.0 * (1 - j) as f32;
            let base = if ok { 1.0 } else { 0.3 };
            let hover = ok && self.hover.borrow().as_ref() == Some(&hit);
            c.fill_rect(bx, y, 22.0, 20.0, 6.0, t.chip.alpha(base));
            if let Some(b) = c.solid_brush((if hover { t.fg } else { t.sub }).alpha(base)) {
                c.text(glyph, 13.0, 400, bx, y - 1.0, 22.0, 20.0, Align::Center, &b, &[]);
            }
            if ok {
                self.add_hit((bx, y, 22.0, 20.0), hit);
            }
        }
        y += 20.0 + 6.0;

        // grid: one colour scale across all weeks, so weeks can be compared by eye
        let max = weeks.iter().flat_map(|w| w.heat.iter().flat_map(|d| d.iter())).fold(1.0f64, |a, b| a.max(*b));
        let cs = cell_size();
        let hovered = match *self.hover.borrow() {
            Some(Hit::Cell(d, h)) => Some((d, h)),
            _ => None,
        };
        let faint = c.solid_brush(t.faint);
        for d in 0..7 {
            let ry = y + d as f32 * (cs + HM_GAP);
            if let Some(b) = &faint {
                let lab: String = wk.day_labels[d].chars().take(2).collect();
                c.text(&lab, 10.0, 400, PAD_X, ry - 2.0, HM_LABEL, cs + 4.0, Align::Left, b, &[]);
            }
            for h in 0..24 {
                let cx = PAD_X + HM_LABEL + h as f32 * (cs + HM_GAP);
                let v = wk.heat[d][h];
                let k = if v > 0.0 { (v / max).powf(0.55) as f32 } else { 0.0 };
                let col = if v > 0.0 { t.ok.0.alpha((18.0 + k * 82.0) / 100.0) } else { t.track };
                if k > 0.7 {
                    c.fill_rect(cx - 1.5, ry - 1.5, cs + 3.0, cs + 3.0, 4.5, t.ok.2.alpha(0.35));
                }
                if hovered == Some((d, h)) {
                    let g = cs * 0.25;
                    c.fill_rect(cx - g, ry - g, cs + 2.0 * g, cs + 2.0 * g, 4.0, col);
                    if let Some(b) = c.solid_brush(t.fg) {
                        let r = windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F { left: cx - g, top: ry - g, right: cx + cs + g, bottom: ry + cs + g };
                        let rr = windows::Win32::Graphics::Direct2D::D2D1_ROUNDED_RECT { rect: r, radiusX: 4.0, radiusY: 4.0 };
                        unsafe { c.dc.DrawRoundedRectangle(&rr, &b, 1.0, None) };
                    }
                } else {
                    c.fill_rect(cx, ry, cs, cs, 3.0, col);
                }
                self.add_hit((cx - HM_GAP / 2.0, ry - HM_GAP / 2.0, cs + HM_GAP, cs + HM_GAP), Hit::Cell(d, h));
            }
        }
        y += 7.0 * cs + 6.0 * HM_GAP + 4.0;
        if let Some(b) = &faint {
            let colw = (CW - HM_LABEL) / 4.0;
            for (i, l) in ["12a", "6a", "12p", "6p"].iter().enumerate() {
                c.text(l, 10.0, 400, PAD_X + HM_LABEL + i as f32 * colw, y, colw, 13.0, Align::Left, b, &[]);
            }
        }
    }

    /// The hover tooltip for a heatmap cell, drawn last so it sits on top.
    pub(crate) fn draw_cell_tip(&self, c: &Canvas, ctx: &CardCtx, card_h: f32) {
        let (Some(Hit::Cell(d, h)), Some((mx, my)), Some(s)) = (self.hover.borrow().clone(), self.cursor.get(), ctx.stats) else { return };
        let wk = &s.weeks[self.hm_week.get().min(s.weeks.len().saturating_sub(1))];
        let hh = format!("{} {}", if h % 12 == 0 { 12 } else { h % 12 }, if h < 12 { "AM" } else { "PM" });
        let txt = format!("{} {} · {} tokens", wk.day_dates[d], hh, fmt_tokens(wk.heat[d][h]));
        let w = c.g.text_width(&txt, 11.5, 400).ceil() + 16.0;
        let bh = 25.0;
        let x = (mx + 10.0).min(super::card::CARD_W - w - 6.0).max(6.0);
        let y = (my - 30.0).max(4.0).min(card_h - bh - 4.0);
        c.fill_rect(x, y, w, bh, 7.0, Rgba(10.0 / 255.0, 10.0 / 255.0, 14.0 / 255.0, 0.9));
        if let Some(b) = c.solid_brush(Rgba(1.0, 1.0, 1.0, 1.0)) {
            c.text(&txt, 11.5, 400, x, y, w, bh, Align::Center, &b, &[]);
        }
    }

    // ------------------------------------------------------------ settings
    pub(crate) fn draw_settings(&self, c: &Canvas, t: &Theme, ctx: &CardCtx, mut y: f32) {
        let fg = c.solid_brush(t.fg);
        let sub = c.solid_brush(t.sub);
        for (label, desc, ctl) in ROWS {
            let h = row_h(desc, ctl);
            let inner = h - 21.0;
            let cy = y + 10.0 + inner / 2.0;
            if let Some(b) = &fg {
                if desc.is_empty() {
                    c.text(label, 13.0, 400, PAD_X, y + 10.0, 220.0, inner, Align::Left, b, &[]);
                } else {
                    c.text(label, 13.0, 400, PAD_X, y + 10.0, 220.0, 17.3, Align::Left, b, &[]);
                    if let Some(s) = &sub {
                        c.text(desc, 11.0, 400, PAD_X, y + 10.0 + 18.3, 220.0, 14.6, Align::Left, s, &[]);
                    }
                }
            }
            match ctl {
                Ctl::Switch(key) => {
                    let on = current(ctx, key).as_bool().unwrap_or(false);
                    let (sx, sy) = (PAD_X + CW - 38.0, cy - 11.0);
                    if on {
                        if let Some(b) = c.linear(sx, 0.0, sx + 38.0, 0.0, &[(0.0, t.ok.0), (1.0, t.ok.1)]) {
                            c.fill_rect_brush(sx, sy, 38.0, 22.0, 11.0, &b);
                        }
                    } else {
                        c.fill_rect(sx, sy, 38.0, 22.0, 11.0, t.track);
                    }
                    let kx = sx + 3.0 + if on { 16.0 } else { 0.0 } + 8.0;
                    c.fill_circle(kx, sy + 11.0, 8.0, if on { Rgba(1.0, 1.0, 1.0, 1.0) } else { t.fg });
                    let new = if *key == "alerts" { json!(if on { json!([]) } else { json!([75, 90]) }) } else { json!(!on) };
                    let set_key = if *key == "alerts" { "alert_thresholds" } else { key };
                    self.add_hit((sx - 4.0, sy - 4.0, 46.0, 30.0), Hit::Set(set_key, new));
                }
                Ctl::Seg(key, opts) => {
                    let cur = current(ctx, key);
                    let widths: Vec<f32> = opts.iter().map(|(_, l)| c.g.text_width(l, 12.0, W600).ceil() + 20.0).collect();
                    let total: f32 = widths.iter().sum::<f32>() + 4.0;
                    let (mut bx, sy) = (PAD_X + CW - total, cy - 14.0);
                    c.fill_rect(bx, sy, total, 28.0, 9.0, t.chip);
                    bx += 2.0;
                    for ((val, lab), w) in opts.iter().zip(widths) {
                        let selected = match &cur {
                            Value::String(s) => s == val,
                            Value::Number(n) => n.to_string() == *val,
                            _ => false,
                        };
                        if selected {
                            c.fill_rect(bx, sy + 2.0, w, 24.0, 7.0, t.track);
                        }
                        if let Some(b) = c.solid_brush(if selected { t.fg } else { t.sub }) {
                            c.text(lab, 12.0, if selected { W600 } else { 400 }, bx, sy + 2.0, w, 24.0, Align::Center, &b, &[]);
                        }
                        let v = if *key == "limits_refresh_seconds" { json!(val.parse::<u64>().unwrap_or(120)) } else { json!(val) };
                        self.add_hit((bx, sy, w, 28.0), Hit::Set(key, v));
                        bx += w;
                    }
                }
            }
            y += h;
            c.fill_rect(PAD_X, y - 1.0, CW, 1.0, 0.0, t.line);
        }
        y += 14.0;
        let bw = (CW - 16.0) / 3.0;
        for (i, (label, hit, danger)) in [("Done", Hit::Done, false), ("Data folder", Hit::Folder, false), ("Quit", Hit::Quit, true)].into_iter().enumerate() {
            let x = PAD_X + i as f32 * (bw + 8.0);
            let hover = self.hover.borrow().as_ref() == Some(&hit);
            c.fill_rect(x, y, bw, 33.0, 10.0, if hover { t.track } else { t.chip });
            if let Some(b) = c.solid_brush(if danger { t.crit.0 } else { t.fg }) {
                c.text(label, 12.5, W600, x, y, bw, 33.0, Align::Center, &b, &[]);
            }
            self.add_hit((x, y, bw, 33.0), hit);
        }
    }
}

