"""The widget: two WebView2 windows (pill + card), the update loop, alerts and glue."""
import datetime as dt
import json
import math
import os
import threading
import time

import webview

from . import winutil as W
from .config import DATA_DIR, WEB_DIR, Store, log
from .data import FIVE_HOURS, History, LimitsFetcher, LogScanner, now_utc, parse_iso, reset_key
from .tray import Tray

PILL_TITLE, CARD_TITLE = "ClaudeUsagePill", "ClaudeUsageCard"
PILL_SIZES = {"bars": (236, 52), "ring": (196, 52), "text": (214, 40)}
CARD_W = 360
WEBVIEW2_FLAGS = " ".join([
    "--allow-file-access-from-files",
    "--disable-features=ElasticOverscroll,BackForwardCache,Translate,msSmartScreenProtection",
    "--disable-gpu", "--disable-gpu-compositing",
    "--renderer-process-limit=1",
    "--disable-extensions", "--disable-background-networking",
])

COLORS = {  # used for tray / taskbar icons
    "ok": "#3DDC97", "warn": "#F5A524", "crit": "#F43F5E", "none": "#808080",
}


def level(pct):
    return "crit" if pct >= 90 else "warn" if pct >= 75 else "ok"


# --------------------------------------------------------------------------- formatting
def fmt_tokens(n):
    n = float(n)
    for div, suf in ((1e9, "B"), (1e6, "M")):
        if n >= div:
            return f"{n / div:.1f}{suf}"
    if n >= 1e4:
        return f"{n / 1e3:.0f}k"
    if n >= 1e3:
        return f"{n / 1e3:.1f}k"
    return f"{int(n)}"


def fmt_dur(secs, compact=False):
    """Countdown text: '1h 48m' (card) or '1h48m' (pill). Never shows less than 1m."""
    secs = max(0, int(secs))
    d, rem = divmod(secs, 86400)
    h, rem = divmod(rem, 3600)
    m = rem // 60
    sep = "" if compact else " "
    if d:
        return f"{d}d{sep}{h}h"
    if h:
        return f"{h}h{sep}{m}m"
    return f"{max(m, 1)}m"


def display_pct(p):
    """Whole-number % for display: round half up, but never show 100% below the limit."""
    i = int(math.floor(p + 0.5))
    return min(i, 99) if p < 100 else i


def fmt_eta(secs):
    """Deliberately coarse time-to-limit, so the estimate doesn't jitter between polls."""
    m = max(0.0, secs) / 60
    if m < 10:
        return "under 10 min"
    r5 = int(5 * round(m / 5))
    if r5 < 60:
        return f"~{r5} min"
    step = 15 if m < 180 else 30
    m = step * round(m / step)
    h, mm = divmod(int(m), 60)
    return f"~{h}h" if not mm else f"~{h}h {mm}m"


def fmt_age(secs):
    secs = max(0, int(secs))
    if secs < 60:
        return "just now"
    if secs < 3600:
        return f"{secs // 60}m ago"
    h, m = divmod(secs // 60, 60)
    return f"{h}h {m}m ago" if h < 24 else f"{h // 24}d ago"


def freshness(status, error, updated, has_rows, interval, now=None):
    """One rule, shared by pill, card, tray and taskbar.
    fresh = last fetch succeeded recently; stale = showing last-known values; none = no values."""
    now = now or time.time()
    if not has_rows:
        return {"state": "none", "label": "", "reason": ""}
    age = now - updated if updated else None
    age_txt = f"Updated {fmt_age(age)}" if age is not None else "Not updated yet"
    if status == "ok" and age is not None and age <= 2 * interval + 60:
        return {"state": "fresh", "label": age_txt, "reason": ""}
    reason = {"expired": "Sign-in expired", "signin": "Not signed in"}.get(status) or error \
        or "Not updated recently"
    if reason == "Starting":
        reason = "Connecting"
    return {"state": "stale", "label": f"Last {age_txt.lower()}" if age is not None else age_txt,
            "reason": reason}


def forecast_verdict(pct, eta_now, left):
    """Pace verdict for the 5-hour window. eta_now: seconds from now to 100% (None = not
    enough data, inf = flat/declining). left: seconds until the window resets.
    A +/-10% band around the reset avoids flip-flopping on small slope changes."""
    if pct >= 100:
        return "reached"
    if eta_now is None:
        return "unknown"
    if eta_now == float("inf"):
        return "steady"
    if left is None or left <= 0:
        return "unknown"
    ratio = eta_now / left
    if ratio < 0.9:
        return "hit"
    if ratio <= 1.1:
        return "around_reset"
    return "under"


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


# --------------------------------------------------------------------------- JS API
class Api:
    """Methods callable from the pages as pywebview.api.<name>(...)."""

    def __init__(self, app):
        self._app = app

    def pill_down(self):
        self._app.begin_drag()

    def pill_menu(self):
        self._app.show_menu()

    def toggle_card(self):
        self._app.toggle_card()

    def close_card(self):
        self._app.hide_card()

    def card_height(self, h):
        self._app.on_card_height(int(h))

    def set_view(self, view):
        self._app.store.state["view"] = "bars" if view == "bars" else "rings"
        self._app.store.save_state()
        self._app.push(force=True)

    def save_settings(self, changes):
        self._app.save_settings(changes)

    def refresh(self):
        self._app.limits.refresh_now()

    def open_data_folder(self):
        os.startfile(DATA_DIR)

    def quit(self):
        self._app.quit()


# --------------------------------------------------------------------------- app
class App:
    def __init__(self, entry_path, demo=False):
        self.entry, self.demo = entry_path, demo
        self.store = Store()
        if not demo and not self.store.state.get("autostart_initialized"):
            try:
                W.set_autostart(True, entry_path)
            except OSError as e:
                log.warning("autostart: %r", e)
            self.store.state["autostart_initialized"] = True
            self.store.save_state()
        self.history = History(demo)
        self.limits = LimitsFetcher(self.store, self.history, demo)
        self.logs = LogScanner(self.store)
        self.light = self.want_light()
        self.api = Api(self)
        self.pill_hwnd = self.card_hwnd = None
        self.card_open = False
        self.card_h = 600
        self.dragging = False
        self.hidden_fs = False
        self.reset_refreshed = set()
        self.last_pill_bd = 0
        self.last_pill_url = None
        self.last_payload = {}
        self.ready = threading.Event()

    # ------------------------------------------------------------ startup
    def run(self):
        # Two tiny pages don't need GPU acceleration; software rendering plus a single
        # shared renderer cuts the WebView2 footprint by ~40% (285 -> ~175 MB private).
        os.environ.setdefault("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS", WEBVIEW2_FLAGS)
        self.limits.start()
        self.logs.start()
        style = self.store.cfg["pill_style"]
        pw, ph = PILL_SIZES.get(style, PILL_SIZES["bars"])
        bg = "#F3F4F7" if self.light else "#1B1C22"
        self.pill = webview.create_window(
            PILL_TITLE, url=os.path.join(WEB_DIR, "pill.html"), js_api=self.api,
            width=pw, height=ph, min_size=(100, 30), frameless=True, on_top=True, resizable=False,
            background_color=bg, focus=False, hidden=True)
        self.card = webview.create_window(
            CARD_TITLE, url=os.path.join(WEB_DIR, "card.html"), js_api=self.api,
            width=CARD_W, height=600, min_size=(200, 150), frameless=True, on_top=True, resizable=False,
            background_color=bg, hidden=True)
        webview.start(self._after_start, gui="edgechromium", private_mode=True,
                      storage_path=os.path.join(DATA_DIR, "webview"))

    def _after_start(self):
        for _ in range(100):
            self.pill_hwnd = W.find_window(PILL_TITLE)
            self.card_hwnd = W.find_window(CARD_TITLE)
            if self.pill_hwnd and self.card_hwnd:
                break
            time.sleep(0.1)
        self.scale = W.dpi_scale(self.pill_hwnd)
        W.style_popup(self.pill_hwnd, not self.light)
        W.style_popup(self.card_hwnd, not self.light)
        self.host = W.NativeHost(self.store.cfg["taskbar_button"], self.toggle_card, self.quit,
                                 on_show=self.show_card)
        self.host.start()
        self.tray = Tray([
            ("Refresh now", None, self.limits.refresh_now),
            ("Start with Windows", W.autostart_enabled, self.toggle_autostart),
            ("Reset position", None, self.reset_position),
            ("Open data folder", None, lambda: os.startfile(DATA_DIR)),
            None,
            ("Quit", None, self.quit),
        ], self.toggle_card)
        time.sleep(0.8)  # let the pages load
        self.place_pill()
        self.pill.show()
        self.place_pill()
        self.refresh_pill_backdrop(force=True)
        self.ready.set()
        threading.Thread(target=self.loop, daemon=True).start()

    def want_light(self):
        t = self.store.cfg.get("theme", "auto")
        return W.windows_uses_light_theme() if t == "auto" else t == "light"

    def px(self, v):
        return int(round(v * self.scale))

    # ------------------------------------------------------------ pill placement / drag
    def pill_size(self):
        w, h = PILL_SIZES.get(self.store.cfg["pill_style"], PILL_SIZES["bars"])
        return self.px(w), self.px(h)

    def place_pill(self):
        w, h = self.pill_size()
        pos = self.store.state.get("pill_pos")
        if pos:
            vx0, vy0, vx1, vy1 = W.virtual_screen()
            x = min(max(pos[0], vx0), vx1 - w)
            y = min(max(pos[1], vy0), vy1 - h)
            _m, mon, _w = W.monitor_info(x + w / 2, y + h / 2)
            x = min(max(x, mon[0]), mon[2] - w)
            y = min(max(y, mon[1]), mon[3] - h)
        else:
            _l, _t, r, b = W.primary_work_area()
            m = self.px(self.store.cfg["margin_px"])
            x, y = r - w - m, b - h - m
        self.pill_pos = (int(x), int(y))
        W.place(self.pill_hwnd, x, y, w, h)

    def begin_drag(self):
        if self.dragging:
            return
        self.dragging = True
        threading.Thread(target=self._drag_loop, daemon=True).start()

    def _drag_loop(self):
        try:
            sx, sy = W.cursor_pos()
            x0, y0 = self.pill_pos
            moved = False
            while W.mouse_down():
                cx, cy = W.cursor_pos()
                dx, dy = cx - sx, cy - sy
                if not moved and abs(dx) + abs(dy) > self.px(5):
                    moved = True
                    self.hide_card()
                if moved:
                    self.pill_pos = (x0 + dx, y0 + dy)
                    W.place(self.pill_hwnd, x0 + dx, y0 + dy)
                time.sleep(0.008)
            if moved:
                self.store.state["pill_pos"] = list(self.pill_pos)
                self.store.save_state()
                self.refresh_pill_backdrop(force=True)
            else:
                self.toggle_card()
        finally:
            self.dragging = False

    def reset_position(self):
        self.store.state.pop("pill_pos", None)
        self.store.save_state()
        self.place_pill()
        self.refresh_pill_backdrop(force=True)

    # ------------------------------------------------------------ glass backdrops
    def refresh_pill_backdrop(self, force=False):
        if not force and time.time() - self.last_pill_bd < 6:
            return
        self.last_pill_bd = time.time()
        w, h = self.pill_size()
        x, y = self.pill_pos
        url = W.capture_backdrop((x, y, x + w, y + h), self.light, exclude_hwnd=self.pill_hwnd)
        if url and (force or url != self.last_pill_url):  # skip repaint if nothing changed behind
            self.last_pill_url = url
            self.js(self.pill, f"setBackdrop({json.dumps(url)})")

    def card_rect(self):
        w, h = self.px(CARD_W), self.px(self.card_h)
        gap = self.px(10)
        px, py = self.pill_pos
        pw, ph = self.pill_size()
        _m, mon, _w = W.monitor_info(px + pw / 2, py + ph / 2)
        x = min(max(px + pw - w, mon[0] + gap), mon[2] - w - gap)
        y = py - h - gap
        if y < mon[1] + gap:
            y = min(py + ph + gap, mon[3] - h - gap)
        return int(x), int(y), int(x + w), int(y + h)

    # ------------------------------------------------------------ card
    def toggle_card(self):
        if self.card_open:
            self.hide_card()
        else:
            self.show_card()

    def show_card(self):
        if self.card_open or not self.ready.is_set():
            return
        self.card_open = True
        rect = self.card_rect()
        url = W.capture_backdrop(rect, self.light)
        self.push(force=True)
        if url:
            self.js(self.card, f"setBackdrop({json.dumps(url)})")
        W.place(self.card_hwnd, rect[0], rect[1], rect[2] - rect[0], rect[3] - rect[1])
        self.card.show()
        W.place(self.card_hwnd, rect[0], rect[1], rect[2] - rect[0], rect[3] - rect[1])
        try:
            W.user32.SetForegroundWindow(self.card_hwnd)
        except Exception:
            pass
        self.js(self.card, "opened()")
        threading.Thread(target=self._watch_outside, daemon=True).start()

    def hide_card(self):
        if not self.card_open:
            return
        self.card_open = False
        self.js(self.card, "closed()")
        self.card.hide()

    def on_card_height(self, h):
        h = max(200, min(h, 900))
        if abs(h - self.card_h) < 2:
            return
        self.card_h = h
        if self.card_open:
            rect = self.card_rect()
            W.place(self.card_hwnd, rect[0], rect[1], rect[2] - rect[0], rect[3] - rect[1])
            url = W.capture_backdrop(rect, self.light, exclude_hwnd=self.card_hwnd)
            if url:
                self.js(self.card, f"setBackdrop({json.dumps(url)})")

    def _watch_outside(self):
        was_down = True
        while self.card_open:
            down = W.mouse_down(right_too=True)
            if down and not was_down:
                x, y = W.cursor_pos()
                inside = False
                for hwnd in (self.card_hwnd, self.pill_hwnd):
                    l, t, r, b = W.window_rect(hwnd)
                    if l <= x < r and t <= y < b:
                        inside = True
                if not inside:
                    self.hide_card()
                    return
            was_down = down
            time.sleep(0.05)

    # ------------------------------------------------------------ menu / settings
    def show_menu(self):
        style = self.store.cfg["pill_style"]
        self.host.popup_menu([
            ("Show details", False, self.toggle_card),
            ("Refresh now", False, self.limits.refresh_now),
            None,
            ("Pill: bars", style == "bars", lambda: self.save_settings({"pill_style": "bars"})),
            ("Pill: ring", style == "ring", lambda: self.save_settings({"pill_style": "ring"})),
            ("Pill: text", style == "text", lambda: self.save_settings({"pill_style": "text"})),
            None,
            ("Start with Windows", W.autostart_enabled(), self.toggle_autostart),
            ("Reset position", False, self.reset_position),
            ("Open data folder", False, lambda: os.startfile(DATA_DIR)),
            None,
            ("Quit", False, self.quit),
        ])

    def toggle_autostart(self):
        W.set_autostart(not W.autostart_enabled(), self.entry)
        self.push(force=True)

    def save_settings(self, changes):
        changes = dict(changes or {})
        if "autostart" in changes:
            W.set_autostart(bool(changes.pop("autostart")), self.entry)
        old_style = self.store.cfg["pill_style"]
        self.store.update_cfg(changes)
        if self.store.cfg["pill_style"] != old_style:
            self.place_pill()
            self.refresh_pill_backdrop(force=True)
        if "limits_refresh_seconds" in changes:
            self.limits.refresh_now()
        self.push(force=True)

    def quit(self):
        log.info("quit")
        try:
            self.tray.stop()
        except Exception:
            pass
        os._exit(0)

    # ------------------------------------------------------------ JS bridge
    def js(self, win, code):
        try:
            win.evaluate_js(code)
        except Exception as e:
            log.debug("js: %r", e)

    def push(self, force=False):
        try:
            model = self.build_model()
        except Exception:
            log.exception("model")
            return
        # The pill only needs a few fields; sending it the whole card model would
        # repaint it every second or two (the timeline's "now" keeps moving).
        pill_model = {k: model[k] for k in ("theme", "status", "error", "rows", "settings", "fresh")}
        if not model["rows"] and model["stats"]:
            pill_model["stats"] = {"today": model["stats"]["today"]}
        pill_payload = json.dumps(pill_model)
        if force or self.last_payload.get("pill") != pill_payload:
            self.last_payload["pill"] = pill_payload
            self.js(self.pill, f"update({pill_payload})")
        payload = json.dumps(model)
        if self.card_open and (force or self.last_payload.get("card") != payload):
            self.last_payload["card"] = payload
            self.js(self.card, f"update({payload})")
        self.update_icons(model)
        return model

    # ------------------------------------------------------------ main loop
    def watchdog(self):
        """Replace a stuck limits thread, logging every thread's stack first."""
        if not self.limits.overdue():
            return
        import sys
        import traceback
        names = {t.ident: t.name for t in threading.enumerate()}
        dump = "\n".join(f"--- {names.get(tid, tid)}\n" + "".join(traceback.format_stack(fr))
                         for tid, fr in sys._current_frames().items())
        log.warning("limits thread stuck; restarting it. Stacks:\n%s", dump)
        old = self.limits
        old.stopped = True
        self.limits = LimitsFetcher(self.store, self.history, self.demo)
        with old.lock:
            if old.windows:
                self.limits.windows, self.limits.plan = old.windows, old.plan
                self.limits.status, self.limits.error = "error", "Reconnecting"
                self.limits.updated = old.updated
        self.limits.start()

    def loop(self):
        tick = 0
        last = time.time()
        while True:
            try:
                now = time.time()
                if now - last > 30:  # woke from sleep: fresh numbers + backdrop now
                    log.info("resumed after %.0fs", now - last)
                    self.limits.heartbeat = now  # time asleep isn't a stuck thread
                    self.limits.refresh_now()
                    self.last_pill_bd = 0
                last = now
                self.watchdog()
                light = self.want_light()
                if light != self.light:
                    self.light = light
                    W.style_popup(self.pill_hwnd, not light)
                    W.style_popup(self.card_hwnd, not light)
                    self.refresh_pill_backdrop(force=True)
                model = self.push()
                if model:
                    self.check_alerts(model)
                self.fullscreen_check()
                if not self.hidden_fs and not self.dragging:
                    W.set_topmost(self.pill_hwnd)
                    if not self.card_open:
                        self.refresh_pill_backdrop()
                tick += 1
                if tick % 15 == 0 and not self.dragging:
                    self.place_pill()  # follows taskbar / resolution changes
            except Exception:
                log.exception("loop")
            time.sleep(1)

    def fullscreen_check(self):
        if not self.store.cfg["hide_in_fullscreen"]:
            if self.hidden_fs:
                self.pill.show()
                self.hidden_fs = False
            return
        w, h = self.pill_size()
        mon = W.monitor_info(self.pill_pos[0] + w / 2, self.pill_pos[1] + h / 2)[0]
        fs = W.foreground_is_fullscreen({self.pill_hwnd, self.card_hwnd}, mon)
        if fs and not self.hidden_fs:
            self.hide_card()
            self.pill.hide()
            self.hidden_fs = True
        elif not fs and self.hidden_fs:
            self.pill.show()
            self.place_pill()
            self.hidden_fs = False

    # ------------------------------------------------------------ view model
    def build_model(self):
        lim, stats = self.limits.snapshot(), self.logs.snapshot()
        now = now_utc()
        nowt = time.time()
        interval = 3 if self.demo else self.store.cfg["limits_refresh_seconds"]
        updated = lim["updated"]
        rows = []
        order = ["five_hour", "seven_day"] + sorted(
            k for k in lim["windows"] if k not in ("five_hour", "seven_day"))
        for key in order:
            w = lim["windows"].get(key)
            if not w:
                continue
            r = w["resets"]
            left = (r - now).total_seconds() if r else None
            # Data fetched before this window's reset no longer describes the current window.
            pending = bool(r and left <= 0 and (not updated or updated < r.timestamp()))
            if pending:
                reset_short, reset_card = "updating", "Reset · updating"
            elif key == "five_hour":
                reset_short = fmt_dur(left, compact=True) if r else ""
                reset_card = ("Resets in " + fmt_dur(left)) if r else ""
            else:
                reset_short = fmt_short_day(r) if r else ""
                reset_card = ("Resets " + fmt_day_time(r)) if r else ""
            prev_peak = None
            if key == "seven_day" and r and not pending:
                pk = self.history.prev_window_peak(key, r)
                prev_peak = None if pk is None else display_pct(pk)
            rows.append(dict(
                key=key, title=w["title"], sub=w["sub"], short=w["short"], prev_peak=prev_peak,
                pct=0.0 if pending else round(w["pct"], 1),
                pct_i=None if pending else display_pct(w["pct"]),
                level="ok" if pending else level(w["pct"]), pending=pending,
                reset_short=reset_short, reset_card=reset_card))

        fresh = freshness(lim["status"], lim["error"], updated, bool(rows), interval, nowt)
        forecast, timeline, eta_now, verdict = None, None, None, None
        five = lim["windows"].get("five_hour")
        five_row = next((r for r in rows if r["key"] == "five_hour"), None)
        if five and five_row and not five_row["pending"]:
            eta = self.history.eta_seconds("five_hour", five)
            age = nowt - updated if updated else 0
            if eta is not None and eta != float("inf"):
                eta_now = eta - age  # the estimate is measured from the last sample
            else:
                eta_now = eta
            left = (five["resets"] - now).total_seconds() if five["resets"] else None
            if fresh["state"] == "stale":
                verdict = "stale"
                forecast = {"text": "Forecast paused until data refreshes", "level": "muted"}
            else:
                verdict = forecast_verdict(five["pct"], eta_now, left)
                if verdict == "reached":
                    forecast = {"text": "Limit reached · resets in " + fmt_dur(left or 0), "level": "crit"}
                elif verdict == "hit":  # only here is eta_now a finite number
                    forecast = {"text": f"At this pace, limit in {fmt_eta(eta_now)}",
                                "level": "crit" if eta_now < 1800 else "warn"}
                else:
                    forecast = {
                        "unknown": {"text": "Not enough recent activity to estimate", "level": "muted"},
                        "steady": {"text": "Steady · no current pace toward the limit", "level": "muted"},
                        "around_reset": {"text": "At this pace, you'll reach the limit around reset time",
                                         "level": "warn"},
                        "under": {"text": "At this pace, you'll stay under the limit until reset",
                                  "level": "ok"},
                    }[verdict]
            if five["resets"]:
                timeline = self.build_timeline(five, None if verdict == "stale" else eta, updated or nowt)

        st = None
        if stats:
            st = dict(today=fmt_tokens(stats["today"]), msgs=stats["msgs"],
                      week=fmt_tokens(stats["week"]), burn=fmt_tokens(stats["burn"]),
                      active=stats["active"], heat=stats["heat"], day_labels=stats["day_labels"],
                      weeks=[dict(w, total=fmt_tokens(w["total"]), empty=not w["total"])
                             for w in stats.get("weeks", [])])
        settings = dict(self.store.cfg, autostart=W.autostart_enabled())
        return dict(theme="light" if self.light else "dark", status=lim["status"],
                    error=lim["error"], plan=lim["plan"], rows=rows, forecast=forecast,
                    timeline=timeline, stats=st, fresh=fresh, updated=fresh["label"],
                    interval=f"{interval}s" if interval < 120 else f"{interval // 60} min",
                    view=self.store.state.get("view", "rings"), settings=settings,
                    verdict=verdict,
                    eta=None if eta_now in (None, float("inf")) else round(eta_now))

    def build_timeline(self, five, eta, updated):
        """Chart for the current 5-hour window. Actual data ends at the last fetch (not at
        'now'); the projection starts there and uses the same estimate as the forecast text,
        so where it meets 100% matches the displayed time-to-limit. Never extends past reset."""
        end = five["resets"].timestamp()
        start = end - FIVE_HOURS
        span = FIVE_HOURS
        clamp = lambda x: round(min(max(x, 0.0), 1.0), 4)
        pts = [(0.0, 0.0)]
        for t, p in self.history.series("five_hour", five["resets"]):
            if start <= t < updated:
                pts.append((clamp((t - start) / span), p))
        last_x = clamp((updated - start) / span)
        pts.append((last_x, five["pct"]))
        pts.sort()
        now_x = clamp((time.time() - start) / span)
        proj, hit = None, False
        if eta is not None and five["pct"] < 100:
            if eta == float("inf"):
                proj = [1.0, five["pct"]]
            else:
                hit_x = last_x + eta / span
                if hit_x <= 1:
                    proj, hit = [round(hit_x, 4), 100.0], True
                else:
                    slope = (100 - five["pct"]) / (hit_x - last_x)
                    proj = [1.0, round(five["pct"] + slope * (1 - last_x), 1)]
        st = dt.datetime.fromtimestamp(start, dt.timezone.utc)
        return dict(points=pts, last_x=last_x, now_x=now_x, now_pct=five["pct"], proj=proj,
                    hit=hit, start_label=fmt_clock(st), end_label=fmt_clock(five["resets"]),
                    level=level(five["pct"]))

    # ------------------------------------------------------------ tray / taskbar
    def update_icons(self, m):
        five = m["rows"][0] if m["rows"] else None
        if five and not five["pending"]:
            stale = m["fresh"]["state"] == "stale"
            color = COLORS["none"] if stale else COLORS[five["level"]]
            tip = f"Claude · {five['title']} {five['pct_i']}%"
            if five["reset_short"]:
                tip += f" (resets in {five['reset_short']})"
            if len(m["rows"]) > 1 and m["rows"][1]["pct_i"] is not None:
                tip += f" · {m['rows'][1]['title']} {m['rows'][1]['pct_i']}%"
            if stale:
                tip += f" · {m['fresh']['reason']}, {m['fresh']['label'].lower()}"
            self.tray.update(five["pct"], color, tip)
            self.host.update(five["pct"], color, f"Claude {five['pct_i']}% · {five['reset_short']}"
                             + (" (stale)" if stale else ""))
        elif five:
            self.tray.update(None, COLORS["none"], "Claude · limit reset, updating…")
            self.host.update(None, COLORS["none"], "Claude usage · updating")
        else:
            self.tray.update(None, COLORS["none"], "Claude usage — run `claude` then /login")
            self.host.update(None, COLORS["none"], "Claude usage")

    # ------------------------------------------------------------ alerts
    def check_alerts(self, model):
        lim = self.limits.snapshot()
        if lim["status"] != "ok":
            return
        cfg, state = self.store.cfg, self.store.state
        fired_all = state.setdefault("alerts", {})
        last = state.setdefault("last_windows", {})
        changed = False
        now = now_utc()
        for key, name in (("five_hour", "5-hour limit"), ("seven_day", "Weekly limit")):
            w = lim["windows"].get(key)
            if not w:
                continue
            rkey = f"{key}|{reset_key(w['resets'])}"
            prev = last.get(key)
            if prev and prev.get("rkey") != rkey and cfg["alert_on_reset"]:
                pr = parse_iso(prev.get("resets"))
                # only a timely alert: skip resets that happened long ago (asleep / not running)
                if pr and now - dt.timedelta(minutes=30) <= pr <= now and prev.get("pct", 0) >= 1:
                    self.tray.notify(f"Claude {name} reset", f"You're back to {w['pct']:.0f}%.")
            if not prev or prev.get("rkey") != rkey or abs(prev.get("pct", 0) - w["pct"]) >= 0.5:
                last[key] = {"rkey": rkey, "pct": w["pct"],
                             "resets": w["resets"].isoformat() if w["resets"] else None}
                changed = True
            fired = fired_all.setdefault(rkey, [])
            for th in sorted(cfg["alert_thresholds"]):
                if w["pct"] >= th and th not in fired:
                    fired.append(th)
                    changed = True
                    when = f" Resets {fmt_day_time(w['resets'])}." if w["resets"] else ""
                    self.tray.notify(f"Claude {name}: {w['pct']:.0f}% used",
                                     f"You've crossed {th}% of your {name.lower()}.{when}")
            # A reset just passed: fetch the new window once (not every second while the
            # API still reports the old reset time).
            if w["resets"] and w["resets"] <= now and rkey not in self.reset_refreshed:
                self.reset_refreshed.add(rkey)
                self.limits.refresh_now()
            if key == "five_hour" and cfg["alert_on_forecast"]:
                eta = model.get("eta")
                # needs a real estimate on fresh data; saved in state so restarts don't repeat it
                if model.get("verdict") == "hit" and eta is not None and eta < 1800 \
                        and w["pct"] < 90 and "forecast" not in fired:
                    fired.append("forecast")
                    changed = True
                    self.tray.notify("Claude: limit ahead",
                                     f"At this pace you'll hit the 5-hour limit in {fmt_eta(eta)}.")
        while len(fired_all) > 40:
            del fired_all[next(iter(fired_all))]
        if changed:
            self.store.save_state()
