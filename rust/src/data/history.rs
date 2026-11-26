//! Persisted limit samples (history.jsonl) for the timeline, pace forecast and last
//! week's peak. Same line format as the Python app: {"t": epoch, "w": {key: [pct, iso]}}.

use super::{Window, from_ts, iso, limit_meta, now, now_utc, parse_iso, reset_key};
use crate::config::history_path;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::sync::Mutex;

pub const KEEP_DAYS: f64 = 35.0;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Sample {
    pub t: f64,
    pub w: BTreeMap<String, (f64, Option<String>)>,
}

pub struct History {
    pub demo: bool,
    pub samples: Mutex<Vec<Sample>>,
    peak_cache: Mutex<Option<((String, String, usize), Option<f64>)>>,
}

fn round_to(v: f64, places: i32) -> f64 {
    let m = 10f64.powi(places);
    (v * m).round() / m
}

impl History {
    pub fn new(samples: Vec<Sample>, demo: bool) -> History {
        History { demo, samples: Mutex::new(samples), peak_cache: Mutex::new(None) }
    }

    pub fn load(demo: bool) -> History {
        if demo {
            return History::new(demo_seed(now()), true);
        }
        let cutoff = now() - KEEP_DAYS * 86400.0;
        let mut out = Vec::new();
        if let Ok(text) = fs::read_to_string(history_path()) {
            for ln in text.lines() {
                if let Ok(s) = serde_json::from_str::<Sample>(ln) {
                    if s.t >= cutoff {
                        out.push(s);
                    }
                }
            }
        }
        let h = History::new(out, false);
        h.compact();
        h
    }

    fn compact(&self) {
        let samples = self.samples.lock().unwrap();
        let mut buf = String::new();
        for s in samples.iter() {
            if let Ok(j) = serde_json::to_string(s) {
                buf.push_str(&j);
                buf.push('\n');
            }
        }
        let _ = fs::write(history_path(), buf);
    }

    pub fn add(&self, t: f64, windows: &BTreeMap<String, Window>) {
        let s = Sample {
            t: round_to(t, 1),
            w: windows
                .iter()
                .map(|(k, w)| (k.clone(), (round_to(w.pct, 2), w.resets.as_ref().map(iso))))
                .collect(),
        };
        {
            let mut v = self.samples.lock().unwrap();
            if let Some(last) = v.last() {
                if last.w == s.w && t - last.t < 300.0 {
                    return; // unchanged; keep the file small
                }
            }
            v.push(s.clone());
        }
        if !self.demo {
            if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(history_path()) {
                if let Ok(j) = serde_json::to_string(&s) {
                    let _ = writeln!(f, "{j}");
                }
            }
        }
    }

    pub fn last_time(&self) -> Option<f64> {
        self.samples.lock().unwrap().last().map(|s| s.t)
    }

    /// Last known limits, for showing something while offline / rate-limited.
    pub fn last_windows(&self) -> BTreeMap<String, Window> {
        let last = match self.samples.lock().unwrap().last() {
            Some(s) => s.w.clone(),
            None => return BTreeMap::new(),
        };
        let mut out = BTreeMap::new();
        let n = now_utc();
        for (key, (pct, resets)) in last {
            let r = parse_iso(resets.as_deref());
            let Some((title, sub, short)) = limit_meta(&key) else { continue };
            if matches!(r, Some(r) if r < n) {
                continue;
            }
            out.insert(key, Window { title: title.into(), sub: sub.into(), short: short.into(), pct, resets: r });
        }
        out
    }

    /// (epoch, pct) samples belonging to the window that resets at `resets`.
    pub fn series(&self, key: &str, resets: Option<&DateTime<Utc>>) -> Vec<(f64, f64)> {
        let rk = reset_key(resets);
        let v = self.samples.lock().unwrap();
        v.iter()
            .filter_map(|s| {
                let (pct, r) = s.w.get(key)?;
                (reset_key(parse_iso(r.as_deref()).as_ref()) == rk).then_some((s.t, *pct))
            })
            .collect()
    }

    /// Seconds until 100% at the recent pace. None = not enough data, inf = not rising.
    pub fn eta_seconds_at(&self, key: &str, w: &Window, at: f64) -> Option<f64> {
        let min_span = if self.demo { 20.0 } else { 240.0 };
        let pts: Vec<(f64, f64)> =
            self.series(key, w.resets.as_ref()).into_iter().filter(|p| p.0 >= at - 1800.0).collect();
        if pts.len() < 3 || pts[pts.len() - 1].0 - pts[0].0 < min_span {
            return None;
        }
        let n = pts.len() as f64;
        let mt = pts.iter().map(|p| p.0).sum::<f64>() / n;
        let mp = pts.iter().map(|p| p.1).sum::<f64>() / n;
        let var: f64 = pts.iter().map(|p| (p.0 - mt).powi(2)).sum();
        let slope = if var > 0.0 { pts.iter().map(|p| (p.0 - mt) * (p.1 - mp)).sum::<f64>() / var } else { 0.0 };
        if slope <= 0.0005 / 60.0 {
            return Some(f64::INFINITY);
        }
        Some((100.0 - w.pct) / slope)
    }

    /// Highest % recorded in the window before the one that resets at `resets`.
    /// None when history has no earlier window. Cached per (window, sample count).
    pub fn prev_window_peak(&self, key: &str, resets: Option<&DateTime<Utc>>) -> Option<f64> {
        let rk = reset_key(resets);
        let rk_n: i64 = rk.parse().ok()?;
        let v = self.samples.lock().unwrap();
        let ck = (key.to_string(), rk.clone(), v.len());
        if let Some((k, val)) = self.peak_cache.lock().unwrap().as_ref() {
            if *k == ck {
                return *val;
            }
        }
        let mut best: BTreeMap<i64, f64> = BTreeMap::new();
        for s in v.iter() {
            let Some((pct, r)) = s.w.get(key) else { continue };
            let Ok(k) = reset_key(parse_iso(r.as_deref()).as_ref()).parse::<i64>() else { continue };
            if k < rk_n {
                let e = best.entry(k).or_insert(0.0);
                *e = e.max(*pct);
            }
        }
        let peak = best.iter().next_back().map(|(_, p)| *p);
        *self.peak_cache.lock().unwrap() = Some((ck, peak));
        peak
    }
}

/// Demo history: a rising session over the last ~2h10m, plus a finished previous weekly
/// window that peaked at 81.6% (the demo weekly window resets in 4d2h).
pub fn demo_seed(now: f64) -> Vec<Sample> {
    let mut out = Vec::new();
    let reset = iso(&from_ts(now + 2.0 * 3600.0 + 50.0 * 60.0));
    let mut i = 0;
    while i < 130 {
        let t = now - (130 - i) as f64 * 60.0;
        let pct = (52.0 * (i as f64 / 130.0).powf(1.3)).max(0.0);
        out.push(Sample { t, w: BTreeMap::from([("five_hour".to_string(), (pct, Some(reset.clone())))]) });
        i += 3;
    }
    let prev = iso(&from_ts(now + (4.0 * 24.0 + 2.0 - 7.0 * 24.0) * 3600.0));
    for (i, pct) in [12.0, 35.5, 58.0, 81.6, 81.6].iter().enumerate() {
        out.push(Sample {
            t: now - (6.5 - 0.75 * i as f64) * 86400.0,
            w: BTreeMap::from([("seven_day".to_string(), (*pct, Some(prev.clone())))]),
        });
    }
    out.sort_by(|a, b| a.t.total_cmp(&b.t));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn win(pct: f64, resets: f64) -> Window {
        Window { title: "S".into(), sub: "".into(), short: "5h".into(), pct, resets: Some(from_ts(resets)) }
    }
    fn sample(t: f64, key: &str, pct: f64, resets: f64) -> Sample {
        Sample { t, w: BTreeMap::from([(key.to_string(), (pct, Some(iso(&from_ts(resets)))))]) }
    }

    #[test]
    fn line_format_matches_python() {
        let s: Sample = serde_json::from_str(r#"{"t":1763000000.5,"w":{"five_hour":[12.5,"2026-11-26T15:00:00.123456+00:00"],"seven_day":[3,null]}}"#).unwrap();
        assert_eq!(s.w["five_hour"].0, 12.5);
        assert_eq!(s.w["seven_day"], (3.0, None));
        let back = serde_json::to_string(&s).unwrap();
        assert!(back.contains(r#""five_hour":[12.5,"2026-11-26T15:00:00.123456+00:00"]"#));
    }

    #[test]
    fn eta_rising_flat_and_thin() {
        let n = 1_800_000_000.0;
        let r = n + 3600.0;
        let rising = History::new((0..10).map(|i| sample(n - 600.0 + i as f64 * 60.0, "five_hour", 40.0 + i as f64, r)).collect(), false);
        let eta = rising.eta_seconds_at("five_hour", &win(49.0, r), n).unwrap();
        assert!((eta - 51.0 * 60.0).abs() < 1.0, "{eta}");
        let flat = History::new((0..10).map(|i| sample(n - 600.0 + i as f64 * 60.0, "five_hour", 40.0, r)).collect(), false);
        assert_eq!(flat.eta_seconds_at("five_hour", &win(40.0, r), n), Some(f64::INFINITY));
        let thin = History::new(vec![sample(n - 60.0, "five_hour", 40.0, r)], false);
        assert_eq!(thin.eta_seconds_at("five_hour", &win(40.0, r), n), None);
    }

    #[test]
    fn series_tolerates_reset_jitter() {
        let r = 1_800_000_000.0;
        let h = History::new(vec![sample(1.0, "five_hour", 1.0, r), sample(2.0, "five_hour", 2.0, r + 20.0),
                                  sample(3.0, "five_hour", 3.0, r - 5.0 * 3600.0)], false);
        assert_eq!(h.series("five_hour", Some(&from_ts(r))).len(), 2);
    }

    #[test]
    fn prev_peak_only_previous_window() {
        let cur = 1_800_000_000.0;
        let d = 86400.0;
        let h = History::new(vec![
            sample(cur - 20.0 * d, "seven_day", 95.0, cur - 14.0 * d),
            sample(cur - 13.0 * d, "seven_day", 40.0, cur - 7.0 * d),
            sample(cur - 12.0 * d, "seven_day", 81.6, cur - 7.0 * d + 20.0),
            sample(cur - 2.0 * d, "seven_day", 50.0, cur),
        ], false);
        assert_eq!(h.prev_window_peak("seven_day", Some(&from_ts(cur))), Some(81.6));
        let none = History::new(vec![sample(cur - d, "seven_day", 50.0, cur)], false);
        assert_eq!(none.prev_window_peak("seven_day", Some(&from_ts(cur))), None);
    }

    #[test]
    fn prev_peak_cache_refreshes() {
        let cur = 1_800_000_000.0;
        let h = History::new(vec![sample(1.0, "seven_day", 40.0, cur - 7.0 * 86400.0)], false);
        assert_eq!(h.prev_window_peak("seven_day", Some(&from_ts(cur))), Some(40.0));
        h.samples.lock().unwrap().push(sample(2.0, "seven_day", 60.0, cur - 7.0 * 86400.0));
        assert_eq!(h.prev_window_peak("seven_day", Some(&from_ts(cur))), Some(60.0));
    }
}
