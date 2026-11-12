"""
Claude Usage Widget for Windows
--------------------------------
A small pill above the taskbar (draggable) that shows your Claude plan limits
live -- the "5-hour" and "Weekly" numbers the Claude app shows -- with a
details card (bars or rings view), a pace forecast, alerts, a tray icon and a
taskbar-button progress overlay.

Data sources
  * Plan limits: the Claude Code CLI's sign-in token (~/.claude/.credentials.json),
    used to ask Anthropic for your usage (what `/usage` shows).
    Sign in once with:  claude   ->   /login
  * Live activity: Claude Code session logs in ~/.claude/projects (read-only).

Run:   pythonw claude_usage_widget.pyw          (--demo for fake limit data)
"""

import ctypes
import ctypes.wintypes as wt
import datetime as dt
import glob
import json
import logging
import os
import queue
import shutil
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.request
import winreg

import tkinter as tk
from PIL import Image, ImageDraw, ImageFont, ImageTk

try:
    import pystray
    HAVE_TRAY = True
except Exception:
    HAVE_TRAY = False

# =========================================================================== config
APP = "ClaudeUsageWidget"
DATA_DIR = os.path.join(os.environ.get("APPDATA", os.path.expanduser("~")), APP)
CLAUDE_DIR = os.path.join(os.path.expanduser("~"), ".claude")
CREDS_PATH = os.path.join(CLAUDE_DIR, ".credentials.json")
PROJECTS_DIR = os.path.join(CLAUDE_DIR, "projects")
CONFIG_PATH = os.path.join(DATA_DIR, "config.json")
STATE_PATH = os.path.join(DATA_DIR, "state.json")
LOG_PATH = os.path.join(DATA_DIR, "widget.log")

USAGE_URL = "https://api.anthropic.com/api/oauth/usage"
TOKEN_URLS = ["https://console.anthropic.com/v1/oauth/token",
              "https://platform.claude.com/v1/oauth/token"]
CLIENT_ID = "9d1c250a-e61b-44d9-88ed-5944d1962f5e"  # Claude Code's public OAuth client id
USER_AGENT = "claude-usage-widget/2.0"

DEFAULT_CONFIG = {
    "limits_refresh_seconds": 60,   # how often to ask Anthropic for plan limits
    "logs_refresh_seconds": 5,      # how often to scan local Claude Code logs
    "alert_thresholds": [75, 90],   # % at which to send a Windows notification
    "alert_on_reset": True,         # notify when a limit window resets
    "alert_on_forecast": True,      # notify when on pace to hit the 5h limit within 30 min
    "dim_opacity": 0.65,            # pill opacity when the mouse isn't over it
    "hide_in_fullscreen": True,
    "taskbar_button": True,         # taskbar button with a progress overlay
    "show_model_limits": False,     # also show per-model weekly limits (e.g. Sonnet)
    "margin_px": 8,
}

os.makedirs(DATA_DIR, exist_ok=True)
logging.basicConfig(filename=LOG_PATH, level=logging.INFO,
                    format="%(asctime)s %(levelname)s %(message)s")
log = logging.getLogger(APP)


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


def load_config():
    cfg = dict(DEFAULT_CONFIG)
    user = load_json(CONFIG_PATH, None)
    if isinstance(user, dict):
        cfg.update({k: v for k, v in user.items() if k in DEFAULT_CONFIG})
    if user != cfg:
        save_json(CONFIG_PATH, cfg)  # add any new keys so the user can edit them
    return cfg


# =========================================================================== win32
user32 = ctypes.windll.user32
dwmapi = ctypes.windll.dwmapi
kernel32 = ctypes.windll.kernel32
ole32 = ctypes.windll.ole32


class MONITORINFO(ctypes.Structure):
    _fields_ = [("cbSize", wt.DWORD), ("rcMonitor", wt.RECT),
                ("rcWork", wt.RECT), ("dwFlags", wt.DWORD)]


user32.GetForegroundWindow.restype = wt.HWND
user32.MonitorFromWindow.restype = wt.HMONITOR
user32.MonitorFromWindow.argtypes = [wt.HWND, wt.DWORD]
user32.MonitorFromPoint.restype = wt.HMONITOR
user32.MonitorFromPoint.argtypes = [wt.POINT, wt.DWORD]
user32.GetParent.restype = wt.HWND
user32.GetParent.argtypes = [wt.HWND]
user32.GetAsyncKeyState.restype = ctypes.c_short


def monitor_info(x, y):
    mon = user32.MonitorFromPoint(wt.POINT(int(x), int(y)), 2)  # nearest
    mi = MONITORINFO()
    mi.cbSize = ctypes.sizeof(MONITORINFO)
    user32.GetMonitorInfoW(mon, ctypes.byref(mi))
    return mon, mi.rcMonitor, mi.rcWork


def primary_work_area():
    r = wt.RECT()
    user32.SystemParametersInfoW(0x30, 0, ctypes.byref(r), 0)
    return r.left, r.top, r.right, r.bottom


def virtual_screen():
    x, y = user32.GetSystemMetrics(76), user32.GetSystemMetrics(77)
    return x, y, x + user32.GetSystemMetrics(78), y + user32.GetSystemMetrics(79)


def foreground_is_fullscreen(own_hwnds, on_monitor):
    h = user32.GetForegroundWindow()
    if not h or h in own_hwnds:
        return False
    cls = ctypes.create_unicode_buffer(256)
    user32.GetClassNameW(h, cls, 256)
    if cls.value in ("Progman", "WorkerW", "Shell_TrayWnd", "Shell_SecondaryTrayWnd"):
        return False
    mon = user32.MonitorFromWindow(h, 2)
    if mon != on_monitor:
        return False
    r = wt.RECT()
    user32.GetWindowRect(h, ctypes.byref(r))
    mi = MONITORINFO()
    mi.cbSize = ctypes.sizeof(MONITORINFO)
    user32.GetMonitorInfoW(mon, ctypes.byref(mi))
    m = mi.rcMonitor
    return r.left <= m.left and r.top <= m.top and r.right >= m.right and r.bottom >= m.bottom


def set_topmost(hwnd):
    user32.SetWindowPos(hwnd, wt.HWND(-1), 0, 0, 0, 0, 0x0002 | 0x0001 | 0x0010)


def style_window(hwnd, dark):
    try:
        pref = ctypes.c_int(2)  # rounded corners
        dwmapi.DwmSetWindowAttribute(hwnd, 33, ctypes.byref(pref), 4)
        border = ctypes.c_uint(0x00383838 if dark else 0x00D8D8D8)
        dwmapi.DwmSetWindowAttribute(hwnd, 34, ctypes.byref(border), 4)
    except Exception:
        pass


def windows_uses_light_theme():
    try:
        with winreg.OpenKey(winreg.HKEY_CURRENT_USER,
                            r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize") as k:
            return winreg.QueryValueEx(k, "SystemUsesLightTheme")[0] == 1
    except OSError:
        return False


RUN_KEY = r"Software\Microsoft\Windows\CurrentVersion\Run"


def autostart_command():
    exe = sys.executable
    if exe.lower().endswith("python.exe"):
        exe = exe[:-10] + "pythonw.exe"
    return f'"{exe}" "{os.path.abspath(__file__)}"'


def autostart_enabled():
    try:
        with winreg.OpenKey(winreg.HKEY_CURRENT_USER, RUN_KEY) as k:
            winreg.QueryValueEx(k, APP)
            return True
    except OSError:
        return False


def set_autostart(on):
    with winreg.OpenKey(winreg.HKEY_CURRENT_USER, RUN_KEY, 0, winreg.KEY_SET_VALUE) as k:
        if on:
            winreg.SetValueEx(k, APP, 0, winreg.REG_SZ, autostart_command())
        else:
            try:
                winreg.DeleteValue(k, APP)
            except OSError:
                pass


class GUID(ctypes.Structure):
    _fields_ = [("d1", ctypes.c_ulong), ("d2", ctypes.c_ushort),
                ("d3", ctypes.c_ushort), ("d4", ctypes.c_ubyte * 8)]


def _guid(s):
    g = GUID()
    ole32.CLSIDFromString(s, ctypes.byref(g))
    return g


class TaskbarProgress:
    """ITaskbarList3 progress overlay on a taskbar button (green / yellow / red)."""
    NOPROGRESS, NORMAL, ERROR, PAUSED = 0, 2, 4, 8

    def __init__(self):
        self.ptr = None
        try:
            ole32.CoInitialize(None)
            p = ctypes.c_void_p()
            hr = ole32.CoCreateInstance(ctypes.byref(_guid("{56FDF344-FD6D-11d0-958A-006097C9A090}")),
                                        None, 1,
                                        ctypes.byref(_guid("{EA1AFB91-9E28-4B86-90E9-9E9F8A5EEFAF}")),
                                        ctypes.byref(p))
            if hr == 0 and p.value:
                self.ptr = p
                self._call(3, [])  # HrInit
        except Exception as e:
            log.warning("taskbar progress unavailable: %r", e)
            self.ptr = None

    def _call(self, idx, argtypes, *args):
        vtbl = ctypes.cast(self.ptr, ctypes.POINTER(ctypes.POINTER(ctypes.c_void_p))).contents
        fn = ctypes.WINFUNCTYPE(ctypes.c_long, ctypes.c_void_p, *argtypes)(vtbl[idx])
        return fn(self.ptr, *args)

    def set(self, hwnd, pct, state):
        if not self.ptr or not hwnd:
            return
        try:
            self._call(10, [wt.HWND, ctypes.c_int], hwnd, state)
            if state != self.NOPROGRESS:
                self._call(9, [wt.HWND, ctypes.c_ulonglong, ctypes.c_ulonglong],
                           hwnd, max(1, int(pct)), 100)
        except Exception as e:
            log.warning("taskbar progress: %r", e)


# =========================================================================== formatting
def parse_iso(s):
    if not s:
        return None
    try:
        d = dt.datetime.fromisoformat(s.replace("Z", "+00:00"))
        return d if d.tzinfo else d.replace(tzinfo=dt.timezone.utc)
    except Exception:
        return None


def now_utc():
    return dt.datetime.now(dt.timezone.utc)


def reset_key(t):
    """Stable id for a limit window (reset times can jitter by seconds between polls)."""
    return str(round(t.timestamp() / 600)) if t else ""


def fmt_tokens(n):
    n = float(n)
    if n >= 1e9:
        return f"{n / 1e9:.1f}B"
    if n >= 1e6:
        return f"{n / 1e6:.1f}M"
    if n >= 1e4:
        return f"{n / 1e3:.0f}k"
    if n >= 1e3:
        return f"{n / 1e3:.1f}k"
    return f"{int(n)}"


def fmt_dur(secs):
    secs = max(0, int(secs))
    d, rem = divmod(secs, 86400)
    h, rem = divmod(rem, 3600)
    m = rem // 60
    if d:
        return f"{d}d {h}h"
    if h:
        return f"{h}h{m:02d}"
    return f"{max(m, 1)}m"


def fmt_clock(t):
    return t.astimezone().strftime("%I:%M %p").lstrip("0")


def fmt_day_time(t):
    local = t.astimezone()
    today = dt.datetime.now().astimezone().date()
    if local.date() == today:
        return fmt_clock(t)
    if local.date() == today + dt.timedelta(days=1):
        return "tomorrow " + fmt_clock(t)
    return local.strftime("%a ") + fmt_clock(t)


def fmt_short_day(t):
    local = t.astimezone()
    if local.date() == dt.datetime.now().astimezone().date():
        return fmt_clock(t)
    return local.strftime("%a ") + local.strftime("%I%p").lstrip("0")


# =========================================================================== http
class HttpError(Exception):
    def __init__(self, code):
        super().__init__(f"HTTP {code}")
        self.code = code


CURL = shutil.which("curl.exe") or os.path.join(os.environ.get("SystemRoot", r"C:\Windows"),
                                                 "System32", "curl.exe")


def http_json(url, headers, body=None):
    """GET/POST JSON. Uses Windows' curl (Cloudflare can block Python's TLS
    fingerprint); secrets go via stdin / a temp file, never the command line."""
    headers = dict(headers, **{"User-Agent": USER_AGENT, "Accept": "application/json"})
    if os.path.exists(CURL):
        args = [CURL, "-sS", "--max-time", "20", "-H", "@-", "-w", "\n%{http_code}"]
        tmp = None
        if body is not None:
            tmp = os.path.join(DATA_DIR, f".req-{os.getpid()}.json")
            with open(tmp, "w", encoding="utf-8") as f:
                f.write(body)
            args += ["-X", "POST", "--data-binary", "@" + tmp]
            headers["Content-Type"] = "application/json"
        args.append(url)
        try:
            r = subprocess.run(args, input="\n".join(f"{k}: {v}" for k, v in headers.items()),
                               capture_output=True, text=True, timeout=30,
                               creationflags=0x08000000)  # CREATE_NO_WINDOW
        finally:
            if tmp and os.path.exists(tmp):
                os.remove(tmp)
        if r.returncode != 0:
            raise ConnectionError(r.stderr.strip()[:200] or f"curl exit {r.returncode}")
        text, _, code = r.stdout.rpartition("\n")
        code = int(code or 0)
        if not 200 <= code < 300:
            raise HttpError(code)
        return json.loads(text)
    req = urllib.request.Request(url, headers=headers,
                                 data=body.encode() if body is not None else None)
    if body is not None:
        req.add_header("Content-Type", "application/json")
    try:
        with urllib.request.urlopen(req, timeout=20) as r:
            return json.load(r)
    except urllib.error.HTTPError as e:
        raise HttpError(e.code)
    except urllib.error.URLError as e:
        raise ConnectionError(str(e.reason))


# =========================================================================== plan limits
LIMIT_META = {
    "five_hour": ("Session", "5h sliding window", "5h"),
    "seven_day": ("Weekly", "All models", "Wk"),
}


class LimitsFetcher(threading.Thread):
    def __init__(self, cfg, demo=False):
        super().__init__(daemon=True)
        self.cfg, self.demo = cfg, demo
        self.lock = threading.Lock()
        self.wake = threading.Event()
        self.windows, self.plan = {}, None
        self.status, self.error, self.updated = "loading", "", None
        self.backoff = 0
        self.t0 = time.time()

    def snapshot(self):
        with self.lock:
            return dict(windows=dict(self.windows), plan=self.plan, status=self.status,
                        error=self.error, updated=self.updated)

    def refresh_now(self):
        self.wake.set()

    @staticmethod
    def _read_oauth():
        data = load_json(CREDS_PATH, {})
        return data.get("claudeAiOauth") if isinstance(data, dict) else None

    def _refresh_token(self, oauth):
        body = json.dumps({"grant_type": "refresh_token", "refresh_token": oauth["refreshToken"],
                           "client_id": CLIENT_ID})
        last = None
        for url in TOKEN_URLS:
            try:
                tok = http_json(url, {}, body)
                break
            except Exception as e:
                last = e
        else:
            raise last
        data = load_json(CREDS_PATH, {})
        cur = data.get("claudeAiOauth") or {}
        if cur.get("refreshToken") != oauth["refreshToken"]:
            return cur  # the CLI refreshed it meanwhile
        cur["accessToken"] = tok["access_token"]
        if tok.get("refresh_token"):
            cur["refreshToken"] = tok["refresh_token"]
        cur["expiresAt"] = int((time.time() + int(tok.get("expires_in", 3600))) * 1000)
        data["claudeAiOauth"] = cur
        save_json(CREDS_PATH, data)
        log.info("refreshed access token")
        return cur

    def _get_usage(self, token):
        return http_json(USAGE_URL, {"Authorization": f"Bearer {token}",
                                     "anthropic-beta": "oauth-2025-04-20"})

    def _parse(self, resp):
        out = {}
        for key, val in resp.items():
            if not isinstance(val, dict) or val.get("utilization") is None:
                continue
            if key in LIMIT_META:
                title, sub, short = LIMIT_META[key]
            elif key.startswith("seven_day_") and self.cfg["show_model_limits"]:
                name = key[len("seven_day_"):].replace("_", " ").title()
                title, sub, short = name, "Dedicated weekly limit", name[:2]
            else:
                continue
            out[key] = {"title": title, "sub": sub, "short": short,
                        "pct": float(val["utilization"]), "resets": parse_iso(val.get("resets_at"))}
        return out

    def _demo(self):
        now = dt.datetime.fromtimestamp(self.t0, dt.timezone.utc)
        el = time.time() - self.t0
        five = min(100.0, 31 + el / 8)  # rises ~7.5%/min so the forecast shows
        return {
            "five_hour": {"title": "Session", "sub": "5h sliding window", "short": "5h", "pct": five,
                          "resets": now + dt.timedelta(hours=3, minutes=1)},
            "seven_day": {"title": "Weekly", "sub": "All models", "short": "Wk", "pct": 54.0,
                          "resets": now + dt.timedelta(days=4, hours=2)},
        }

    def poll_once(self):
        if self.demo:
            with self.lock:
                self.windows, self.plan, self.status = self._demo(), "Pro", "ok"
                self.updated = time.time()
            return
        oauth = self._read_oauth()
        if not oauth or not oauth.get("accessToken"):
            with self.lock:
                self.status, self.error = "signin", ""
            return
        plan = (oauth.get("subscriptionType") or "").title() or None
        try:
            if oauth.get("expiresAt") and oauth["expiresAt"] / 1000 < time.time() + 60 \
                    and oauth.get("refreshToken"):
                oauth = self._refresh_token(oauth)
            try:
                resp = self._get_usage(oauth["accessToken"])
            except HttpError as e:
                if e.code == 401 and oauth.get("refreshToken"):
                    oauth = self._refresh_token(oauth)
                    resp = self._get_usage(oauth["accessToken"])
                else:
                    raise
            windows = self._parse(resp)
            with self.lock:
                self.windows, self.plan, self.status, self.error = windows, plan, "ok", ""
                self.updated = time.time()
            self.backoff = 0
        except HttpError as e:
            log.warning("usage %s", e)
            with self.lock:
                if e.code in (401, 403):
                    self.status, self.error = "expired", "Sign-in expired"
                else:
                    self.status, self.error = "error", f"HTTP {e.code}"
            if e.code == 429:
                self.backoff = min(max(self.backoff * 2, 120), 900)
        except Exception as e:
            log.warning("usage fetch failed: %r", e)
            with self.lock:
                self.status, self.error = "error", "Offline"

    def run(self):
        while True:
            self.poll_once()
            wait = 2 if self.demo else max(30, self.cfg["limits_refresh_seconds"]) + self.backoff
            self.wake.wait(wait)
            self.wake.clear()


class Forecast:
    """Estimates when the 5-hour limit will be hit from recent % samples."""

    def __init__(self, min_span):
        self.samples = []  # (epoch, pct)
        self.rkey = None
        self.min_span = min_span

    def add(self, w, updated):
        rkey = reset_key(w["resets"])
        if rkey != self.rkey:
            self.samples, self.rkey = [], rkey
        if not self.samples or self.samples[-1][0] != updated:
            self.samples.append((updated, w["pct"]))
        cutoff = time.time() - 1800
        self.samples = [s for s in self.samples if s[0] >= cutoff][-120:]

    def eta_seconds(self, w):
        """None = not enough data; inf = not rising; else seconds until 100%."""
        s = self.samples
        if len(s) < 3 or s[-1][0] - s[0][0] < self.min_span:
            return None
        n = len(s)
        mt = sum(t for t, _ in s) / n
        mp = sum(p for _, p in s) / n
        var = sum((t - mt) ** 2 for t, _ in s)
        slope = sum((t - mt) * (p - mp) for t, p in s) / var if var else 0  # %/sec
        if slope <= 0.0005 / 60:
            return float("inf")
        return (100 - w["pct"]) / slope


# =========================================================================== local logs
class LogScanner(threading.Thread):
    KEEP_DAYS = 8

    def __init__(self, cfg):
        super().__init__(daemon=True)
        self.cfg = cfg
        self.lock = threading.Lock()
        self.offsets, self.partial, self.events = {}, {}, {}
        self.stats = None

    def _scan(self):
        cutoff = time.time() - self.KEEP_DAYS * 86400
        for path in glob.glob(os.path.join(PROJECTS_DIR, "**", "*.jsonl"), recursive=True):
            try:
                st = os.stat(path)
            except OSError:
                continue
            if path not in self.offsets and st.st_mtime < cutoff:
                continue
            off = self.offsets.get(path, 0)
            if st.st_size < off:
                off, self.partial[path] = 0, b""
            if st.st_size == off:
                continue
            try:
                with open(path, "rb") as f:
                    f.seek(off)
                    chunk = f.read()
            except OSError:
                continue
            self.offsets[path] = off + len(chunk)
            lines = (self.partial.get(path, b"") + chunk).split(b"\n")
            self.partial[path] = lines.pop()
            for ln in lines:
                if b'"usage"' not in ln:
                    continue
                try:
                    o = json.loads(ln)
                    m = o.get("message") or {}
                    u = m.get("usage")
                    ts = parse_iso(o.get("timestamp"))
                    if not isinstance(u, dict) or not ts:
                        continue
                    tokens = (u.get("input_tokens") or 0) + (u.get("output_tokens") or 0) \
                        + (u.get("cache_creation_input_tokens") or 0)
                    self.events[(m.get("id"), o.get("requestId"))] = (
                        ts.timestamp(), tokens, o.get("sessionId"))
                except Exception:
                    continue
        self.events = {k: v for k, v in self.events.items() if v[0] >= cutoff}

    def _compute(self):
        now = time.time()
        today0 = dt.datetime.now().replace(hour=0, minute=0, second=0, microsecond=0)
        days = [(today0 - dt.timedelta(days=i)).timestamp() for i in range(6, -1, -1)]
        daily = [0] * 7
        today = msgs = week = burn = 0
        active = set()
        for ts, tok, sess in self.events.values():
            if ts >= days[0]:
                week += tok
                daily[max(i for i, d in enumerate(days) if ts >= d)] += tok
            if ts >= days[-1]:
                today += tok
                msgs += 1
            if ts >= now - 600:
                burn += tok
            if ts >= now - 300 and sess:
                active.add(sess)
        labels = [dt.datetime.fromtimestamp(d).strftime("%a")[:2] for d in days]
        return dict(today=today, msgs=msgs, week=week, burn=burn / 10, active=len(active),
                    daily=daily, labels=labels)

    def snapshot(self):
        with self.lock:
            return self.stats

    def run(self):
        while True:
            try:
                self._scan()
                s = self._compute()
                with self.lock:
                    self.stats = s
            except Exception as e:
                log.warning("log scan failed: %r", e)
            time.sleep(max(2, self.cfg["logs_refresh_seconds"]))


# =========================================================================== drawing
THEMES = {
    "dark": dict(bg="#121212", fg="#F4F4F4", sub="#8C8C8C", track="#262626", line="#242424",
                 green="#34D162"),
    "light": dict(bg="#FBFBFB", fg="#151515", sub="#6E6E6E", track="#E6E6E6", line="#E8E8E8",
                  green="#1EA84A"),
}
AMBER, RED, ORANGE = "#F5A524", "#FF4D45", "#E8743B"
SS = 2  # supersampling factor for antialiasing
FONT_FILES = {"r": "segoeui.ttf", "sb": "seguisb.ttf", "b": "segoeuib.ttf"}


def level_color(pct, t):
    return RED if pct >= 90 else AMBER if pct >= 75 else t["green"]


def rgb(h, a=255):
    return (int(h[1:3], 16), int(h[3:5], 16), int(h[5:7], 16), a)


class Painter:
    """Draws in logical px on a supersampled canvas; finish() returns a crisp PhotoImage."""
    _fonts = {}

    def __init__(self, w, h, scale, bg):
        self.w, self.scale = w, scale
        self.k = scale * SS
        self.img = Image.new("RGBA", (round(w * self.k), round(h * self.k)), rgb(bg))
        self.d = ImageDraw.Draw(self.img)

    def font(self, kind, size):
        key = (kind, round(size * self.k))
        if key not in self._fonts:
            try:
                self._fonts[key] = ImageFont.truetype(FONT_FILES[kind], key[1])
            except OSError:
                self._fonts[key] = ImageFont.load_default()
        return self._fonts[key]

    def text(self, x, y, s, kind, size, color, anchor="ls"):
        self.d.text((x * self.k, y * self.k), s, font=self.font(kind, size), fill=rgb(color),
                    anchor=anchor)

    def text_w(self, s, kind, size):
        return self.d.textlength(s, font=self.font(kind, size)) / self.k

    def line(self, x0, y0, x1, y1, color, w=1):
        k = self.k
        self.d.line((x0 * k, y0 * k, x1 * k, y1 * k), fill=rgb(color), width=max(1, round(w * k)))

    def rrect(self, x0, y0, x1, y1, r, color):
        k = self.k
        self.d.rounded_rectangle((x0 * k, y0 * k, x1 * k, y1 * k), radius=r * k, fill=rgb(color))

    def circle(self, cx, cy, r, color=None, outline=None, w=1):
        k = self.k
        self.d.ellipse(((cx - r) * k, (cy - r) * k, (cx + r) * k, (cy + r) * k),
                       fill=rgb(color) if color else None,
                       outline=rgb(outline) if outline else None, width=round(w * k))

    def bar(self, x0, y, x1, h, pct, color, track):
        self.rrect(x0, y, x1, y + h, h / 2, track)
        if pct > 0:
            xe = x0 + max(h, (x1 - x0) * min(pct, 100) / 100)
            self.rrect(x0, y, xe, y + h, h / 2, color)

    def ring(self, cx, cy, r, w, pct, color, track):
        k = self.k
        box = ((cx - r) * k, (cy - r) * k, (cx + r) * k, (cy + r) * k)
        self.d.ellipse(box, outline=rgb(track), width=round(w * k))
        if pct > 0:
            end = -90 + 360 * min(pct, 100) / 100
            self.d.arc(box, -90, end, fill=rgb(color), width=round(w * k))
            import math
            rr = r - w / 2
            for ang in (-90, end):  # round caps
                a = math.radians(ang)
                self.circle(cx + rr * math.cos(a), cy + rr * math.sin(a), w / 2, color)

    # -- small icons (16px box centred at cx, cy)
    def icon_logo(self, cx, cy, color=ORANGE):
        import math
        for i in range(8):
            a = math.radians(i * 45 + 22.5)
            self.line(cx + 2.2 * math.cos(a), cy + 2.2 * math.sin(a),
                      cx + 7.5 * math.cos(a), cy + 7.5 * math.sin(a), color, 2.2)
        self.circle(cx, cy, 2.4, color)

    def icon_timer(self, cx, cy, color):
        self.circle(cx, cy + 1, 7, outline=color, w=1.8)
        self.line(cx, cy + 1, cx + 3, cy - 2, color, 1.8)
        self.line(cx - 2.5, cy - 8, cx + 2.5, cy - 8, color, 1.8)

    def icon_bars(self, cx, cy, color):
        for i, hgt in enumerate((6, 10, 14)):
            x = cx - 7 + i * 5.5
            self.rrect(x, cy + 7 - hgt, x + 3.6, cy + 7, 1, color)

    def icon_spark(self, cx, cy, color):
        self.line(cx - 6, cy + 6, cx + 4, cy - 4, color, 1.8)
        self.line(cx + 4, cy - 8, cx + 4, cy - 2, color, 1.4)
        self.line(cx + 1, cy - 5, cx + 7, cy - 5, color, 1.4)

    def icon_rings(self, cx, cy, color):
        self.circle(cx, cy, 6.5, outline=color, w=2)

    def icon_list(self, cx, cy, color):
        for dy in (-5, 0, 5):
            self.rrect(cx - 7, cy + dy - 1.2, cx + 7, cy + dy + 1.2, 1.2, color)

    def finish(self, h):
        img = self.img.crop((0, 0, self.img.width, round(h * self.k)))
        img = img.resize((round(self.w * self.scale), round(h * self.scale)), Image.LANCZOS)
        return ImageTk.PhotoImage(img), img


def ring_icon_image(pct, color):
    s = 64
    img = Image.new("RGBA", (s * 2, s * 2), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    d.ellipse((8, 8, 2 * s - 8, 2 * s - 8), outline=(90, 90, 90, 255), width=16)
    if pct is not None:
        d.arc((8, 8, 2 * s - 8, 2 * s - 8), -90, -90 + 360 * min(pct, 100) / 100,
              fill=rgb(color), width=16)
        txt = str(int(round(pct)))
        try:
            fnt = ImageFont.truetype("segoeuib.ttf", 54 if len(txt) < 3 else 40)
        except OSError:
            fnt = ImageFont.load_default()
        d.text((s, s + 2), txt, font=fnt, anchor="mm", fill=(255, 255, 255, 255),
               stroke_width=4, stroke_fill=(0, 0, 0, 220))
    return img.resize((s, s), Image.LANCZOS)


# =========================================================================== tray
class Tray:
    def __init__(self, commands):
        self.commands, self.icon, self.last_key = commands, None, None
        if not HAVE_TRAY:
            return
        menu = pystray.Menu(
            pystray.MenuItem("Show details", lambda: commands.put("toggle"), default=True),
            pystray.MenuItem("Refresh now", lambda: commands.put("refresh")),
            pystray.MenuItem("Start with Windows", lambda: commands.put("autostart"),
                             checked=lambda _i: autostart_enabled()),
            pystray.MenuItem("Reset position", lambda: commands.put("resetpos")),
            pystray.MenuItem("Open settings folder", lambda: os.startfile(DATA_DIR)),
            pystray.Menu.SEPARATOR,
            pystray.MenuItem("Quit", lambda: commands.put("quit")),
        )
        self.icon = pystray.Icon(APP, ring_icon_image(None, "#808080"), "Claude usage", menu)
        self.icon.run_detached()

    def update(self, pct, color, tip):
        if not self.icon:
            return
        key = (None if pct is None else int(round(pct)), color)
        if key != self.last_key:
            self.icon.icon = ring_icon_image(pct, color)
            self.last_key = key
        self.icon.title = tip[:127]

    def notify(self, title, msg):
        if self.icon:
            try:
                self.icon.notify(msg, title)
            except Exception as e:
                log.warning("notify failed: %r", e)

    def stop(self):
        if self.icon:
            self.icon.stop()


# =========================================================================== widget
class Widget:
    PILL_W, PILL_H = 236, 50
    CARD_W = 360

    def __init__(self, demo=False):
        try:
            ctypes.windll.shcore.SetProcessDpiAwareness(2)
        except Exception:
            pass
        try:  # own taskbar group + icon instead of Python's
            ctypes.windll.shell32.SetCurrentProcessExplicitAppUserModelID(APP)
        except Exception:
            pass
        self.demo = demo
        self.cfg = load_config()
        self.state = load_json(STATE_PATH, {})
        if not demo and not self.state.get("autostart_initialized"):
            try:
                set_autostart(True)
            except OSError as e:
                log.warning("autostart: %r", e)
            self.state["autostart_initialized"] = True
            self.save_state()

        self.limits = LimitsFetcher(self.cfg, demo)
        self.logs = LogScanner(self.cfg)
        self.forecast = Forecast(min_span=20 if demo else 240)
        self.limits.start()
        self.logs.start()
        self.commands = queue.Queue()

        self.root = tk.Tk()
        self.root.title("Claude usage pill")
        self.sc = self.root.winfo_fpixels("1i") / 96.0
        self.root.overrideredirect(True)
        self.root.attributes("-topmost", True)
        self.light = windows_uses_light_theme()
        self.t = THEMES["light" if self.light else "dark"]
        self.root.configure(bg=self.t["bg"])

        self.pill = tk.Label(self.root, bd=0, highlightthickness=0, cursor="hand2", bg=self.t["bg"])
        self.pill.pack()
        self.pill.bind("<ButtonPress-1>", self.on_press)
        self.pill.bind("<B1-Motion>", self.on_drag)
        self.pill.bind("<ButtonRelease-1>", self.on_release)
        self.pill.bind("<Button-3>", self.context_menu)
        self.pill.bind("<Enter>", lambda e: self.set_hover(True))
        self.pill.bind("<Leave>", lambda e: self.set_hover(False))
        self.hover, self.drag = False, None
        self.panel, self.panel_key, self.pill_key = None, None, None
        self.hidden_fullscreen = False
        self.forecast_alerted = None
        self.root.update_idletasks()
        self.hwnd = user32.GetParent(self.root.winfo_id())
        style_window(self.hwnd, not self.light)
        self.place_pill()
        self.apply_opacity()

        self.tray = Tray(self.commands)
        self.progress = TaskbarProgress()
        self.tb_win, self.tb_key = None, None
        if self.cfg["taskbar_button"]:
            self.make_taskbar_button()
        self.tick()
        self.slow_tick()

    def px(self, v):
        return int(round(v * self.sc))

    def save_state(self):
        try:
            save_json(STATE_PATH, self.state)
        except OSError as e:
            log.warning("state: %r", e)

    # ------------------------------------------------------------ taskbar button
    def make_taskbar_button(self):
        """A minimised, invisible window whose taskbar button shows progress.
        Clicking the button opens the details card."""
        w = tk.Toplevel(self.root)
        w.title("Claude usage")
        w.attributes("-alpha", 0.0)
        w.geometry("1x1+-32000+-32000")
        w.protocol("WM_DELETE_WINDOW", lambda: self.handle("quit"))
        w.iconify()
        self.tb_win = w
        self.root.after(800, lambda: w.bind("<Map>", self._tb_clicked))

    def _tb_clicked(self, _e=None):
        if self.tb_win and self.tb_win.state() == "normal":
            self.root.after(10, self.tb_win.iconify)
            self.toggle_panel()

    def update_taskbar(self, model):
        if not self.tb_win:
            return
        five = model["rows"][0] if model["rows"] else None
        key = (five["pct_i"], five["color"]) if five else None
        if key == self.tb_key:
            return
        self.tb_key = key
        hwnd = user32.GetParent(self.tb_win.winfo_id())
        img = ring_icon_image(five["pct"] if five else None, five["color"] if five else "#808080")
        self._tb_icon = ImageTk.PhotoImage(img)
        self.tb_win.iconphoto(False, self._tb_icon)
        if five:
            st = (TaskbarProgress.ERROR if five["pct"] >= 90 else
                  TaskbarProgress.PAUSED if five["pct"] >= 75 else TaskbarProgress.NORMAL)
            self.progress.set(hwnd, five["pct"], st)
            self.tb_win.title(f"Claude {five['pct_i']}% \u00b7 resets in {five['reset_short']}")
        else:
            self.progress.set(hwnd, 0, TaskbarProgress.NOPROGRESS)
            self.tb_win.title("Claude usage")

    # ------------------------------------------------------------ placement / drag
    def default_pos(self):
        _l, _t, r, b = primary_work_area()
        m = self.px(self.cfg["margin_px"])
        return r - self.px(self.PILL_W) - m, b - self.px(self.PILL_H) - m

    def place_pill(self):
        w, h = self.px(self.PILL_W), self.px(self.PILL_H)
        pos = self.state.get("pill_pos")
        if pos:
            vx0, vy0, vx1, vy1 = virtual_screen()
            x = min(max(pos[0], vx0), vx1 - w)
            y = min(max(pos[1], vy0), vy1 - h)
            # make sure it's actually on a monitor (e.g. one was unplugged)
            _m, mon, _w = monitor_info(x + w / 2, y + h / 2)
            x = min(max(x, mon.left), mon.right - w)
            y = min(max(y, mon.top), mon.bottom - h)
        else:
            x, y = self.default_pos()
        self.pill_pos = (int(x), int(y))
        self.root.geometry(f"{w}x{h}+{self.pill_pos[0]}+{self.pill_pos[1]}")

    def on_press(self, e):
        self.drag = [e.x_root, e.y_root, self.root.winfo_x(), self.root.winfo_y(), False]

    def on_drag(self, e):
        if not self.drag:
            return
        sx, sy, wx, wy, moved = self.drag
        dx, dy = e.x_root - sx, e.y_root - sy
        if not moved and abs(dx) + abs(dy) < self.px(5):
            return
        if not moved:
            self.close_panel()
            self.drag[4] = True
        self.pill_pos = (wx + dx, wy + dy)
        self.root.geometry(f"+{wx + dx}+{wy + dy}")

    def on_release(self, _e):
        if self.drag and self.drag[4]:
            self.state["pill_pos"] = list(self.pill_pos)
            self.save_state()
        else:
            self.toggle_panel()
        self.drag = None

    def reset_position(self):
        self.state.pop("pill_pos", None)
        self.save_state()
        self.place_pill()

    def set_hover(self, on):
        self.hover = on
        self.apply_opacity()

    def apply_opacity(self):
        full = self.hover or self.panel is not None
        self.root.attributes("-alpha", 1.0 if full else float(self.cfg["dim_opacity"]))

    # ------------------------------------------------------------ loop
    def tick(self):
        try:
            while True:
                self.handle(self.commands.get_nowait())
        except queue.Empty:
            pass
        try:
            lim, stats = self.limits.snapshot(), self.logs.snapshot()
            five = lim["windows"].get("five_hour")
            if five and lim["status"] == "ok" and lim["updated"]:
                self.forecast.add(five, lim["updated"])
            model = self.build_model(lim, stats)
            self.check_alerts(lim, model)
            self.render_pill(model)
            if self.panel:
                self.render_panel(model)
            self.update_tray(model)
            self.update_taskbar(model)
            if self.cfg["hide_in_fullscreen"]:
                cx = self.pill_pos[0] + self.px(self.PILL_W) // 2
                cy = self.pill_pos[1] + self.px(self.PILL_H) // 2
                mon = monitor_info(cx, cy)[0]
                fs = foreground_is_fullscreen({self.hwnd}, mon)
                if fs and not self.hidden_fullscreen:
                    self.close_panel()
                    self.root.withdraw()
                    self.hidden_fullscreen = True
                elif not fs and self.hidden_fullscreen:
                    self.root.deiconify()
                    self.hidden_fullscreen = False
            if not self.hidden_fullscreen:
                set_topmost(self.hwnd)
        except Exception:
            log.exception("tick failed")
        self.root.after(1000, self.tick)

    def slow_tick(self):
        light = windows_uses_light_theme()
        if light != self.light:
            self.light = light
            self.t = THEMES["light" if light else "dark"]
            self.root.configure(bg=self.t["bg"])
            self.pill.configure(bg=self.t["bg"])
            style_window(self.hwnd, not light)
            self.close_panel()
            self.pill_key = self.tb_key = None
        if not self.drag:
            self.place_pill()
        self.root.after(15000, self.slow_tick)

    def handle(self, cmd):
        if cmd == "toggle":
            self.toggle_panel()
        elif cmd == "refresh":
            self.limits.refresh_now()
        elif cmd == "autostart":
            set_autostart(not autostart_enabled())
        elif cmd == "resetpos":
            self.reset_position()
        elif cmd == "quit":
            self.tray.stop()
            self.root.destroy()
            os._exit(0)

    # ------------------------------------------------------------ view model
    def build_model(self, lim, stats):
        t = self.t
        rows = []
        if lim["windows"]:
            order = ["five_hour", "seven_day"] + sorted(
                k for k in lim["windows"] if k not in ("five_hour", "seven_day"))
            for key in order:
                w = lim["windows"].get(key)
                if not w:
                    continue
                r = w["resets"]
                left = (r - now_utc()).total_seconds() if r else None
                rows.append(dict(
                    key=key, title=w["title"], sub=w["sub"], short=w["short"], pct=w["pct"],
                    pct_i=int(round(w["pct"])), color=level_color(w["pct"], t),
                    reset_short=(fmt_dur(left) if key == "five_hour" else fmt_short_day(r)) if r else "",
                    reset_card=(fmt_dur(left) if key == "five_hour" else fmt_day_time(r)) if r else "",
                ))
        # forecast
        fc_text, fc_color, eta = None, t["sub"], None
        five = lim["windows"].get("five_hour")
        if five and lim["status"] in ("ok", "error"):
            eta = self.forecast.eta_seconds(five)
            left = (five["resets"] - now_utc()).total_seconds() if five["resets"] else None
            if five["pct"] >= 100:
                fc_text, fc_color = "Limit reached", RED
            elif eta is None:
                fc_text = "Pace forecast after a few minutes of use"
            elif eta == float("inf"):
                fc_text = "Steady \u00b7 not using the limit right now"
            elif left is not None and eta < left:
                fc_text = f"At this pace: limit in ~{fmt_dur(eta)}"
                fc_color = RED if eta < 1800 else AMBER
            else:
                fc_text = "At this pace: you'll stay under until reset"
                fc_color = t["green"]
        if lim["updated"]:
            ago = time.time() - lim["updated"]
            upd = "Updated just now" if ago < 60 else f"Updated {int(ago // 60)}m ago"
        else:
            upd = "Not updated yet"
        interval = 2 if self.demo else self.cfg["limits_refresh_seconds"]
        return dict(status=lim["status"], error=lim["error"], plan=lim["plan"], rows=rows,
                    forecast=fc_text, forecast_color=fc_color, eta=eta, stats=stats,
                    updated=upd, interval=f"{interval}s" if interval < 120 else f"{interval // 60} min",
                    view=self.state.get("view", "bars"), theme=self.light)

    # ------------------------------------------------------------ alerts
    def check_alerts(self, lim, model):
        if lim["status"] != "ok":
            return
        st = self.state.setdefault("alerts", {})
        last = self.state.setdefault("last_windows", {})
        changed = False
        now = now_utc()
        names = {"five_hour": "5-hour limit", "seven_day": "Weekly limit"}
        for key, name in names.items():
            w = lim["windows"].get(key)
            if not w:
                continue
            rkey = f"{key}|{reset_key(w['resets'])}"
            prev = last.get(key)
            if prev and prev.get("rkey") != rkey and self.cfg["alert_on_reset"]:
                pr = parse_iso(prev.get("resets"))
                if pr and pr <= now and prev.get("pct", 0) >= 1:
                    self.tray.notify(f"Claude {name} reset",
                                     f"You're back to {w['pct']:.0f}%.")
            if not prev or prev.get("rkey") != rkey or abs(prev.get("pct", 0) - w["pct"]) >= 0.5:
                last[key] = {"rkey": rkey, "pct": w["pct"],
                             "resets": w["resets"].isoformat() if w["resets"] else None}
                changed = True
            fired = st.setdefault(rkey, [])
            for th in sorted(self.cfg["alert_thresholds"]):
                if w["pct"] >= th and th not in fired:
                    fired.append(th)
                    changed = True
                    when = f" Resets {fmt_day_time(w['resets'])}." if w["resets"] else ""
                    self.tray.notify(f"Claude {name}: {w['pct']:.0f}% used",
                                     f"You've crossed {th}% of your {name.lower()}.{when}")
            if w["resets"] and w["resets"] <= now:
                self.limits.refresh_now()
            if key == "five_hour" and self.cfg["alert_on_forecast"]:
                eta = model["eta"]
                left = (w["resets"] - now).total_seconds() if w["resets"] else None
                if eta and eta != float("inf") and eta < 1800 and left and eta < left \
                        and w["pct"] < 90 and self.forecast_alerted != rkey:
                    self.forecast_alerted = rkey
                    self.tray.notify("Claude: limit ahead",
                                     f"At this pace you'll hit the 5-hour limit in ~{fmt_dur(eta)}.")
        while len(st) > 40:
            del st[next(iter(st))]
        if changed:
            self.save_state()

    # ------------------------------------------------------------ pill
    def render_pill(self, m):
        key = ("pill", repr([(r["pct_i"], r["color"], r["reset_short"]) for r in m["rows"][:2]]),
               m["status"], m["theme"], m["error"],
               m["stats"]["today"] // 1000 if m["stats"] else None)
        if key == self.pill_key:
            return
        self.pill_key = key
        t = self.t
        P = Painter(self.PILL_W, self.PILL_H, self.sc, t["bg"])
        if m["rows"]:
            for (y, r) in zip((19, 36), m["rows"][:2]):
                P.text(12, y + 4, r["short"], "sb", 12, t["fg"])
                P.bar(38, y - 1, 116, 5, r["pct"], r["color"], t["track"])
                P.text(158, y + 4.5, f"{r['pct_i']}%", "b", 12.5,
                       r["color"] if r["pct"] >= 75 else t["fg"], anchor="rs")
                P.text(self.PILL_W - 12, y + 4, r["reset_short"], "r", 11.5, t["sub"], anchor="rs")
            if m["status"] == "error":
                P.circle(self.PILL_W - 6, 6, 2.5, AMBER)
        else:
            msg = {"loading": ("Claude usage", "Loading\u2026"),
                   "signin": ("Sign in to see limits", "Run  claude  \u2192  /login"),
                   "expired": ("Sign-in expired", "Run  claude  \u2192  /login"),
                   "error": ("Claude usage", m["error"] or "Can't reach Anthropic")}[m["status"]]
            P.icon_logo(20, 25)
            P.text(36, 22, msg[0], "sb", 12.5, t["fg"])
            sub = msg[1]
            if m["stats"] and m["status"] in ("signin", "expired"):
                sub += f"  \u00b7  {fmt_tokens(m['stats']['today'])} today"
            P.text(36, 39, sub, "r", 11.5, t["sub"])
        self._pill_img, _ = P.finish(self.PILL_H)
        self.pill.configure(image=self._pill_img)

    def update_tray(self, m):
        if m["rows"]:
            five = m["rows"][0]
            tip = f"Claude \u00b7 {five['title']} {five['pct_i']}%"
            if five["reset_short"]:
                tip += f" (resets in {five['reset_short']})"
            if len(m["rows"]) > 1:
                tip += f" \u00b7 {m['rows'][1]['title']} {m['rows'][1]['pct_i']}%"
            self.tray.update(five["pct"], five["color"], tip)
        else:
            self.tray.update(None, "#808080", "Claude usage \u2014 run `claude` then /login")

    # ------------------------------------------------------------ card
    def toggle_panel(self):
        if self.panel:
            self.close_panel()
        else:
            self.open_panel()

    def open_panel(self):
        self.panel = tk.Toplevel(self.root)
        self.panel.overrideredirect(True)
        self.panel.attributes("-topmost", True)
        self.panel.configure(bg=self.t["bg"])
        self.panel_label = tk.Label(self.panel, bd=0, highlightthickness=0, bg=self.t["bg"])
        self.panel_label.pack()
        self.panel_label.bind("<Button-1>", self.on_panel_click)
        self.panel_label.bind("<Button-3>", self.context_menu)
        self.panel.bind("<Escape>", lambda e: self.close_panel())
        self.panel_key = None
        lim, stats = self.limits.snapshot(), self.logs.snapshot()
        self.render_panel(self.build_model(lim, stats))
        self.panel.update_idletasks()
        style_window(user32.GetParent(self.panel.winfo_id()), not self.light)
        self.apply_opacity()
        self.mouse_was_down = True
        self.root.after(80, self._watch_outside_click)

    def _watch_outside_click(self):
        if not self.panel:
            return
        down = bool(user32.GetAsyncKeyState(0x01) & 0x8000) or \
            bool(user32.GetAsyncKeyState(0x02) & 0x8000)
        if down and not self.mouse_was_down:
            pt = wt.POINT()
            user32.GetCursorPos(ctypes.byref(pt))
            inside = any(
                win.winfo_rootx() <= pt.x < win.winfo_rootx() + win.winfo_width()
                and win.winfo_rooty() <= pt.y < win.winfo_rooty() + win.winfo_height()
                for win in (self.root, self.panel))
            if not inside:
                self.close_panel()
                return
        self.mouse_was_down = down
        self.root.after(80, self._watch_outside_click)

    def close_panel(self):
        if self.panel:
            self.panel.destroy()
            self.panel = None
            self.apply_opacity()

    def on_panel_click(self, e):
        x, y = e.x / self.sc, e.y / self.sc
        bx0, by0, bx1, by1 = self.toggle_rect
        if bx0 <= x <= bx1 and by0 <= y <= by1:
            self.state["view"] = "rings" if self.state.get("view", "bars") == "bars" else "bars"
            self.save_state()
            self.panel_key = None
            self.render_panel(self.build_model(self.limits.snapshot(), self.logs.snapshot()))

    def render_panel(self, m):
        key = repr({k: v for k, v in m.items() if k not in ("eta",)})
        if key == self.panel_key:
            return
        self.panel_key = key
        t = self.t
        W = self.CARD_W
        L, R = 22, W - 22
        P = Painter(W, 900, self.sc, t["bg"])

        # header
        y = 34
        P.icon_logo(L + 8, y - 5)
        P.text(L + 24, y, "Claude usage", "b", 17, t["fg"])
        tx = L + 24 + P.text_w("Claude usage", "b", 17) + 8
        if m["plan"]:
            pw = P.text_w(m["plan"], "sb", 10.5) + 12
            P.rrect(tx, y - 14, tx + pw, y + 2, 8, t["track"])
            P.text(tx + pw / 2, y - 2.5, m["plan"], "sb", 10.5, t["sub"], anchor="ms")
        self.toggle_rect = (R - 26, y - 19, R, y + 5)
        P.rrect(*self.toggle_rect, 7, t["track"])
        if m["view"] == "bars":
            P.icon_rings(R - 13, y - 7, t["sub"])
        else:
            P.icon_list(R - 13, y - 7, t["sub"])
        y += 26

        rows = m["rows"]
        if rows and m["view"] == "bars":
            for r in rows:
                y += 8
                icon = {"five_hour": P.icon_timer, "seven_day": P.icon_bars}.get(r["key"], P.icon_spark)
                icon(L + 8, y + 8, r["color"])
                P.text(L + 26, y + 14, r["title"], "b", 16, t["fg"])
                P.text(L + 26, y + 33, r["sub"], "r", 11.5, t["sub"])
                P.text(R, y + 18, f"{r['pct_i']}%", "b", 25, r["color"], anchor="rs")
                if r["reset_card"]:
                    P.text(R, y + 34, f"Reset {r['reset_card']}", "r", 11, t["sub"], anchor="rs")
                y += 46
                P.bar(L, y, R, 8, r["pct"], r["color"], t["track"])
                y += 16
                if r["key"] == "five_hour" and m["forecast"]:
                    P.text(L, y + 10, m["forecast"], "sb" if m["forecast_color"] != t["sub"] else "r",
                           11, m["forecast_color"])
                    y += 18
                y += 10
        elif rows:
            n = len(rows)
            colw = (R - L) / n
            rad = min(44, colw / 2 - 10)
            cy = y + 12 + rad
            for i, r in enumerate(rows):
                cx = L + colw * (i + 0.5)
                P.ring(cx, cy, rad, 9, r["pct"], r["color"], t["track"])
                P.text(cx, cy + 6, f"{r['pct_i']}%", "b", 19, t["fg"], anchor="ms")
                P.text(cx, cy + rad + 24, r["title"], "b", 13.5, t["fg"], anchor="ms")
                if r["reset_card"]:
                    P.text(cx, cy + rad + 41, r["reset_card"], "r", 11, t["sub"], anchor="ms")
            y = cy + rad + 52
            if m["forecast"]:
                P.text(W / 2, y + 8, m["forecast"], "sb" if m["forecast_color"] != t["sub"] else "r",
                       11, m["forecast_color"], anchor="ms")
                y += 24
        else:
            msg = {"signin": ["Not signed in yet.", "Open a terminal, run  claude  and type",
                              "/login  once \u2014 your real limits appear here."],
                   "expired": ["Your Claude Code sign-in expired.", "Run  claude  and  /login  again."],
                   "loading": ["Loading\u2026"]}.get(m["status"], [m["error"] or "Can't reach Anthropic."])
            y += 8
            for i, line in enumerate(msg):
                P.text(L, y + 12, line, "sb" if i == 0 else "r", 12.5, t["fg"] if i == 0 else t["sub"])
                y += 20
            y += 10

        # Claude Code activity
        y += 4
        P.line(L, y, R, y, t["line"])
        y += 24
        P.text(L, y, "CLAUDE CODE ACTIVITY", "sb", 10, t["sub"])
        s = m["stats"]
        if s:
            y += 12
            cells = [("Today", fmt_tokens(s["today"]), f"{s['msgs']} replies"),
                     ("7 days", fmt_tokens(s["week"]), "tokens"),
                     ("Now", f"{fmt_tokens(s['burn'])}/min",
                      f"{s['active']} active" if s["active"] else "idle")]
            cw = (R - L) / 3
            for i, (lab, val, sub) in enumerate(cells):
                x = L + i * cw
                P.text(x, y + 22, val, "b", 16, t["fg"])
                P.text(x, y + 39, f"{lab} \u00b7 {sub}", "r", 10.5, t["sub"])
            y += 54
            ch, mx = 30, max(s["daily"]) or 1
            bw = (R - L) / 7
            for i, v in enumerate(s["daily"]):
                x0, x1 = L + i * bw + 7, L + (i + 1) * bw - 7
                hgt = max(3, ch * v / mx)
                col = t["green"] if i == 6 else (t["sub"] if v else t["track"])
                P.rrect(x0, y + ch - hgt, x1, y + ch, 2, col)
                P.text((x0 + x1) / 2, y + ch + 15, s["labels"][i], "r", 10, t["sub"], anchor="ms")
            y += ch + 24
        else:
            P.text(L, y + 22, "Reading logs\u2026", "r", 11.5, t["sub"])
            y += 30

        # footer
        y += 6
        P.line(L, y, R, y, t["line"])
        y += 22
        foot = m["updated"]
        if m["status"] == "error" and rows:
            foot += f" \u00b7 {m['error']}, last values"
        P.text(L, y, foot, "r", 11, t["sub"])
        ok = m["status"] == "ok"
        iw = P.text_w(m["interval"], "r", 11)
        P.text(R, y, m["interval"], "r", 11, t["sub"], anchor="rs")
        P.circle(R - iw - 8, y - 4, 3, t["green"] if ok else AMBER)
        y += 16

        self._panel_img, _ = P.finish(y)
        self.panel_label.configure(image=self._panel_img)
        self.place_panel(self.px(W), self.px(y))

    def place_panel(self, w, h):
        gap = self.px(8)
        px, py = self.pill_pos
        pw, ph = self.px(self.PILL_W), self.px(self.PILL_H)
        _m, mon, _w = monitor_info(px + pw / 2, py + ph / 2)
        x = min(max(px + pw - w, mon.left + gap), mon.right - w - gap)
        y = py - h - gap
        if y < mon.top + gap:  # no room above -> open below
            y = min(py + ph + gap, mon.bottom - h - gap)
        self.panel.geometry(f"{w}x{h}+{int(x)}+{int(y)}")

    # ------------------------------------------------------------ menu
    def context_menu(self, event):
        m = tk.Menu(self.root, tearoff=0)
        m.add_command(label="Refresh now", command=self.limits.refresh_now)
        m.add_command(label="Switch to " + ("rings" if self.state.get("view", "bars") == "bars"
                                            else "bars") + " view",
                      command=lambda: (self.state.__setitem__(
                          "view", "rings" if self.state.get("view", "bars") == "bars" else "bars"),
                          self.save_state(), setattr(self, "panel_key", None)))
        auto = tk.BooleanVar(value=autostart_enabled())
        m.add_checkbutton(label="Start with Windows", variable=auto,
                          command=lambda: set_autostart(auto.get()))
        m.add_command(label="Reset position", command=self.reset_position)
        m.add_command(label="Open settings folder", command=lambda: os.startfile(DATA_DIR))
        m.add_separator()
        m.add_command(label="Quit", command=lambda: self.handle("quit"))
        m.tk_popup(event.x_root, event.y_root)

    def run(self):
        self.root.mainloop()


def single_instance():
    kernel32.CreateMutexW(None, False, "Local\\" + APP)
    return kernel32.GetLastError() != 183


if __name__ == "__main__":
    if not single_instance():
        sys.exit(0)
    try:
        Widget(demo="--demo" in sys.argv).run()
    except Exception:
        log.exception("crashed")
        raise
