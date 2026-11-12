"""Paths, settings, persisted state and logging."""
import json
import logging
import logging.handlers
import os
import threading

APP = "ClaudeUsageWidget"
DATA_DIR = os.path.join(os.environ.get("APPDATA", os.path.expanduser("~")), APP)
CLAUDE_DIR = os.path.join(os.path.expanduser("~"), ".claude")
CREDS_PATH = os.path.join(CLAUDE_DIR, ".credentials.json")
PROJECTS_DIR = os.path.join(CLAUDE_DIR, "projects")
CONFIG_PATH = os.path.join(DATA_DIR, "config.json")
STATE_PATH = os.path.join(DATA_DIR, "state.json")
HISTORY_PATH = os.path.join(DATA_DIR, "history.jsonl")
LOG_PATH = os.path.join(DATA_DIR, "widget.log")
WEB_DIR = os.path.join(os.path.dirname(os.path.abspath(__file__)), "web")

DEFAULT_CONFIG = {
    "limits_refresh_seconds": 120,
    "logs_refresh_seconds": 5,
    "alert_thresholds": [75, 90],
    "alert_on_reset": True,
    "alert_on_forecast": True,
    "dim_when_idle": True,
    "hide_in_fullscreen": True,
    "taskbar_button": True,
    "show_model_limits": False,
    "pill_style": "bars",        # bars | ring | text
    "margin_px": 8,
}

os.makedirs(DATA_DIR, exist_ok=True)

log = logging.getLogger(APP)
if not log.handlers:
    h = logging.handlers.RotatingFileHandler(LOG_PATH, maxBytes=512_000, backupCount=2,
                                             encoding="utf-8")
    h.setFormatter(logging.Formatter("%(asctime)s %(levelname)s %(message)s"))
    log.addHandler(h)
    log.setLevel(logging.INFO)


def load_json(path, default):
    try:
        with open(path, encoding="utf-8") as f:
            return json.load(f)
    except Exception:
        return default


def save_json(path, data):
    tmp = path + ".tmp"
    with open(tmp, "w", encoding="utf-8") as f:
        json.dump(data, f, indent=2)
    os.replace(tmp, path)


class Store:
    """Thread-safe settings (config.json) + runtime state (state.json)."""

    def __init__(self):
        self.lock = threading.RLock()
        user = load_json(CONFIG_PATH, None)
        self.cfg = dict(DEFAULT_CONFIG)
        if isinstance(user, dict):
            self.cfg.update({k: v for k, v in user.items() if k in DEFAULT_CONFIG})
        if user != self.cfg:
            save_json(CONFIG_PATH, self.cfg)
        self.state = load_json(STATE_PATH, {})
        if not isinstance(self.state, dict):
            self.state = {}

    def update_cfg(self, changes):
        with self.lock:
            for k, v in changes.items():
                if k in DEFAULT_CONFIG and type(v) is type(DEFAULT_CONFIG[k]):
                    self.cfg[k] = v
            save_json(CONFIG_PATH, self.cfg)

    def save_state(self):
        with self.lock:
            try:
                save_json(STATE_PATH, self.state)
            except OSError as e:
                log.warning("state save: %r", e)
