//! Plan limits from Anthropic's (undocumented) usage endpoint, via Windows' curl.exe.
//! Secrets travel on stdin or in a short-lived temp file, never on the command line or in
//! the log.

use super::history::History;
use super::{Window, from_ts, limit_meta, now, parse_iso};
use crate::config::{Store, creds_path, data_dir, load_json, save_json};
use crate::logf;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::Write;
use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

pub const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
pub const TOKEN_URLS: [&str; 2] =
    ["https://console.anthropic.com/v1/oauth/token", "https://platform.claude.com/v1/oauth/token"];
pub const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e"; // Claude Code's public OAuth client id
pub const USER_AGENT: &str = "claude-usage-widget/3.0";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Debug)]
pub enum FetchError {
    Http(u16),
    Connection(String),
    Unexpected(String),
}

fn curl_path() -> std::path::PathBuf {
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    std::path::PathBuf::from(root).join("System32").join("curl.exe")
}

pub fn http_json(url: &str, headers: &[(&str, String)], body: Option<&str>) -> Result<Value, FetchError> {
    let mut hdr: Vec<(String, String)> = headers.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
    hdr.push(("User-Agent".into(), USER_AGENT.into()));
    hdr.push(("Accept".into(), "application/json".into()));
    let mut cmd = Command::new(curl_path());
    cmd.args(["-sS", "--max-time", "20", "-H", "@-", "-w", "\n%{http_code}"]);
    let mut tmp = None;
    if let Some(b) = body {
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
        let p = data_dir().join(format!(".req-{}-r-{}.json", std::process::id(), t.as_nanos()));
        std::fs::write(&p, b).map_err(|e| FetchError::Connection(e.to_string()))?;
        cmd.args(["-X", "POST", "--data-binary"]).arg(format!("@{}", p.display()));
        hdr.push(("Content-Type".into(), "application/json".into()));
        tmp = Some(p);
    }
    cmd.arg(url).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    cmd.creation_flags(CREATE_NO_WINDOW);
    let result = (|| {
        let mut child = cmd.spawn().map_err(|e| FetchError::Connection(e.to_string()))?;
        if let Some(mut stdin) = child.stdin.take() {
            let text: Vec<String> = hdr.iter().map(|(k, v)| format!("{k}: {v}")).collect();
            let _ = stdin.write_all(text.join("\n").as_bytes());
        }
        child.wait_with_output().map_err(|e| FetchError::Connection(e.to_string()))
    })();
    if let Some(p) = tmp {
        let _ = std::fs::remove_file(p);
    }
    let out = result?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().chars().take(200).collect::<String>();
        return Err(FetchError::Connection(if err.is_empty() { format!("curl exit {:?}", out.status.code()) } else { err }));
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let (text, code) = stdout.rsplit_once('\n').unwrap_or(("", &stdout));
    let code: u16 = code.trim().parse().unwrap_or(0);
    if !(200..300).contains(&code) {
        return Err(FetchError::Http(code));
    }
    serde_json::from_str(text).map_err(|_| FetchError::Unexpected("response is not JSON".into()))
}

/// Status shown for an HTTP error from the usage endpoint.
pub fn http_status(code: u16) -> (&'static str, String) {
    match code {
        401 | 403 => ("expired", "Sign-in expired".into()),
        429 => ("error", "Rate-limited, retrying".into()),
        c => ("error", format!("HTTP {c}")),
    }
}

/// Extra wait after a 429: 120 s, then doubling, capped at 15 minutes.
pub fn next_backoff(prev: u64) -> u64 {
    (prev * 2).clamp(120, 900)
}

#[derive(Clone, Debug, PartialEq)]
pub struct LimitsSnapshot {
    pub windows: BTreeMap<String, Window>,
    pub plan: Option<String>,
    pub status: String, // loading | ok | error | signin | expired
    pub error: String,
    pub updated: Option<f64>,
}

pub struct LimitsFetcher {
    pub store: Arc<Store>,
    pub history: Arc<History>,
    pub demo: bool,
    pub state: Mutex<LimitsSnapshot>,
    wake: (Mutex<bool>, Condvar),
    heartbeat: AtomicU64,
    pub stopped: AtomicBool,
    backoff: AtomicU64,
    t0: f64,
    /// Called after each poll (posts a message to the UI thread).
    pub on_update: Mutex<Option<Box<dyn Fn() + Send>>>,
}

impl LimitsFetcher {
    pub fn new(store: Arc<Store>, history: Arc<History>, demo: bool) -> LimitsFetcher {
        let mut snap = LimitsSnapshot { windows: BTreeMap::new(), plan: None, status: "loading".into(), error: String::new(), updated: None };
        if !demo {
            let cached = history.last_windows();
            if !cached.is_empty() {
                snap.windows = cached;
                snap.status = "error".into();
                snap.error = "Starting".into();
                snap.updated = history.last_time();
            }
        }
        LimitsFetcher {
            store, history, demo, state: Mutex::new(snap), wake: (Mutex::new(false), Condvar::new()),
            heartbeat: AtomicU64::new(now().to_bits()), stopped: AtomicBool::new(false),
            backoff: AtomicU64::new(0), t0: now(), on_update: Mutex::new(None),
        }
    }

    pub fn snapshot(&self) -> LimitsSnapshot {
        self.state.lock().unwrap().clone()
    }

    pub fn refresh_now(&self) {
        let (m, c) = &self.wake;
        *m.lock().unwrap() = true;
        c.notify_all();
    }

    pub fn set_heartbeat(&self, t: f64) {
        self.heartbeat.store(t.to_bits(), Ordering::Relaxed);
    }

    fn read_oauth() -> Option<serde_json::Map<String, Value>> {
        match load_json(&creds_path())?.get("claudeAiOauth")? {
            Value::Object(m) => Some(m.clone()),
            _ => None,
        }
    }

    fn refresh_token(&self, oauth: &serde_json::Map<String, Value>) -> Result<serde_json::Map<String, Value>, FetchError> {
        let rt = oauth.get("refreshToken").and_then(Value::as_str).unwrap_or_default().to_string();
        let body = json!({"grant_type": "refresh_token", "refresh_token": rt, "client_id": CLIENT_ID}).to_string();
        let mut last = FetchError::Connection("no token endpoint".into());
        let mut tok = None;
        for url in TOKEN_URLS {
            match http_json(url, &[], Some(&body)) {
                Ok(v) => {
                    tok = Some(v);
                    break;
                }
                Err(e) => last = e,
            }
        }
        let tok = tok.ok_or(last)?;
        let mut data = match load_json(&creds_path()) {
            Some(Value::Object(m)) => m,
            _ => serde_json::Map::new(),
        };
        let mut cur = match data.get("claudeAiOauth") {
            Some(Value::Object(m)) => m.clone(),
            _ => serde_json::Map::new(),
        };
        if cur.get("refreshToken").and_then(Value::as_str) != Some(rt.as_str()) {
            return Ok(cur); // the CLI refreshed it meanwhile
        }
        let access = tok.get("access_token").and_then(Value::as_str).ok_or_else(|| FetchError::Unexpected("token response".into()))?;
        cur.insert("accessToken".into(), json!(access));
        if let Some(r) = tok.get("refresh_token").and_then(Value::as_str) {
            cur.insert("refreshToken".into(), json!(r));
        }
        let exp = tok.get("expires_in").and_then(Value::as_i64).unwrap_or(3600);
        cur.insert("expiresAt".into(), json!(((now() + exp as f64) * 1000.0) as i64));
        data.insert("claudeAiOauth".into(), Value::Object(cur.clone()));
        let _ = save_json(&creds_path(), &Value::Object(data));
        logf!("refreshed access token");
        Ok(cur)
    }

    pub fn parse(&self, resp: &Value) -> BTreeMap<String, Window> {
        let mut out = BTreeMap::new();
        let Value::Object(m) = resp else { return out };
        let show_models = self.store.cfg_bool("show_model_limits");
        for (key, val) in m {
            let Value::Object(v) = val else { continue };
            let Some(pct) = v.get("utilization").and_then(Value::as_f64) else { continue };
            let (title, sub, short) = if let Some((t, s, sh)) = limit_meta(key) {
                (t.to_string(), s.to_string(), sh.to_string())
            } else if let (Some(rest), true) = (key.strip_prefix("seven_day_"), show_models) {
                let name: String = rest
                    .split('_')
                    .map(|w| {
                        let mut c = w.chars();
                        c.next().map(|f| f.to_uppercase().chain(c.flat_map(char::to_lowercase)).collect::<String>()).unwrap_or_default()
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                let short: String = name.chars().take(2).collect();
                (name, "Dedicated weekly limit".into(), short)
            } else {
                continue;
            };
            let resets = parse_iso(v.get("resets_at").and_then(Value::as_str));
            out.insert(key.clone(), Window { title, sub, short, pct, resets });
        }
        out
    }

    fn demo_windows(&self) -> BTreeMap<String, Window> {
        let base = self.t0;
        // CUW_DEMO_STILL=1 holds the values steady (for measuring idle cost with the card open).
        let el = if std::env::var_os("CUW_DEMO_STILL").is_some() { 0.0 } else { now() - self.t0 };
        // CUW_DEMO_SCENARIO: warn (80%), crit (93%), pending (the session resets 20 s in,
        // after updates stop at 15 s, so the "reset · updating" state shows).
        let scenario = std::env::var("CUW_DEMO_SCENARIO").unwrap_or_default();
        let (pct, reset) = match scenario.as_str() {
            "warn" => (80.0, base + 2.0 * 3600.0 + 50.0 * 60.0),
            "crit" => (93.0, base + 2.0 * 3600.0 + 50.0 * 60.0),
            "pending" => (64.0, base + 20.0),
            _ => ((52.0 + el / 10.0).min(100.0), base + 2.0 * 3600.0 + 50.0 * 60.0),
        };
        BTreeMap::from([
            ("five_hour".to_string(), Window { title: "Session".into(), sub: "5h sliding window".into(), short: "5h".into(),
                pct, resets: Some(from_ts(reset)) }),
            ("seven_day".to_string(), Window { title: "Weekly".into(), sub: "All models".into(), short: "Wk".into(),
                pct: 22.0, resets: Some(from_ts(base + (4.0 * 24.0 + 2.0) * 3600.0)) }),
        ])
    }

    fn set_status(&self, status: &str, error: &str) {
        let mut s = self.state.lock().unwrap();
        s.status = status.into();
        s.error = error.into();
    }

    pub fn poll_once(&self) {
        if self.demo {
            if std::env::var_os("CUW_DEMO_OFFLINE").is_some() && now() - self.t0 > 25.0 {
                self.set_status("error", "Offline"); // demo-only: simulate losing the network
                return;
            }
            if std::env::var("CUW_DEMO_SCENARIO").as_deref() == Ok("pending") && now() - self.t0 > 15.0 {
                return; // no new data after the (demo) reset
            }
            let w = self.demo_windows();
            let t = now();
            {
                let mut s = self.state.lock().unwrap();
                s.windows = w.clone();
                s.plan = Some("Pro".into());
                s.status = "ok".into();
                s.error.clear();
                s.updated = Some(t);
            }
            self.history.add(t, &w);
            return;
        }
        let Some(mut oauth) = Self::read_oauth().filter(|o| o.get("accessToken").and_then(Value::as_str).is_some()) else {
            self.set_status("signin", "");
            return;
        };
        let plan = oauth.get("subscriptionType").and_then(Value::as_str).filter(|s| !s.is_empty()).map(|s| {
            let mut c = s.chars();
            c.next().map(|f| f.to_uppercase().chain(c.flat_map(char::to_lowercase)).collect::<String>()).unwrap_or_default()
        });
        let result: Result<BTreeMap<String, Window>, FetchError> = (|| {
            let has_refresh = oauth.get("refreshToken").and_then(Value::as_str).is_some();
            if let Some(exp) = oauth.get("expiresAt").and_then(Value::as_f64) {
                if exp / 1000.0 < now() + 60.0 && has_refresh {
                    oauth = self.refresh_token(&oauth)?;
                }
            }
            let hdr = |o: &serde_json::Map<String, Value>| {
                vec![("Authorization", format!("Bearer {}", o.get("accessToken").and_then(Value::as_str).unwrap_or_default())),
                     ("anthropic-beta", "oauth-2025-04-20".to_string())]
            };
            let resp = match http_json(USAGE_URL, &hdr(&oauth), None) {
                Err(FetchError::Http(401)) if has_refresh => {
                    oauth = self.refresh_token(&oauth)?;
                    http_json(USAGE_URL, &hdr(&oauth), None)?
                }
                r => r?,
            };
            let windows = self.parse(&resp);
            if windows.is_empty() {
                // Never show an unrecognised response as 0%: keep the last values, flag stale.
                let keys: Vec<String> = match &resp {
                    Value::Object(m) => m.keys().take(12).cloned().collect(),
                    _ => vec!["not an object".into()],
                };
                return Err(FetchError::Unexpected(format!("no known usage windows (keys: {keys:?})")));
            }
            Ok(windows)
        })();
        match result {
            Ok(windows) => {
                let t = now();
                {
                    let mut s = self.state.lock().unwrap();
                    s.windows = windows.clone();
                    s.plan = plan;
                    s.status = "ok".into();
                    s.error.clear();
                    s.updated = Some(t);
                }
                self.history.add(t, &windows);
                self.backoff.store(0, Ordering::Relaxed);
            }
            Err(FetchError::Http(code)) => {
                logf!("WARNING usage HTTP {code}");
                let (status, error) = http_status(code);
                self.set_status(status, &error);
                if code == 429 {
                    let b = self.backoff.load(Ordering::Relaxed);
                    self.backoff.store(next_backoff(b), Ordering::Relaxed);
                }
            }
            Err(FetchError::Unexpected(e)) => {
                logf!("WARNING usage response: {e}");
                self.set_status("error", "Unexpected response");
            }
            Err(FetchError::Connection(e)) => {
                logf!("WARNING usage fetch failed: {e}");
                self.set_status("error", "Offline");
            }
        }
    }

    pub fn next_wait(&self) -> f64 {
        if self.demo {
            3.0
        } else {
            self.store.cfg_u64("limits_refresh_seconds").max(30) as f64 + self.backoff.load(Ordering::Relaxed) as f64
        }
    }

    /// True if the thread has missed its schedule (a stuck call).
    pub fn overdue(&self) -> bool {
        now() - f64::from_bits(self.heartbeat.load(Ordering::Relaxed)) > self.next_wait() + 150.0
    }

    pub fn run(self: Arc<Self>) {
        loop {
            self.set_heartbeat(now());
            self.poll_once();
            self.set_heartbeat(now());
            if let Some(cb) = self.on_update.lock().unwrap().as_ref() {
                cb();
            }
            // Wait in short steps against the wall clock: Windows timed waits don't
            // advance while the PC sleeps, so one long wait can overrun by hours.
            let deadline = now() + self.next_wait();
            let (m, c) = &self.wake;
            let mut woke = m.lock().unwrap();
            while now() < deadline && !self.stopped.load(Ordering::Relaxed) && !*woke {
                woke = c.wait_timeout(woke, Duration::from_secs(5)).unwrap().0;
            }
            *woke = false;
            drop(woke);
            if self.stopped.load(Ordering::Relaxed) {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fetcher() -> LimitsFetcher {
        LimitsFetcher::new(Arc::new(Store::for_tests()), Arc::new(History::new(vec![], false)), true)
    }

    #[test]
    fn parse_known_windows_and_skips_others() {
        let f = fetcher();
        let w = f.parse(&json!({
            "five_hour": {"utilization": 18.0, "resets_at": "2026-11-26T15:00:00.5+00:00"},
            "seven_day": {"utilization": 22, "resets_at": null},
            "seven_day_opus": {"utilization": 5.0},
            "extra_usage": {"is_enabled": false},
            "something_new": 3
        }));
        assert_eq!(w.len(), 2);
        assert_eq!(w["five_hour"].pct, 18.0);
        assert_eq!(w["five_hour"].title, "Session");
        assert!(w["five_hour"].resets.is_some());
        assert_eq!(w["seven_day"].resets, None);
    }

    #[test]
    fn model_limits_only_when_enabled() {
        let f = fetcher();
        f.store.update_cfg_in_memory("show_model_limits", json!(true));
        let w = f.parse(&json!({"seven_day_opus": {"utilization": 5.0}}));
        assert_eq!(w["seven_day_opus"].title, "Opus");
        assert_eq!(w["seven_day_opus"].short, "Op");
    }

    #[test]
    fn rate_limit_backoff_grows_and_caps() {
        let mut b = 0;
        let mut seen = vec![];
        for _ in 0..5 {
            b = next_backoff(b);
            seen.push(b);
        }
        assert_eq!(seen, vec![120, 240, 480, 900, 900]);
        // a real (non-demo) fetcher: demo mode always waits 3 s
        let f = LimitsFetcher::new(Arc::new(Store::for_tests()), Arc::new(History::new(vec![], false)), false);
        let base = f.next_wait();
        assert_eq!(base, 120.0);
        f.backoff.store(240, Ordering::Relaxed);
        assert_eq!(f.next_wait(), base + 240.0);
    }

    #[test]
    fn http_errors_map_to_honest_statuses() {
        assert_eq!(http_status(401), ("expired", "Sign-in expired".to_string()));
        assert_eq!(http_status(403).0, "expired");
        assert_eq!(http_status(429), ("error", "Rate-limited, retrying".to_string()));
        assert_eq!(http_status(503), ("error", "HTTP 503".to_string()));
    }

    #[test]
    fn unparseable_response_is_empty() {
        assert!(fetcher().parse(&json!([1, 2])).is_empty());
    }
}
