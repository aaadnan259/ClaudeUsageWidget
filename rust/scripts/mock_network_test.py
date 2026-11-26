r"""End-to-end network tests against a local stand-in for Anthropic's server.

Run (quit the installed widget first: it holds the single-instance lock):
    python rust\scripts\mock_network_test.py rust\target\release\ClaudeUsageWidget.exe


The widget runs in real (non-demo) mode with an isolated data folder and a fake
credentials file, so nothing touches the user's real sign-in or settings.
Covers: token renewal (expired + on 401), the request body arriving via stdin, 429 and
the rate-limit pause, 403 wording, quick offline retries, the refresh throttle, and the
crash auto-restart (with its loop guard).
"""
import ctypes
import ctypes.wintypes as wt
import json
import os
import shutil
import subprocess
import sys
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

EXE = sys.argv[1]
PORT = 8765
u = ctypes.windll.user32
u.FindWindowW.restype = wt.HWND
u.PostMessageW.argtypes = [wt.HWND, wt.UINT, wt.WPARAM, wt.LPARAM]

MODE = {"usage": "ok"}
REQS = []  # (time, method, path, auth, body)
LOCK = threading.Lock()


class H(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def _send(self, code, obj):
        b = json.dumps(obj).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(b)))
        self.end_headers()
        self.wfile.write(b)

    def do_GET(self):
        with LOCK:
            REQS.append((time.time(), "GET", self.path, self.headers.get("Authorization"), None))
        m = MODE["usage"]
        if m == "ok":
            reset = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(time.time() + 3 * 3600))
            wk = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(time.time() + 4 * 86400))
            self._send(200, {"five_hour": {"utilization": 41.0, "resets_at": reset},
                             "seven_day": {"utilization": 12.0, "resets_at": wk}})
        elif m == "401-once":
            MODE["usage"] = "ok"
            self._send(401, {"error": "expired"})
        else:
            self._send(int(m), {"error": m})

    def do_POST(self):
        n = int(self.headers.get("Content-Length") or 0)
        body = self.rfile.read(n).decode()
        with LOCK:
            REQS.append((time.time(), "POST", self.path, self.headers.get("Authorization"), body))
        self._send(200, {"access_token": "at-new", "refresh_token": "rt-new", "expires_in": 3600})


srv = ThreadingHTTPServer(("127.0.0.1", PORT), H)
threading.Thread(target=srv.serve_forever, daemon=True).start()

tmp = tempfile.mkdtemp(prefix="cuw-mock-")
data = os.path.join(tmp, "data")
os.makedirs(data)
creds = os.path.join(tmp, "credentials.json")
# pre-mark autostart as handled so the test instance never touches the real Run key
json.dump({"autostart_initialized": True}, open(os.path.join(data, "state.json"), "w"))
json.dump({"limits_refresh_seconds": 30}, open(os.path.join(data, "config.json"), "w"))
env = dict(os.environ, CUW_DATA_DIR=data, CUW_CREDS_PATH=creds,
           CUW_USAGE_URL=f"http://127.0.0.1:{PORT}/usage", CUW_TOKEN_URL=f"http://127.0.0.1:{PORT}/token")
for k in ("CUW_DEMO_STILL", "CUW_DEMO_SCENARIO", "CUW_DEMO_OFFLINE", "CUW_TEST_CRASH"):
    env.pop(k, None)


def write_creds(expired):
    exp = (time.time() + (-600 if expired else 3600)) * 1000
    json.dump({"claudeAiOauth": {"accessToken": "at-old", "refreshToken": "rt-old", "expiresAt": int(exp),
                                 "subscriptionType": "pro"}, "other": "kept"}, open(creds, "w"), indent=2)


def log():
    p = os.path.join(data, "widget.log")
    return open(p, encoding="utf-8").read() if os.path.exists(p) else ""


def host():
    return u.FindWindowW("ClaudeUsageWidgetHost", None)


def reqs(since, method="GET"):
    with LOCK:
        return [r for r in REQS if r[0] >= since and r[1] == method]


def start(extra_env=None):
    e = dict(env, **(extra_env or {}))
    p = subprocess.Popen([EXE], env=e)
    time.sleep(3)
    return p


def stop(p):
    h = host()
    if h:
        u.PostMessageW(h, 0x10, 0, 0)
    try:
        p.wait(5)
    except subprocess.TimeoutExpired:
        p.kill()
    time.sleep(1)


results = []


def check(name, ok, detail=""):
    results.append((name, ok))
    print(("PASS " if ok else "FAIL ") + name + (f"  [{detail}]" if detail else ""))


# 1) an expired token is renewed; the refresh token travels in the body (stdin), and the
#    renewed tokens are saved without losing other fields
write_creds(expired=True)
t0 = time.time()
p = start()
time.sleep(3)
posts = reqs(t0, "POST")
gets = reqs(t0)
c = json.load(open(creds))
check("expired token renewed via the token endpoint", len(posts) == 1 and '"refresh_token":"rt-old"' in posts[0][4].replace(" ", ""),
      f"{len(posts)} POST")
check("usage fetched with the renewed token", any(g[3] == "Bearer at-new" for g in gets))
check("renewed tokens saved, other fields kept", c["claudeAiOauth"]["refreshToken"] == "rt-new" and c.get("other") == "kept")
check("no request-body temp file in the data folder", not any(f.startswith(".req") for f in os.listdir(data)))

# 2) refresh throttle: five refresh requests in 2 s -> at most one extra poll, >= 20 s apart
time.sleep(1)
t1 = time.time()
for _ in range(5):
    u.PostMessageW(host(), 0x8004, 0, 0)
    time.sleep(0.4)
time.sleep(4)
burst = reqs(t1)
check("refresh spam doesn't burst (at most 1 poll)", len(burst) <= 1, f"{len(burst)} polls")

# 3) 429: rate-limited status, then early refreshes are ignored during the pause
MODE["usage"] = "429"
time.sleep(21)
u.PostMessageW(host(), 0x8004, 0, 0)  # wait out MIN_GAP, then poll once -> 429
time.sleep(4)
t2 = time.time()
check("429 reported as rate-limited", "usage HTTP 429" in log())
for _ in range(3):
    u.PostMessageW(host(), 0x8004, 0, 0)
    time.sleep(1)
time.sleep(25)
check("no polls while rate-limited, even when asked", len(reqs(t2)) == 0, f"{len(reqs(t2))} polls")
stop(p)

# 4) 403 isn't called an expired sign-in
MODE["usage"] = "403"
write_creds(expired=False)
p = start()
time.sleep(2)
lg = log()
check("403 logged", "usage HTTP 403" in lg)
stop(p)

# 5) 401 -> renew once, then retry with the new token
MODE["usage"] = "401-once"
write_creds(expired=False)
t5 = time.time()
p = start()
time.sleep(3)
check("401 triggers one renewal and a retry", len(reqs(t5, "POST")) == 1 and any(g[3] == "Bearer at-new" for g in reqs(t5)))
stop(p)

# 6) offline: quick retries (about 10 s, then 20 s), not a full interval
MODE["usage"] = "ok"
srv.shutdown()
srv.server_close()
n0 = log().count("usage fetch failed")
p = start()
time.sleep(36)
fails = log().count("usage fetch failed") - n0
check("offline retries quickly (3 tries in ~35 s)", fails >= 3, f"{fails} tries")
stop(p)

# 7) crash on the UI thread -> one automatic restart, and no restart loop
n0 = log().count("restarting after the crash")
p = start({"CUW_TEST_CRASH": "1"})
time.sleep(20)
lg = log()
check("UI-thread crash restarts the widget", lg.count("restarting after the crash") - n0 == 1)
check("a second crash within 2 min doesn't restart again", "crashed twice within 2 minutes: not restarting" in lg)
subprocess.run(["taskkill", "/F", "/IM", "ClaudeUsageWidget.exe"], capture_output=True)

print(f"\n{sum(ok for _, ok in results)}/{len(results)} passed; test files in {tmp}")
shutil.rmtree(tmp, ignore_errors=True)
