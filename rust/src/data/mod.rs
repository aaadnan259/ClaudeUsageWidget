//! Data sources: plan limits (Anthropic), limit history + forecast.

pub mod fetch;
pub mod logs;
pub mod history;

use chrono::{DateTime, NaiveDateTime, SecondsFormat, Utc};

pub const FIVE_HOURS: f64 = 5.0 * 3600.0;

/// One plan-limit window as shown to the user.
#[derive(Clone, Debug, PartialEq)]
pub struct Window {
    pub title: String,
    pub sub: String,
    pub short: String,
    pub pct: f64,
    pub resets: Option<DateTime<Utc>>,
}

pub fn limit_meta(key: &str) -> Option<(&'static str, &'static str, &'static str)> {
    match key {
        "five_hour" => Some(("Session", "5h sliding window", "5h")),
        "seven_day" => Some(("Weekly", "All models", "Wk")),
        _ => None,
    }
}

/// Seconds since the Unix epoch, as a float (like Python's time.time()).
pub fn now() -> f64 {
    let d = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    d.as_secs_f64()
}

pub fn now_utc() -> DateTime<Utc> {
    Utc::now()
}

pub fn ts(t: &DateTime<Utc>) -> f64 {
    t.timestamp() as f64 + t.timestamp_subsec_nanos() as f64 / 1e9
}

pub fn from_ts(t: f64) -> DateTime<Utc> {
    let secs = t.floor();
    DateTime::from_timestamp(secs as i64, ((t - secs) * 1e9) as u32).unwrap_or_default()
}

/// ISO-8601 parse; naive times are UTC (same as the Python parse_iso).
pub fn parse_iso(s: Option<&str>) -> Option<DateTime<Utc>> {
    let s = s?.trim();
    if s.is_empty() {
        return None;
    }
    if let Ok(d) = DateTime::parse_from_rfc3339(&s.replace(' ', "T")) {
        return Some(d.with_timezone(&Utc));
    }
    for f in ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%dT%H:%M:%S"] {
        if let Ok(n) = NaiveDateTime::parse_from_str(s, f) {
            return Some(n.and_utc());
        }
    }
    None
}

pub fn iso(t: &DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::AutoSi, false)
}

/// Stable id for a limit window: reset times jitter by seconds between polls.
/// Python's round() (ties to even) so ids match the ones in state.json.
pub fn reset_key(t: Option<&DateTime<Utc>>) -> String {
    match t {
        Some(t) => format!("{}", (ts(t) / 600.0).round_ties_even() as i64),
        None => String::new(),
    }
}
