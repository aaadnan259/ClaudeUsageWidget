"""Data sources: plan limits (Anthropic), limit history + forecast, local Claude Code logs."""
import datetime as dt
import glob
import json
import os
import shutil
import subprocess
import threading
import time
import urllib.error
import urllib.request

from .config import CREDS_PATH, DATA_DIR, HISTORY_PATH, PROJECTS_DIR, load_json, log, save_json

USAGE_URL = "https://api.anthropic.com/api/oauth/usage"
TOKEN_URLS = ["https://console.anthropic.com/v1/oauth/token",
              "https://platform.claude.com/v1/oauth/token"]
CLIENT_ID = "9d1c250a-e61b-44d9-88ed-5944d1962f5e"  # Claude Code's public OAuth client id
USER_AGENT = "claude-usage-widget/3.0"
FIVE_HOURS = 5 * 3600


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


# --------------------------------------------------------------------------- http
class HttpError(Exception):
    def __init__(self, code):
        super().__init__(f"HTTP {code}")
        self.code = code


class UnexpectedResponse(Exception):
    """The usage endpoint answered, but not with any window we recognise."""


CURL = shutil.which("curl.exe") or os.path.join(os.environ.get("SystemRoot", r"C:\Windows"),
                                                 "System32", "curl.exe")


def http_json(url, headers, body=None):
    """GET/POST JSON via Windows' curl (Cloudflare can block Python's TLS fingerprint).
    Secrets travel via stdin / a temp file, never the command line."""
    headers = dict(headers, **{"User-Agent": USER_AGENT, "Accept": "application/json"})
    if os.path.exists(CURL):
        args = [CURL, "-sS", "--max-time", "20", "-H", "@-", "-w", "\n%{http_code}"]
        tmp = None
        if body is not None:
            tmp = os.path.join(DATA_DIR, f".req-{os.getpid()}-{threading.get_ident()}-{time.time_ns()}.json")
            with open(tmp, "w", encoding="utf-8") as f:
                f.write(body)
            args += ["-X", "POST", "--data-binary", "@" + tmp]
            headers["Content-Type"] = "application/json"
        args.append(url)
        try:
            r = subprocess.run(args, input="\n".join(f"{k}: {v}" for k, v in headers.items()),
                               capture_output=True, text=True, timeout=30,
                               creationflags=0x08000000)
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


# --------------------------------------------------------------------------- plan limits
LIMIT_META = {
    "five_hour": ("Session", "5h sliding window", "5h"),
    "seven_day": ("Weekly", "All models", "Wk"),
}


class LimitsFetcher(threading.Thread):
    def __init__(self, store, history, demo=False):
        super().__init__(daemon=True)
        self.store, self.history, self.demo = store, history, demo
        self.lock = threading.Lock()
        self.wake = threading.Event()
        self.windows, self.plan = {}, None
        self.status, self.error, self.updated = "loading", "", None
        self.backoff = 0
        self.t0 = time.time()
        self.heartbeat = time.time()
        self.stopped = False
        if not demo:
            cached = history.last_windows()
            if cached:
                self.windows, self.status, self.error = cached, "error", "Starting"
                self.updated = history.samples[-1]["t"]

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

    def _parse(self, resp):
        out = {}
        if not isinstance(resp, dict):
            return out
        for key, val in resp.items():
            if not isinstance(val, dict) or val.get("utilization") is None:
                continue
            if key in LIMIT_META:
                title, sub, short = LIMIT_META[key]
            elif key.startswith("seven_day_") and self.store.cfg["show_model_limits"]:
                name = key[len("seven_day_"):].replace("_", " ").title()
                title, sub, short = name, "Dedicated weekly limit", name[:2]
            else:
                continue
            out[key] = {"title": title, "sub": sub, "short": short,
                        "pct": float(val["utilization"]), "resets": parse_iso(val.get("resets_at"))}
        return out

    def _demo(self):
        base = dt.datetime.fromtimestamp(self.t0, dt.timezone.utc)
        el = time.time() - self.t0
        return {
            "five_hour": {"title": "Session", "sub": "5h sliding window", "short": "5h",
                          "pct": min(100.0, 52 + el / 10),
                          "resets": base + dt.timedelta(hours=2, minutes=50)},
            "seven_day": {"title": "Weekly", "sub": "All models", "short": "Wk", "pct": 22.0,
                          "resets": base + dt.timedelta(days=4, hours=2)},
        }

    def poll_once(self):
        if self.demo:
            if os.environ.get("CUW_DEMO_OFFLINE") and time.time() - self.t0 > 25:
                with self.lock:  # demo-only: simulate losing the network to check stale UI
                    self.status, self.error = "error", "Offline"
                return
            w = self._demo()
            with self.lock:
                self.windows, self.plan, self.status = w, "Pro", "ok"
                self.updated = time.time()
            self.history.add(self.updated, w)
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
            hdr = lambda o: {"Authorization": f"Bearer {o['accessToken']}",
                             "anthropic-beta": "oauth-2025-04-20"}
            try:
                resp = http_json(USAGE_URL, hdr(oauth))
            except HttpError as e:
                if e.code == 401 and oauth.get("refreshToken"):
                    oauth = self._refresh_token(oauth)
                    resp = http_json(USAGE_URL, hdr(oauth))
                else:
                    raise
            windows = self._parse(resp)
            if not windows:
                # Never show an unrecognised response as "0%": keep the last values, flag stale.
                keys = sorted(resp)[:12] if isinstance(resp, dict) else type(resp).__name__
                raise UnexpectedResponse(f"no known usage windows (keys: {keys})")
            with self.lock:
                self.windows, self.plan, self.status, self.error = windows, plan, "ok", ""
                self.updated = time.time()
            self.history.add(self.updated, windows)
            self.backoff = 0
        except HttpError as e:
            log.warning("usage %s", e)
            with self.lock:
                if e.code in (401, 403):
                    self.status, self.error = "expired", "Sign-in expired"
                elif e.code == 429:
                    self.status, self.error = "error", "Rate-limited, retrying"
                else:
                    self.status, self.error = "error", f"HTTP {e.code}"
            if e.code == 429:
                self.backoff = min(max(self.backoff * 2, 120), 900)
        except UnexpectedResponse as e:
            log.warning("usage response: %s", e)
            with self.lock:
                self.status, self.error = "error", "Unexpected response"
        except Exception as e:
            log.warning("usage fetch failed: %r", e)
            with self.lock:
                self.status, self.error = "error", "Offline"

    def next_wait(self):
        return 3 if self.demo else max(30, self.store.cfg["limits_refresh_seconds"]) + self.backoff

    def overdue(self):
        """True if this thread has missed its schedule (stuck call)."""
        return time.time() - self.heartbeat > self.next_wait() + 150

    def run(self):
        while True:
            self.heartbeat = time.time()
            try:
                self.poll_once()
            except Exception:
                log.exception("poll crashed")
            self.heartbeat = time.time()
            # Wait in short steps against the wall clock: Windows timed waits don't
            # advance while the PC sleeps, so one long wait can overrun by hours.
            deadline = time.time() + self.next_wait()
            while time.time() < deadline and not self.stopped:
                if self.wake.wait(5):
                    break
            self.wake.clear()
            if self.stopped:
                return


# --------------------------------------------------------------------------- history + forecast
class History:
    """Persisted limit samples (history.jsonl) for the timeline and pace forecast."""
    KEEP_DAYS = 35

    def __init__(self, demo=False):
        self.lock = threading.Lock()
        self.demo = demo
        self.samples = []  # dicts: {"t": epoch, "w": {key: [pct, resets_iso]}}
        if not demo:
            cutoff = time.time() - self.KEEP_DAYS * 86400
            try:
                with open(HISTORY_PATH, encoding="utf-8") as f:
                    for ln in f:
                        try:
                            s = json.loads(ln)
                            if s.get("t", 0) >= cutoff:
                                self.samples.append(s)
                        except Exception:
                            pass
            except OSError:
                pass
            self._compact()
        else:
            self._seed_demo()

    def _seed_demo(self):
        now = time.time()
        reset = now + 2 * 3600 + 50 * 60
        for i in range(0, 130, 3):  # last ~2h10m of a rising session
            t = now - (130 - i) * 60
            pct = max(0.0, 52 * (i / 130) ** 1.3)
            self.samples.append({"t": t, "w": {"five_hour": [pct, dt.datetime.fromtimestamp(
                reset, dt.timezone.utc).isoformat()]}})

    def _compact(self):
        try:
            with open(HISTORY_PATH, "w", encoding="utf-8") as f:
                for s in self.samples:
                    f.write(json.dumps(s, separators=(",", ":")) + "\n")
        except OSError:
            pass

    def add(self, t, windows):
        s = {"t": round(t, 1), "w": {k: [round(w["pct"], 2),
                                         w["resets"].isoformat() if w["resets"] else None]
                                     for k, w in windows.items()}}
        with self.lock:
            if self.samples and self.samples[-1]["w"] == s["w"] and t - self.samples[-1]["t"] < 300:
                return  # unchanged; keep the file small
            self.samples.append(s)
        if not self.demo:
            try:
                with open(HISTORY_PATH, "a", encoding="utf-8") as f:
                    f.write(json.dumps(s, separators=(",", ":")) + "\n")
            except OSError:
                pass

    def last_windows(self):
        """Last known limits (for showing something while offline / rate-limited)."""
        with self.lock:
            if not self.samples:
                return {}
            last = self.samples[-1]["w"]
        out = {}
        for key, (pct, resets) in last.items():
            r = parse_iso(resets)
            if key not in LIMIT_META or (r and r < now_utc()):
                continue
            title, sub, short = LIMIT_META[key]
            out[key] = {"title": title, "sub": sub, "short": short, "pct": pct, "resets": r}
        return out

    def series(self, key, resets):
        """(epoch, pct) samples belonging to the window that resets at `resets`."""
        rk = reset_key(resets)
        with self.lock:
            out = []
            for s in self.samples:
                v = s["w"].get(key)
                if v and reset_key(parse_iso(v[1])) == rk:
                    out.append((s["t"], v[0]))
        return out

    def eta_seconds(self, key, w, min_span=240):
        """Seconds until 100% at the recent pace. None = not enough data, inf = not rising."""
        pts = [p for p in self.series(key, w["resets"]) if p[0] >= time.time() - 1800]
        if self.demo:
            min_span = 20
        if len(pts) < 3 or pts[-1][0] - pts[0][0] < min_span:
            return None
        n = len(pts)
        mt = sum(t for t, _ in pts) / n
        mp = sum(p for _, p in pts) / n
        var = sum((t - mt) ** 2 for t, _ in pts)
        slope = sum((t - mt) * (p - mp) for t, p in pts) / var if var else 0
        if slope <= 0.0005 / 60:
            return float("inf")
        return (100 - w["pct"]) / slope


# --------------------------------------------------------------------------- local logs
class LogScanner(threading.Thread):
    KEEP_DAYS = 8

    def __init__(self, store):
        super().__init__(daemon=True)
        self.store = store
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
        days = [(today0 - dt.timedelta(days=i)) for i in range(6, -1, -1)]
        starts = [d.timestamp() for d in days]
        daily = [0] * 7
        heat = [[0] * 24 for _ in range(7)]
        today = msgs = week = burn = 0
        active = set()
        for ts, tok, sess in self.events.values():
            if ts >= starts[0]:
                week += tok
                di = max(i for i, d in enumerate(starts) if ts >= d)
                daily[di] += tok
                heat[di][dt.datetime.fromtimestamp(ts).hour] += tok
            if ts >= starts[-1]:
                today += tok
                msgs += 1
            if ts >= now - 600:
                burn += tok
            if ts >= now - 300 and sess:
                active.add(sess)
        return dict(today=today, msgs=msgs, week=week, burn=burn / 10, active=len(active),
                    daily=daily, heat=heat, day_labels=[d.strftime("%a") for d in days])

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
            time.sleep(max(2, self.store.cfg["logs_refresh_seconds"]))
