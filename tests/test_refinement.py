"""Tests for the trust-and-clarity refinement: display rounding, freshness, forecast/chart
agreement, reset boundaries, unexpected responses, and alert de-duplication.

Run:  .venv\\Scripts\\python.exe -m unittest discover -s tests -v
"""
import datetime as dt
import logging
import os
import sys
import threading
import time
import unittest
from types import SimpleNamespace

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from cuw import data  # noqa: E402
from cuw.app import App, display_pct, fmt_eta, forecast_verdict, freshness  # noqa: E402
from cuw.config import DEFAULT_CONFIG  # noqa: E402
from cuw.data import FIVE_HOURS, History, LimitsFetcher, reset_key  # noqa: E402

UTC = dt.timezone.utc
logging.getLogger("ClaudeUsageWidget").disabled = True  # keep test noise out of the real log


def make_history(samples=()):
    h = History.__new__(History)
    h.lock, h.demo, h.samples = threading.Lock(), False, list(samples)
    return h


def sample(t, pct, resets, key="five_hour"):
    return {"t": t, "w": {key: [pct, resets.isoformat()]}}


class FakeLimits:
    def __init__(self, windows, status="ok", error="", updated=None, plan="Pro"):
        self.windows, self.status, self.error, self.plan = windows, status, error, plan
        self.updated = time.time() if updated is None else updated
        self.refreshes = 0

    def snapshot(self):
        return dict(windows=dict(self.windows), plan=self.plan, status=self.status,
                    error=self.error, updated=self.updated)

    def refresh_now(self):
        self.refreshes += 1


class FakeTray:
    def __init__(self):
        self.sent = []

    def notify(self, title, msg):
        self.sent.append(title)


def make_app(limits, history=None, state=None):
    """An App with fake collaborators: no windows, no network, no registry writes."""
    app = App.__new__(App)
    app.store = SimpleNamespace(cfg=dict(DEFAULT_CONFIG), state={} if state is None else state,
                                save_state=lambda: None)
    app.limits, app.history = limits, history or make_history()
    app.logs = SimpleNamespace(snapshot=lambda: None)
    app.light, app.demo, app.tray, app.reset_refreshed = False, False, FakeTray(), set()
    return app


def win(pct, resets, title="Session", short="5h"):
    return {"title": title, "sub": "", "short": short, "pct": pct, "resets": resets}


def at(seconds_from_now):
    return dt.datetime.fromtimestamp(time.time() + seconds_from_now, UTC)


NO_FORECAST = {"verdict": None, "eta": None}


class DisplayTests(unittest.TestCase):
    def test_display_pct_rounds_half_up_and_never_fakes_100(self):
        self.assertEqual(display_pct(56.5), 57)   # Python's round() would give 56
        self.assertEqual(display_pct(0.4), 0)
        self.assertEqual(display_pct(99.6), 99)   # not at the limit yet
        self.assertEqual(display_pct(100.0), 100)

    def test_eta_is_coarse(self):
        self.assertEqual(fmt_eta(5 * 60), "under 10 min")
        self.assertEqual(fmt_eta(23 * 60), "~25 min")
        self.assertEqual(fmt_eta(80 * 60), "~1h 15m")
        self.assertEqual(fmt_eta(120 * 60), "~2h")
        self.assertEqual(fmt_eta(200 * 60), "~3h 30m")
        self.assertEqual(fmt_eta(81 * 60), fmt_eta(79 * 60))  # small slope wobble: same text


class FreshnessTests(unittest.TestCase):
    def test_states(self):
        now = time.time()
        self.assertEqual(freshness("ok", "", now - 30, True, 120, now)["state"], "fresh")
        old = freshness("ok", "", now - 3600, True, 120, now)
        self.assertEqual((old["state"], old["reason"]), ("stale", "Not updated recently"))
        off = freshness("error", "Offline", now - 900, True, 120, now)
        self.assertEqual((off["state"], off["reason"]), ("stale", "Offline"))
        self.assertTrue(off["label"].startswith("Last updated 15m ago"))
        rl = freshness("error", "Rate-limited, retrying", now - 300, True, 120, now)
        self.assertEqual(rl["reason"], "Rate-limited, retrying")
        exp = freshness("expired", "Sign-in expired", now - 60, True, 120, now)
        self.assertEqual((exp["state"], exp["reason"]), ("stale", "Sign-in expired"))  # used to look fresh
        self.assertEqual(freshness("error", "Starting", now - 60, True, 120, now)["reason"], "Connecting")
        self.assertEqual(freshness("signin", "", None, False, 120, now)["state"], "none")


class VerdictTests(unittest.TestCase):
    def test_verdicts(self):
        self.assertEqual(forecast_verdict(100, 0, 3600), "reached")
        self.assertEqual(forecast_verdict(40, None, 3600), "unknown")
        self.assertEqual(forecast_verdict(40, float("inf"), 3600), "steady")
        self.assertEqual(forecast_verdict(40, 1800, 3600), "hit")
        self.assertEqual(forecast_verdict(40, 3600, 3600), "around_reset")
        self.assertEqual(forecast_verdict(40, 3 * 3600, 3600), "under")
        self.assertEqual(forecast_verdict(40, 600, -5), "unknown")  # window already reset


class ModelTests(unittest.TestCase):
    def test_values_trace_back_to_api(self):
        lim = FakeLimits({"five_hour": win(57.4, at(3600)),
                          "seven_day": win(36.0, at(86400 * 3), "Weekly", "Wk")})
        m = make_app(lim).build_model()
        self.assertEqual([(r["key"], r["pct_i"]) for r in m["rows"]],
                         [("five_hour", 57), ("seven_day", 36)])
        self.assertEqual(m["fresh"]["state"], "fresh")
        self.assertTrue(m["rows"][0]["reset_card"].startswith("Resets in "))

    def test_window_that_reset_since_last_fetch_is_not_shown_as_current(self):
        lim = FakeLimits({"five_hour": win(83.0, at(-60))}, updated=time.time() - 600)
        m = make_app(lim).build_model()
        row = m["rows"][0]
        self.assertTrue(row["pending"])
        self.assertIsNone(row["pct_i"])
        self.assertIsNone(m["forecast"])
        self.assertIsNone(m["timeline"])

    def test_stale_data_pauses_forecast_and_projection(self):
        now = time.time()
        resets = at(3 * 3600)
        h = make_history([sample(now - 900 - (10 - i) * 60, 30 + i, resets) for i in range(11)])
        lim = FakeLimits({"five_hour": win(40.0, resets)}, status="error", error="Offline",
                         updated=now - 900)
        m = make_app(lim, h).build_model()
        self.assertEqual(m["fresh"]["state"], "stale")
        self.assertEqual(m["verdict"], "stale")
        self.assertIn("paused", m["forecast"]["text"])
        self.assertIsNone(m["timeline"]["proj"])

    def test_forecast_text_and_chart_agree(self):
        now = time.time()
        resets = at(3 * 3600)
        # +1%/min for 10 minutes, now at 40% -> ~60 min to the limit, well before reset
        h = make_history([sample(now - (10 - i) * 60, 30 + i, resets) for i in range(11)])
        lim = FakeLimits({"five_hour": win(40.0, resets)}, updated=now)
        m = make_app(lim, h).build_model()
        self.assertEqual(m["verdict"], "hit")
        self.assertIn("~1h", m["forecast"]["text"])
        tl = m["timeline"]
        self.assertTrue(tl["hit"])
        start = resets.timestamp() - FIVE_HOURS
        self.assertAlmostEqual(start + tl["proj"][0] * FIVE_HOURS, now + m["eta"], delta=5)

    def test_not_projected_to_hit_before_reset(self):
        now = time.time()
        resets = at(30 * 60)
        h = make_history([sample(now - (10 - i) * 60, 30 + i * 0.2, resets) for i in range(11)])
        lim = FakeLimits({"five_hour": win(32.0, resets)}, updated=now)
        m = make_app(lim, h).build_model()
        self.assertEqual(m["verdict"], "under")
        self.assertIn("stay under", m["forecast"]["text"])
        self.assertFalse(m["timeline"]["hit"])
        self.assertEqual(m["timeline"]["proj"][0], 1.0)

    def test_flat_usage_has_no_exhaustion_time(self):
        now = time.time()
        resets = at(3 * 3600)
        h = make_history([sample(now - (10 - i) * 60, 40, resets) for i in range(11)])
        m = make_app(FakeLimits({"five_hour": win(40.0, resets)}, updated=now), h).build_model()
        self.assertEqual(m["verdict"], "steady")
        self.assertIsNone(m["eta"])
        self.assertFalse(m["timeline"]["hit"])

    def test_thin_history_gives_honest_no_estimate(self):
        m = make_app(FakeLimits({"five_hour": win(40.0, at(3 * 3600))})).build_model()
        self.assertEqual(m["forecast"]["text"], "Not enough recent activity to estimate")
        self.assertIsNone(m["timeline"]["proj"])


class AlertTests(unittest.TestCase):
    def test_thresholds_fire_once_per_window_and_survive_restart(self):
        state = {}
        lim = FakeLimits({"five_hour": win(80.0, at(3600)),
                          "seven_day": win(76.0, at(86400), "Weekly", "Wk")})
        app = make_app(lim, state=state)
        for _ in range(3):
            app.check_alerts(NO_FORECAST)
        self.assertEqual(len(app.tray.sent), 2)  # 75% session + 75% weekly, tracked independently
        again = make_app(lim, state=state)        # "restart" with the saved state
        again.check_alerts(NO_FORECAST)
        self.assertEqual(again.tray.sent, [])
        lim.windows["five_hour"] = win(80.0, at(3600 + 5 * 3600))  # a new 5-hour window
        again.check_alerts(NO_FORECAST)
        self.assertEqual(len(again.tray.sent), 1)

    def test_no_alerts_from_cached_values(self):
        lim = FakeLimits({"five_hour": win(95.0, at(3600))}, status="error", error="Offline")
        app = make_app(lim)
        app.check_alerts(NO_FORECAST)
        self.assertEqual(app.tray.sent, [])

    def test_forecast_alert_once_even_across_restart(self):
        state = {}
        lim = FakeLimits({"five_hour": win(50.0, at(3600))})
        model = {"verdict": "hit", "eta": 900}
        app = make_app(lim, state=state)
        app.check_alerts(model)
        app.check_alerts(model)
        make_app(lim, state=state).check_alerts(model)
        self.assertEqual(app.tray.sent, ["Claude: limit ahead"])
        quiet = make_app(FakeLimits({"five_hour": win(50.0, at(7200))}))
        quiet.check_alerts({"verdict": "unknown", "eta": None})  # no estimate -> no alert
        self.assertEqual(quiet.tray.sent, [])

    def test_reset_alert_only_when_timely(self):
        def run(prev_reset_ago):
            old = at(-prev_reset_ago)
            state = {"last_windows": {"five_hour": {"rkey": f"five_hour|{reset_key(old)}",
                                                     "pct": 50, "resets": old.isoformat()}}}
            app = make_app(FakeLimits({"five_hour": win(3.0, at(4 * 3600))}), state=state)
            app.check_alerts(NO_FORECAST)
            app.check_alerts(NO_FORECAST)
            return [t for t in app.tray.sent if "reset" in t]
        self.assertEqual(len(run(10 * 60)), 1)   # reset 10 min ago -> one notification
        self.assertEqual(run(5 * 3600), [])      # reset hours ago (asleep) -> no stale alert

    def test_reset_triggers_one_refresh_not_one_per_second(self):
        lim = FakeLimits({"five_hour": win(60.0, at(-30))})
        app = make_app(lim)
        for _ in range(5):
            app.check_alerts(NO_FORECAST)
        self.assertEqual(lim.refreshes, 1)


class FetcherTests(unittest.TestCase):
    def test_unrecognised_response_keeps_last_values_and_flags_error(self):
        f = LimitsFetcher(SimpleNamespace(cfg=dict(DEFAULT_CONFIG)), make_history())
        f.windows = {"five_hour": win(40.0, at(3600))}
        f.status, f.updated = "ok", time.time() - 120
        old_http, old_read = data.http_json, LimitsFetcher._read_oauth
        data.http_json = lambda *a, **k: {"something_new": {"x": 1}}
        LimitsFetcher._read_oauth = staticmethod(lambda: {"accessToken": "t", "expiresAt": 9e15})
        try:
            f.poll_once()
        finally:
            data.http_json, LimitsFetcher._read_oauth = old_http, old_read
        self.assertEqual((f.status, f.error), ("error", "Unexpected response"))
        self.assertEqual(f.windows["five_hour"]["pct"], 40.0)  # not replaced by an empty result


if __name__ == "__main__":
    unittest.main()
