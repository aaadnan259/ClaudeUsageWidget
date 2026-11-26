//! Limit alerts: thresholds (75/90, once per window, saved so restarts don't repeat them),
//! a timely reset notice, and the "limit ahead" pace alert. Same rules and the same
//! state.json shapes as the Python check_alerts.

use crate::config::Store;
use crate::data::fetch::LimitsSnapshot;
use crate::data::{iso, parse_iso, reset_key};
use crate::model::{Model, Verdict, fmt_day_time, fmt_eta};
use chrono::{DateTime, Duration, Utc};
use serde_json::{Value, json};

/// Returns the notifications to show, as (title, message). Updates `store.state`.
pub fn check(store: &Store, lim: &LimitsSnapshot, model: &Model, now: DateTime<Utc>) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if lim.status != "ok" {
        return out; // never alert from cached or stale values
    }
    let cfg = store.cfg();
    let thresholds: Vec<i64> = cfg.get("alert_thresholds").and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_i64).collect()).unwrap_or_default();
    let on_reset = cfg.get("alert_on_reset").and_then(Value::as_bool).unwrap_or(true);
    let on_forecast = cfg.get("alert_on_forecast").and_then(Value::as_bool).unwrap_or(true);
    let mut changed = false;
    {
        let mut state = store.state.lock().unwrap();
        if !state.get("alerts").is_some_and(Value::is_object) {
            state.insert("alerts".into(), json!({}));
        }
        if !state.get("last_windows").is_some_and(Value::is_object) {
            state.insert("last_windows".into(), json!({}));
        }
        for (key, name) in [("five_hour", "5-hour limit"), ("seven_day", "Weekly limit")] {
            let Some(w) = lim.windows.get(key) else { continue };
            let rkey = format!("{key}|{}", reset_key(w.resets.as_ref()));
            let last = state.get_mut("last_windows").and_then(Value::as_object_mut).unwrap();
            let prev = last.get(key).cloned();
            let prev_rkey = prev.as_ref().and_then(|p| p.get("rkey")).and_then(Value::as_str).map(str::to_string);
            let prev_pct = prev.as_ref().and_then(|p| p.get("pct")).and_then(Value::as_f64).unwrap_or(0.0);
            if prev.is_some() && prev_rkey.as_deref() != Some(rkey.as_str()) && on_reset {
                let pr = parse_iso(prev.as_ref().and_then(|p| p.get("resets")).and_then(Value::as_str));
                // only a timely alert: skip resets that happened long ago (asleep / not running)
                if pr.is_some_and(|pr| now - Duration::minutes(30) <= pr && pr <= now) && prev_pct >= 1.0 {
                    out.push((format!("Claude {name} reset"), format!("You're back to {:.0}%.", w.pct)));
                }
            }
            if prev.is_none() || prev_rkey.as_deref() != Some(rkey.as_str()) || (prev_pct - w.pct).abs() >= 0.5 {
                last.insert(key.into(), json!({"rkey": rkey, "pct": w.pct, "resets": w.resets.as_ref().map(iso)}));
                changed = true;
            }
            let alerts = state.get_mut("alerts").and_then(Value::as_object_mut).unwrap();
            let fired = alerts.entry(rkey.clone()).or_insert_with(|| json!([]));
            let fired = fired.as_array_mut().unwrap();
            let mut ths = thresholds.clone();
            ths.sort();
            for th in ths {
                if w.pct >= th as f64 && !fired.iter().any(|f| f.as_i64() == Some(th)) {
                    fired.push(json!(th));
                    changed = true;
                    let when = w.resets.as_ref().map(|r| format!(" Resets {}.", fmt_day_time(r))).unwrap_or_default();
                    out.push((format!("Claude {name}: {:.0}% used", w.pct),
                              format!("You've crossed {th}% of your {}.{when}", name.to_lowercase())));
                }
            }
            if key == "five_hour" && on_forecast {
                // needs a real estimate on fresh data; saved so restarts don't repeat it
                if model.verdict == Some(Verdict::Hit) && model.eta.is_some_and(|e| e < 1800.0) && w.pct < 90.0
                    && !fired.iter().any(|f| f.as_str() == Some("forecast")) {
                    fired.push(json!("forecast"));
                    changed = true;
                    out.push(("Claude: limit ahead".into(),
                              format!("At this pace you'll hit the 5-hour limit in {}.", fmt_eta(model.eta.unwrap_or(0.0)))));
                }
            }
        }
        let alerts = state.get_mut("alerts").and_then(Value::as_object_mut).unwrap();
        while alerts.len() > 40 {
            let first = alerts.keys().next().cloned().unwrap();
            alerts.shift_remove(&first);
        }
    }
    if changed {
        store.save_state();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::history::History;
    use crate::data::{Window, from_ts, now};
    use crate::model::build_model;
    use std::collections::BTreeMap;

    fn lim(pct5: f64, reset5: f64, pctw: f64, status: &str) -> LimitsSnapshot {
        let w = |pct, r: f64| Window { title: "x".into(), sub: "".into(), short: "x".into(), pct, resets: Some(from_ts(r)) };
        LimitsSnapshot {
            windows: BTreeMap::from([("five_hour".into(), w(pct5, reset5)), ("seven_day".into(), w(pctw, now() + 5.0 * 86400.0))]),
            plan: None, status: status.into(), error: String::new(), updated: Some(now()),
        }
    }
    fn model(l: &LimitsSnapshot) -> Model {
        build_model(l, &History::new(vec![], false), 120.0, now())
    }

    #[test]
    fn thresholds_fire_once_per_window_and_survive_restart() {
        let store = Store::for_tests();
        let r = now() + 3600.0;
        let l = lim(80.0, r, 10.0, "ok");
        let a = check(&store, &l, &model(&l), Utc::now());
        assert_eq!(a.len(), 1);
        assert!(a[0].0.contains("5-hour limit: 80% used"));
        assert!(check(&store, &l, &model(&l), Utc::now()).is_empty());
        let l2 = lim(91.0, r, 10.0, "ok");
        assert_eq!(check(&store, &l2, &model(&l2), Utc::now()).len(), 1);
        // a restart keeps state.json: no repeats
        let saved = store.state.lock().unwrap().clone();
        let store2 = Store::for_tests();
        *store2.state.lock().unwrap() = saved;
        assert!(check(&store2, &l2, &model(&l2), Utc::now()).is_empty());
    }

    #[test]
    fn session_and_weekly_are_independent_and_cached_values_never_alert() {
        let store = Store::for_tests();
        let l = lim(10.0, now() + 3600.0, 76.0, "error");
        assert!(check(&store, &l, &model(&l), Utc::now()).is_empty());
        let l = lim(10.0, now() + 3600.0, 76.0, "ok");
        let a = check(&store, &l, &model(&l), Utc::now());
        assert_eq!(a.len(), 1);
        assert!(a[0].0.contains("Weekly limit"));
    }

    #[test]
    fn reset_alert_only_when_timely() {
        let store = Store::for_tests();
        let old_reset = now() - 600.0;
        store.state.lock().unwrap().insert("last_windows".into(), json!({"five_hour": {
            "rkey": format!("five_hour|{}", reset_key(Some(&from_ts(old_reset)))), "pct": 60.0, "resets": iso(&from_ts(old_reset))}}));
        let l = lim(0.0, now() + 5.0 * 3600.0, 10.0, "ok");
        let a = check(&store, &l, &model(&l), Utc::now());
        assert!(a.iter().any(|(t, _)| t == "Claude 5-hour limit reset"));
        // a reset 3 hours ago (PC asleep) gives no late toast
        let store = Store::for_tests();
        let long_ago = now() - 3.0 * 3600.0;
        store.state.lock().unwrap().insert("last_windows".into(), json!({"five_hour": {
            "rkey": format!("five_hour|{}", reset_key(Some(&from_ts(long_ago)))), "pct": 60.0, "resets": iso(&from_ts(long_ago))}}));
        assert!(check(&store, &l, &model(&l), Utc::now()).is_empty());
    }

    #[test]
    fn forecast_alert_once() {
        let store = Store::for_tests();
        let l = lim(50.0, now() + 3.0 * 3600.0, 10.0, "ok");
        let mut m = model(&l);
        m.verdict = Some(Verdict::Hit);
        m.eta = Some(900.0);
        let a = check(&store, &l, &m, Utc::now());
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].0, "Claude: limit ahead");
        assert!(check(&store, &l, &m, Utc::now()).is_empty());
    }
}
