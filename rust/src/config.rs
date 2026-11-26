//! Paths, settings (config.json), runtime state (state.json) and the log file.
//! Same files and formats as the Python app, so the two can be swapped freely.

use serde_json::{Map, Value, json};
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

pub const APP: &str = "ClaudeUsageWidget";

pub fn data_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(home_dir);
    let d = base.join(APP);
    let _ = fs::create_dir_all(&d);
    d
}

pub fn home_dir() -> PathBuf {
    std::env::var_os("USERPROFILE").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
}

pub fn claude_dir() -> PathBuf {
    home_dir().join(".claude")
}
pub fn creds_path() -> PathBuf {
    claude_dir().join(".credentials.json")
}
pub fn projects_dir() -> PathBuf {
    claude_dir().join("projects")
}
pub fn config_path() -> PathBuf {
    data_dir().join("config.json")
}
pub fn state_path() -> PathBuf {
    data_dir().join("state.json")
}
pub fn history_path() -> PathBuf {
    data_dir().join("history.jsonl")
}
pub fn log_path() -> PathBuf {
    data_dir().join("widget.log")
}

pub fn default_config() -> Map<String, Value> {
    let v = json!({
        "limits_refresh_seconds": 120,
        "logs_refresh_seconds": 5,
        "alert_thresholds": [75, 90],
        "alert_on_reset": true,
        "alert_on_forecast": true,
        "dim_when_idle": true,
        "hide_in_fullscreen": true,
        "taskbar_button": true,
        "show_model_limits": false,
        "pill_style": "bars",
        "theme": "auto",
        "margin_px": 8
    });
    match v {
        Value::Object(m) => m,
        _ => unreachable!(),
    }
}

pub fn load_json(path: &PathBuf) -> Option<Value> {
    let s = fs::read_to_string(path).ok()?;
    serde_json::from_str(s.trim_start_matches('\u{feff}')).ok()
}

pub fn save_json(path: &PathBuf, v: &Value) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_string_pretty(v).unwrap_or_default())?;
    fs::rename(&tmp, path)
}

fn same_kind(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.is_f64() == y.is_f64(),
        _ => std::mem::discriminant(a) == std::mem::discriminant(b),
    }
}

/// Thread-safe settings + runtime state.
pub struct Store {
    cfg: Mutex<Map<String, Value>>,
    pub state: Mutex<Map<String, Value>>,
    /// false for --demo and tests: settings and state changes stay in memory, so a demo
    /// run never rewrites the user's config.json / state.json.
    persist: bool,
}

impl Store {
    pub fn load(persist: bool) -> Store {
        let defaults = default_config();
        let mut cfg = defaults.clone();
        let user = load_json(&config_path());
        if let Some(Value::Object(u)) = &user {
            for (k, v) in u {
                if let Some(d) = defaults.get(k) {
                    if same_kind(d, v) {
                        cfg.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        if persist && user.as_ref() != Some(&Value::Object(cfg.clone())) {
            let _ = save_json(&config_path(), &Value::Object(cfg.clone()));
        }
        let state = match load_json(&state_path()) {
            Some(Value::Object(m)) => m,
            _ => Map::new(),
        };
        Store { cfg: Mutex::new(cfg), state: Mutex::new(state), persist }
    }

    /// In-memory store for tests.
    #[cfg(test)]
    pub fn for_tests() -> Store {
        Store { cfg: Mutex::new(default_config()), state: Mutex::new(Map::new()), persist: false }
    }

    #[cfg(test)]
    pub fn update_cfg_in_memory(&self, k: &str, v: Value) {
        self.cfg.lock().unwrap().insert(k.into(), v);
    }

    pub fn cfg(&self) -> Map<String, Value> {
        self.cfg.lock().unwrap().clone()
    }
    pub fn cfg_u64(&self, k: &str) -> u64 {
        self.cfg.lock().unwrap().get(k).and_then(Value::as_u64).unwrap_or(0)
    }
    pub fn cfg_bool(&self, k: &str) -> bool {
        self.cfg.lock().unwrap().get(k).and_then(Value::as_bool).unwrap_or(false)
    }
    pub fn cfg_str(&self, k: &str) -> String {
        self.cfg.lock().unwrap().get(k).and_then(Value::as_str).unwrap_or("").to_string()
    }

    pub fn update_cfg(&self, changes: &Map<String, Value>) {
        let defaults = default_config();
        let snapshot = {
            let mut cfg = self.cfg.lock().unwrap();
            for (k, v) in changes {
                if let Some(d) = defaults.get(k) {
                    if same_kind(d, v) {
                        cfg.insert(k.clone(), v.clone());
                    }
                }
            }
            cfg.clone()
        };
        if self.persist {
            let _ = save_json(&config_path(), &Value::Object(snapshot));
        }
    }

    pub fn save_state(&self) {
        if !self.persist {
            return;
        }
        let snapshot = self.state.lock().unwrap().clone();
        if let Err(e) = save_json(&state_path(), &Value::Object(snapshot)) {
            log(&format!("WARNING state save: {e}"));
        }
    }
}

// ------------------------------------------------------------------ log
static LOG_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

/// Append a line to widget.log (rotates at 512 KB, like the Python RotatingFileHandler).
/// Never pass secrets here.
pub fn log(msg: &str) {
    if cfg!(test) {
        return; // keep test noise out of the real log
    }
    let _g = LOG_LOCK.get_or_init(|| Mutex::new(())).lock();
    let p = log_path();
    if fs::metadata(&p).map(|m| m.len() > 512_000).unwrap_or(false) {
        let _ = fs::rename(&p, p.with_extension("log.1"));
    }
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(&p) {
        let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
        let level = if msg.starts_with("WARNING") || msg.starts_with("ERROR") { "" } else { "INFO " };
        let one_line = msg.replace("\r\n", " | ").replace('\n', " | ");
        let _ = writeln!(f, "{ts} {level}{one_line} [rust]");
    }
}

#[macro_export]
macro_rules! logf {
    ($($t:tt)*) => { $crate::config::log(&format!($($t)*)) };
}
