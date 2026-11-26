//! The view model: pure functions from fetched data to what the pill, card and tray show.
//! A direct port of the Python helpers, including every honesty rule (see docs §12).

use crate::data::fetch::LimitsSnapshot;
use crate::data::history::History;
use crate::data::{FIVE_HOURS, Window, from_ts, ts};
use chrono::{DateTime, Local, Utc};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Ok,
    Warn,
    Crit,
}

pub fn level(pct: f64) -> Level {
    if pct >= 90.0 {
        Level::Crit
    } else if pct >= 75.0 {
        Level::Warn
    } else {
        Level::Ok
    }
}

// ------------------------------------------------------------------ formatting
pub fn fmt_tokens(n: f64) -> String {
    if n >= 1e9 {
        format!("{:.1}B", n / 1e9)
    } else if n >= 1e6 {
        format!("{:.1}M", n / 1e6)
    } else if n >= 1e4 {
        format!("{:.0}k", n / 1e3)
    } else if n >= 1e3 {
        format!("{:.1}k", n / 1e3)
    } else {
        format!("{}", n as i64)
    }
}

/// Countdown text: "1h 48m" (card) or "1h48m" (pill). Never less than 1m.
pub fn fmt_dur(secs: f64, compact: bool) -> String {
    let secs = secs.max(0.0) as i64;
    let (d, rem) = (secs / 86400, secs % 86400);
    let (h, rem) = (rem / 3600, rem % 3600);
    let m = rem / 60;
    let sep = if compact { "" } else { " " };
    if d > 0 {
        format!("{d}d{sep}{h}h")
    } else if h > 0 {
        format!("{h}h{sep}{m}m")
    } else {
        format!("{}m", m.max(1))
    }
}

/// Whole-number % for display: round half up, but never show 100% below the limit.
pub fn display_pct(p: f64) -> i64 {
    let i = (p + 0.5).floor() as i64;
    if p < 100.0 { i.min(99) } else { i }
}

/// Deliberately coarse time-to-limit, so the estimate doesn't jitter between polls.
pub fn fmt_eta(secs: f64) -> String {
    let m = secs.max(0.0) / 60.0;
    if m < 10.0 {
        return "under 10 min".into();
    }
    let r5 = (5.0 * (m / 5.0).round_ties_even()) as i64;
    if r5 < 60 {
        return format!("~{r5} min");
    }
    let step = if m < 180.0 { 15.0 } else { 30.0 };
    let mm = (step * (m / step).round_ties_even()) as i64;
    let (h, mm) = (mm / 60, mm % 60);
    if mm == 0 { format!("~{h}h") } else { format!("~{h}h {mm}m") }
}

pub fn fmt_age(secs: f64) -> String {
    let secs = secs.max(0.0) as i64;
    if secs < 60 {
        return "just now".into();
    }
    if secs < 3600 {
        return format!("{}m ago", secs / 60);
    }
    let (h, m) = (secs / 3600, (secs / 60) % 60);
    if h < 24 { format!("{h}h {m}m ago") } else { format!("{}d ago", h / 24) }
}

pub fn fmt_clock(t: &DateTime<Utc>) -> String {
    let s = t.with_timezone(&Local).format("%I:%M %p").to_string();
    s.trim_start_matches('0').to_string()
}

pub fn fmt_day_time(t: &DateTime<Utc>) -> String {
    let local = t.with_timezone(&Local);
    let today = Local::now().date_naive();
    if local.date_naive() == today {
        fmt_clock(t)
    } else if local.date_naive() == today.succ_opt().unwrap_or(today) {
        format!("tomorrow {}", fmt_clock(t))
    } else {
        format!("{} {}", local.format("%a"), fmt_clock(t))
    }
}

pub fn fmt_short_day(t: &DateTime<Utc>) -> String {
    let local = t.with_timezone(&Local);
    if local.date_naive() == Local::now().date_naive() {
        return fmt_clock(t);
    }
    let hour = local.format("%I%p").to_string();
    format!("{} {}", local.format("%a"), hour.trim_start_matches('0'))
}

// ------------------------------------------------------------------ freshness / verdict
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FreshState {
    Fresh,
    Stale,
    None,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Fresh {
    pub state: FreshState,
    pub label: String,
    pub reason: String,
}

/// One rule, shared by pill, card, tray and taskbar. fresh = last fetch succeeded
/// recently; stale = showing last-known values; none = nothing to show.
pub fn freshness(status: &str, error: &str, updated: Option<f64>, has_rows: bool, interval: f64, now: f64) -> Fresh {
    if !has_rows {
        return Fresh { state: FreshState::None, label: String::new(), reason: String::new() };
    }
    let age = updated.map(|u| now - u);
    let age_txt = match age {
        Some(a) => format!("Updated {}", fmt_age(a)),
        None => "Not updated yet".into(),
    };
    if status == "ok" && matches!(age, Some(a) if a <= 2.0 * interval + 60.0) {
        return Fresh { state: FreshState::Fresh, label: age_txt, reason: String::new() };
    }
    let mut reason = match status {
        "expired" => "Sign-in expired".to_string(),
        "signin" => "Not signed in".to_string(),
        _ if !error.is_empty() => error.to_string(),
        _ => "Not updated recently".to_string(),
    };
    if reason == "Starting" {
        reason = "Connecting".into();
    }
    let label = if age.is_some() { format!("Last {}", age_txt.to_lowercase()) } else { age_txt };
    Fresh { state: FreshState::Stale, label, reason }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Reached,
    Unknown,
    Steady,
    Hit,
    AroundReset,
    Under,
    Stale,
}

/// eta_now: seconds from now to 100% (None = not enough data, inf = flat/declining).
/// left: seconds until reset. A ±10% band around the reset stops flip-flopping.
pub fn forecast_verdict(pct: f64, eta_now: Option<f64>, left: Option<f64>) -> Verdict {
    if pct >= 100.0 {
        return Verdict::Reached;
    }
    let Some(eta) = eta_now else { return Verdict::Unknown };
    if eta.is_infinite() {
        return Verdict::Steady;
    }
    let Some(left) = left.filter(|l| *l > 0.0) else { return Verdict::Unknown };
    let ratio = eta / left;
    if ratio < 0.9 {
        Verdict::Hit
    } else if ratio <= 1.1 {
        Verdict::AroundReset
    } else {
        Verdict::Under
    }
}

// ------------------------------------------------------------------ model
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub key: String,
    pub title: String,
    pub sub: String,
    pub short: String,
    pub pct: f64,
    pub pct_i: Option<i64>,
    pub level: Level,
    pub pending: bool,
    pub reset_short: String,
    pub reset_card: String,
    pub prev_peak: Option<i64>,
}

impl Row {
    /// "57%" or "—" while a reset is pending.
    pub fn pct_text(&self) -> String {
        self.pct_i.map(|p| format!("{p}%")).unwrap_or_else(|| "—".into())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FcLevel {
    Ok,
    Warn,
    Crit,
    Muted,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Forecast {
    pub text: String,
    pub level: FcLevel,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Timeline {
    pub points: Vec<(f64, f64)>,
    pub last_x: f64,
    pub now_x: f64,
    pub now_pct: f64,
    pub proj: Option<(f64, f64)>,
    pub hit: bool,
    pub start_label: String,
    pub end_label: String,
    pub level: Level,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Model {
    pub status: String,
    pub error: String,
    pub plan: Option<String>,
    pub rows: Vec<Row>,
    pub fresh: Fresh,
    pub forecast: Option<Forecast>,
    pub timeline: Option<Timeline>,
    pub verdict: Option<Verdict>,
    /// Seconds from now to 100% (finite estimates only).
    pub eta: Option<f64>,
    pub interval_label: String,
}

fn clamp01(x: f64) -> f64 {
    (x.clamp(0.0, 1.0) * 10000.0).round() / 10000.0
}

/// Chart for the current 5-hour window. Actual data ends at the last fetch; the projection
/// starts there and uses the forecast's estimate, so where it meets 100% matches the text.
pub fn build_timeline(history: &History, five: &Window, eta: Option<f64>, updated: f64, now: f64) -> Option<Timeline> {
    let resets = five.resets.as_ref()?;
    let end = ts(resets);
    let start = end - FIVE_HOURS;
    let span = FIVE_HOURS;
    let mut pts = vec![(0.0, 0.0)];
    for (t, p) in history.series("five_hour", Some(resets)) {
        if start <= t && t < updated {
            pts.push((clamp01((t - start) / span), p));
        }
    }
    let last_x = clamp01((updated - start) / span);
    pts.push((last_x, five.pct));
    pts.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let now_x = clamp01((now - start) / span);
    let (mut proj, mut hit) = (None, false);
    if let Some(eta) = eta
        && five.pct < 100.0 {
            if eta.is_infinite() {
                proj = Some((1.0, five.pct));
            } else {
                let hit_x = last_x + eta / span;
                if hit_x <= 1.0 {
                    proj = Some(((hit_x * 10000.0).round() / 10000.0, 100.0));
                    hit = true;
                } else {
                    let slope = (100.0 - five.pct) / (hit_x - last_x);
                    proj = Some((1.0, ((five.pct + slope * (1.0 - last_x)) * 10.0).round() / 10.0));
                }
            }
        }
    Some(Timeline {
        points: pts, last_x, now_x, now_pct: five.pct, proj, hit,
        start_label: fmt_clock(&from_ts(start)), end_label: fmt_clock(resets), level: level(five.pct),
    })
}

pub fn build_model(lim: &LimitsSnapshot, history: &History, interval: f64, now: f64) -> Model {
    let updated = lim.updated;
    let mut order = vec!["five_hour".to_string(), "seven_day".to_string()];
    order.extend(lim.windows.keys().filter(|k| *k != "five_hour" && *k != "seven_day").cloned());
    let mut rows = Vec::new();
    for key in order {
        let Some(w) = lim.windows.get(&key) else { continue };
        let left = w.resets.map(|r| ts(&r) - now);
        // Data fetched before this window's reset no longer describes the current window.
        let pending = matches!((w.resets, left), (Some(r), Some(l)) if l <= 0.0 && updated.is_none_or(|u| u < ts(&r)));
        let (reset_short, reset_card) = if pending {
            ("updating".to_string(), "Reset · updating".to_string())
        } else if let Some(r) = &w.resets {
            if key == "five_hour" {
                (fmt_dur(left.unwrap_or(0.0), true), format!("Resets in {}", fmt_dur(left.unwrap_or(0.0), false)))
            } else {
                (fmt_short_day(r), format!("Resets {}", fmt_day_time(r)))
            }
        } else {
            (String::new(), String::new())
        };
        let prev_peak = if key == "seven_day" && w.resets.is_some() && !pending {
            history.prev_window_peak(&key, w.resets.as_ref()).map(display_pct)
        } else {
            None
        };
        rows.push(Row {
            key: key.clone(), title: w.title.clone(), sub: w.sub.clone(), short: w.short.clone(),
            pct: if pending { 0.0 } else { (w.pct * 10.0).round() / 10.0 },
            pct_i: if pending { None } else { Some(display_pct(w.pct)) },
            level: if pending { Level::Ok } else { level(w.pct) },
            pending, reset_short, reset_card, prev_peak,
        });
    }

    let fresh = freshness(&lim.status, &lim.error, updated, !rows.is_empty(), interval, now);
    let (mut forecast, mut timeline, mut eta_now, mut verdict) = (None, None, None, None);
    let five_row = rows.iter().find(|r| r.key == "five_hour");
    if let (Some(five), Some(row)) = (lim.windows.get("five_hour"), five_row)
        && !row.pending {
            let eta = history.eta_seconds_at("five_hour", five, now);
            let age = updated.map(|u| now - u).unwrap_or(0.0);
            // The estimate is measured from the last sample.
            eta_now = eta.map(|e| if e.is_finite() { e - age } else { e });
            let left = five.resets.map(|r| ts(&r) - now);
            let v = if fresh.state == FreshState::Stale {
                forecast = Some(Forecast { text: "Forecast paused until data refreshes".into(), level: FcLevel::Muted });
                Verdict::Stale
            } else {
                let v = forecast_verdict(five.pct, eta_now, left);
                forecast = Some(match v {
                    Verdict::Reached => Forecast { text: format!("Limit reached · resets in {}", fmt_dur(left.unwrap_or(0.0), false)), level: FcLevel::Crit },
                    Verdict::Hit => {
                        let e = eta_now.unwrap_or(0.0); // only here is eta_now a finite number
                        Forecast { text: format!("At this pace, limit in {}", fmt_eta(e)), level: if e < 1800.0 { FcLevel::Crit } else { FcLevel::Warn } }
                    }
                    Verdict::Unknown => Forecast { text: "Not enough recent activity to estimate".into(), level: FcLevel::Muted },
                    Verdict::Steady => Forecast { text: "Steady · no current pace toward the limit".into(), level: FcLevel::Muted },
                    Verdict::AroundReset => Forecast { text: "At this pace, you'll reach the limit around reset time".into(), level: FcLevel::Warn },
                    _ => Forecast { text: "At this pace, you'll stay under the limit until reset".into(), level: FcLevel::Ok },
                });
                v
            };
            verdict = Some(v);
            if five.resets.is_some() {
                let e = if v == Verdict::Stale { None } else { eta };
                timeline = build_timeline(history, five, e, updated.unwrap_or(now), now);
            }
        }
    let iv = interval as i64;
    Model {
        status: lim.status.clone(), error: lim.error.clone(), plan: lim.plan.clone(), rows, fresh, forecast, timeline, verdict,
        eta: eta_now.filter(|e| e.is_finite()).map(|e| e.round()),
        interval_label: if iv < 120 { format!("{iv}s") } else { format!("{} min", iv / 60) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::history::Sample;
    use std::collections::BTreeMap;

    fn win(pct: f64, resets_in: f64, now: f64) -> Window {
        Window { title: "Session".into(), sub: "".into(), short: "5h".into(), pct, resets: Some(from_ts(now + resets_in)) }
    }
    fn snap(windows: Vec<(&str, Window)>, status: &str, updated: Option<f64>) -> LimitsSnapshot {
        LimitsSnapshot {
            windows: windows.into_iter().map(|(k, w)| (k.to_string(), w)).collect(),
            plan: Some("Pro".into()), status: status.into(), error: String::new(), updated,
        }
    }
    fn rising(now: f64, resets: f64, from: f64, per_min: f64) -> History {
        let r = crate::data::iso(&from_ts(resets));
        History::new((0..10).map(|i| Sample { t: now - 600.0 + i as f64 * 60.0,
            w: BTreeMap::from([("five_hour".into(), (from + per_min * i as f64, Some(r.clone())))]) }).collect(), false)
    }

    #[test]
    fn display_pct_rounds_half_up_and_never_fakes_100() {
        assert_eq!(display_pct(56.5), 57);
        assert_eq!(display_pct(56.4), 56);
        assert_eq!(display_pct(99.6), 99);
        assert_eq!(display_pct(100.0), 100);
        assert_eq!(display_pct(0.0), 0);
    }

    #[test]
    fn eta_is_coarse() {
        assert_eq!(fmt_eta(5.0 * 60.0), "under 10 min");
        assert_eq!(fmt_eta(23.0 * 60.0), "~25 min");
        assert_eq!(fmt_eta(58.0 * 60.0), "~1h");
        assert_eq!(fmt_eta(80.0 * 60.0), "~1h 15m");
        assert_eq!(fmt_eta(200.0 * 60.0), "~3h 30m");
    }

    #[test]
    fn durations() {
        assert_eq!(fmt_dur(4.0 * 3600.0 + 23.0 * 60.0, false), "4h 23m");
        assert_eq!(fmt_dur(4.0 * 3600.0 + 23.0 * 60.0, true), "4h23m");
        assert_eq!(fmt_dur(20.0, false), "1m");
        assert_eq!(fmt_dur(2.0 * 86400.0 + 3600.0, false), "2d 1h");
        assert_eq!(fmt_tokens(1500.0), "1.5k");
        assert_eq!(fmt_tokens(6_600_000.0), "6.6M");
        assert_eq!(fmt_tokens(45_000.0), "45k");
    }

    #[test]
    fn freshness_states() {
        let n = 1_800_000_000.0;
        assert_eq!(freshness("ok", "", Some(n - 30.0), true, 120.0, n).state, FreshState::Fresh);
        assert_eq!(freshness("ok", "", Some(n - 400.0), true, 120.0, n).state, FreshState::Stale);
        let e = freshness("expired", "Sign-in expired", Some(n - 30.0), true, 120.0, n);
        assert_eq!((e.state, e.reason.as_str()), (FreshState::Stale, "Sign-in expired"));
        let s = freshness("error", "Starting", Some(n - 30.0), true, 120.0, n);
        assert_eq!(s.reason, "Connecting");
        assert_eq!(s.label, "Last updated just now");
        assert_eq!(freshness("ok", "", None, false, 120.0, n).state, FreshState::None);
    }

    #[test]
    fn verdicts() {
        assert_eq!(forecast_verdict(100.0, Some(5.0), Some(100.0)), Verdict::Reached);
        assert_eq!(forecast_verdict(50.0, None, Some(100.0)), Verdict::Unknown);
        assert_eq!(forecast_verdict(50.0, Some(f64::INFINITY), Some(100.0)), Verdict::Steady);
        assert_eq!(forecast_verdict(50.0, Some(50.0), Some(100.0)), Verdict::Hit);
        assert_eq!(forecast_verdict(50.0, Some(100.0), Some(100.0)), Verdict::AroundReset);
        assert_eq!(forecast_verdict(50.0, Some(200.0), Some(100.0)), Verdict::Under);
    }

    #[test]
    fn forecast_text_and_chart_agree() {
        let n = crate::data::now();
        let resets = n + 3.0 * 3600.0;
        let h = rising(n, resets, 40.0, 1.0); // +1%/min: 100% in 51 min after the last sample
        let lim = snap(vec![("five_hour", win(49.0, 3.0 * 3600.0, n))], "ok", Some(n));
        let m = build_model(&lim, &h, 120.0, n);
        assert_eq!(m.verdict, Some(Verdict::Hit));
        let tl = m.timeline.unwrap();
        assert!(tl.hit);
        let start = resets - FIVE_HOURS;
        let chart_eta = start + tl.proj.unwrap().0 * FIVE_HOURS - n;
        assert!((chart_eta - m.eta.unwrap()).abs() < 5.0, "{chart_eta} vs {:?}", m.eta);
        assert_eq!(m.forecast.unwrap().text, format!("At this pace, limit in {}", fmt_eta(m.eta.unwrap())));
    }

    #[test]
    fn window_that_reset_since_last_fetch_is_pending() {
        let n = 1_800_000_000.0;
        let lim = snap(vec![("five_hour", win(80.0, -60.0, n))], "ok", Some(n - 3600.0));
        let m = build_model(&lim, &History::new(vec![], false), 120.0, n);
        let r = &m.rows[0];
        assert!(r.pending);
        assert_eq!(r.pct_i, None);
        assert_eq!(r.pct_text(), "—");
        assert_eq!(r.reset_short, "updating");
        assert!(m.forecast.is_none() && m.timeline.is_none());
    }

    #[test]
    fn stale_pauses_forecast_and_projection() {
        let n = crate::data::now();
        let h = rising(n - 900.0, n + 3.0 * 3600.0, 40.0, 1.0);
        let lim = snap(vec![("five_hour", win(49.0, 3.0 * 3600.0, n))], "error", Some(n - 900.0));
        let m = build_model(&lim, &h, 120.0, n);
        assert_eq!(m.verdict, Some(Verdict::Stale));
        assert_eq!(m.forecast.unwrap().text, "Forecast paused until data refreshes");
        assert!(m.timeline.unwrap().proj.is_none());
    }

    #[test]
    fn flat_usage_has_no_exhaustion_time() {
        let n = crate::data::now();
        let h = rising(n, n + 3.0 * 3600.0, 40.0, 0.0);
        let lim = snap(vec![("five_hour", win(40.0, 3.0 * 3600.0, n))], "ok", Some(n));
        let m = build_model(&lim, &h, 120.0, n);
        assert_eq!(m.verdict, Some(Verdict::Steady));
        assert_eq!(m.eta, None);
        assert_eq!(m.timeline.unwrap().proj, Some((1.0, 40.0)));
    }

    #[test]
    fn values_reach_model_unchanged_and_ordered() {
        let n = 1_800_000_000.0;
        let mut wk = win(22.04, 3.0 * 86400.0, n);
        wk.title = "Weekly".into();
        let lim = snap(vec![("seven_day", wk), ("five_hour", win(18.0, 3600.0, n))], "ok", Some(n));
        let m = build_model(&lim, &History::new(vec![], false), 120.0, n);
        assert_eq!(m.rows[0].key, "five_hour");
        assert_eq!(m.rows[1].pct, 22.0);
        assert_eq!(m.rows[1].pct_i, Some(22));
        assert_eq!(m.rows[0].reset_card, "Resets in 1h 0m");
    }

    #[test]
    fn actual_line_ends_at_last_fetch() {
        let n = crate::data::now();
        let h = rising(n - 600.0, n + 3.0 * 3600.0, 40.0, 1.0);
        let five = win(49.0, 3.0 * 3600.0, n);
        let tl = build_timeline(&h, &five, None, n - 600.0, n).unwrap();
        assert!(tl.last_x < tl.now_x);
        assert_eq!(tl.points.last().unwrap().0, tl.last_x);
        assert!(tl.proj.is_none());
    }
}
