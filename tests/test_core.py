"""Unit tests for the parts most likely to break silently.

Run:  python -m unittest discover -s tests -v
"""
import datetime as dt
import json
import os
import sys
import tempfile
import threading
import time
import unittest
from types import SimpleNamespace

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from cuw import data  # noqa: E402
from cuw.app import (App, display_pct, fmt_dur, fmt_eta, fmt_tokens, forecast_verdict,  # noqa: E402
                     freshness, level)
from cuw.data import FIVE_HOURS, History, LimitsFetcher, LogScanner, parse_iso, reset_key  # noqa: E402

UTC = dt.timezone.utc


def make_history(samples=()):
    """A History with no file I/O."""
    h = History.__new__(History)
    h.lock, h.demo, h.samples = threading.Lock(), False, list(samples)
    return h


def sample(t, pct, resets, key="five_hour"):
    return {"t": t, "w": {key: [pct, resets.isoformat()]}}


class ParsingTests(unittest.TestCase):
    def test_parse_iso_variants(self):
        self.assertEqual(parse_iso("2026-11-12T09:59:59Z"), dt.datetime(2026, 11, 12, 9, 59, 59, tzinfo=UTC))
        self.assertEqual(parse_iso("2026-11-12T09:59:59.661600+00:00").microsecond, 661600)
        self.assertEqual(parse_iso("2026-11-12T09:59:59").tzinfo, UTC)  # naive -> UTC
        self.assertIsNone(parse_iso(None))
        self.assertIsNone(parse_iso("not a date"))

    def test_reset_key_ignores_jitter(self):
        a = dt.datetime(2026, 11, 12, 9, 59, 59, 661600, tzinfo=UTC)
        b = dt.datetime(2026, 11, 12, 10, 0, 0, 12, tzinfo=UTC)
        self.assertEqual(reset_key(a), reset_key(b))
        self.assertNotEqual(reset_key(a), reset_key(a + dt.timedelta(hours=1)))
        self.assertEqual(reset_key(None), "")

    def test_usage_response_parsing(self):
        resp = {
            "five_hour": {"utilization": 7.0, "resets_at": "2026-11-12T09:59:59.661600+00:00"},
            "seven_day": {"utilization": 25, "resets_at": "2026-11-17T02:59:59Z"},
            "seven_day_sonnet": {"utilization": 3.0, "resets_at": None},
            "seven_day_opus": None,
            "extra_usage": {"is_enabled": False},
        }
        f = LimitsFetcher.__new__(LimitsFetcher)
        f.store = SimpleNamespace(cfg={"show_model_limits": False})
        out = f._parse(resp)
        self.assertEqual(set(out), {"five_hour", "seven_day"})
        self.assertEqual(out["five_hour"]["pct"], 7.0)
        self.assertEqual(out["seven_day"]["title"], "Weekly")
        self.assertIsInstance(out["seven_day"]["pct"], float)
        f.store.cfg["show_model_limits"] = True
        out = f._parse(resp)
        self.assertIn("seven_day_sonnet", out)
        self.assertEqual(out["seven_day_sonnet"]["title"], "Sonnet")
        self.assertIsNone(out["seven_day_sonnet"]["resets"])


class ForecastTests(unittest.TestCase):
    def setUp(self):
        self.now = time.time()
        self.resets = dt.datetime.fromtimestamp(self.now + 3 * 3600, UTC)

    def test_not_enough_data(self):
        h = make_history([sample(self.now - 60, 10, self.resets)])
        self.assertIsNone(h.eta_seconds("five_hour", {"pct": 10, "resets": self.resets}))

    def test_steady_rise(self):
        # +1% per minute over the last 10 minutes, now at 40% -> 60 minutes to 100%
        h = make_history([sample(self.now - (10 - i) * 60, 30 + i, self.resets) for i in range(11)])
        eta = h.eta_seconds("five_hour", {"pct": 40, "resets": self.resets})
        self.assertAlmostEqual(eta / 60, 60, delta=0.5)

    def test_flat_usage_is_infinite(self):
        h = make_history([sample(self.now - (10 - i) * 60, 40, self.resets) for i in range(11)])
        self.assertEqual(h.eta_seconds("five_hour", {"pct": 40, "resets": self.resets}), float("inf"))

    def test_other_windows_are_ignored(self):
        old = dt.datetime.fromtimestamp(self.now - 3600, UTC)
        pts = [sample(self.now - (10 - i) * 60, 90 - i, old) for i in range(11)]  # previous window
        pts += [sample(self.now - 120, 5, self.resets), sample(self.now - 60, 5, self.resets)]
        h = make_history(pts)
        self.assertIsNone(h.eta_seconds("five_hour", {"pct": 5, "resets": self.resets}))

    def test_last_windows_skips_expired(self):
        expired = dt.datetime.fromtimestamp(self.now - 60, UTC)
        h = make_history([{"t": self.now, "w": {"five_hour": [12, expired.isoformat()],
                                                "seven_day": [30, self.resets.isoformat()]}}])
        w = h.last_windows()
        self.assertEqual(set(w), {"seven_day"})
        self.assertEqual(w["seven_day"]["pct"], 30)


class TimelineTests(unittest.TestCase):
    def test_projection_hits_limit_where_the_forecast_says(self):
        now = time.time()
        resets = dt.datetime.fromtimestamp(now + 2.5 * 3600, UTC)  # halfway through the 5h window
        app = SimpleNamespace(history=make_history([sample(now - 600, 40, resets)]))
        five = {"pct": 50.0, "resets": resets}
        tl = App.build_timeline(app, five, eta=3600, updated=now)  # 1h to 100% < 2.5h left
        self.assertAlmostEqual(tl["now_x"], 0.5, places=2)
        self.assertTrue(tl["hit"])
        start = resets.timestamp() - FIVE_HOURS
        # where the projection meets 100% == last sample time + eta (same estimate as the text)
        self.assertAlmostEqual(start + tl["proj"][0] * FIVE_HOURS, now + 3600, delta=2)
        self.assertEqual(tl["proj"][1], 100.0)
        self.assertEqual(tl["points"][0], (0.0, 0.0))
        xs = [p[0] for p in tl["points"]]
        self.assertEqual(xs, sorted(xs))

    def test_projection_stays_under_and_stops_at_reset(self):
        now = time.time()
        resets = dt.datetime.fromtimestamp(now + 2.5 * 3600, UTC)
        app = SimpleNamespace(history=make_history())
        tl = App.build_timeline(app, {"pct": 20.0, "resets": resets}, eta=10 * 3600, updated=now)
        self.assertFalse(tl["hit"])
        self.assertEqual(tl["proj"][0], 1.0)  # never past the end of the window
        self.assertLess(tl["proj"][1], 100)
        self.assertGreater(tl["proj"][1], 20)

    def test_actual_line_ends_at_last_fetch_not_now(self):
        now = time.time()
        resets = dt.datetime.fromtimestamp(now + 2.5 * 3600, UTC)
        app = SimpleNamespace(history=make_history())
        tl = App.build_timeline(app, {"pct": 30.0, "resets": resets}, eta=None, updated=now - 1200)
        self.assertAlmostEqual(tl["points"][-1][0], tl["last_x"])
        self.assertLess(tl["last_x"], tl["now_x"] - 0.05)  # 20 min stale shows as a gap
        self.assertIsNone(tl["proj"])                      # no estimate -> no projected path

    def test_flat_usage_projects_flat(self):
        now = time.time()
        resets = dt.datetime.fromtimestamp(now + 3600, UTC)
        app = SimpleNamespace(history=make_history())
        tl = App.build_timeline(app, {"pct": 40.0, "resets": resets}, eta=float("inf"), updated=now)
        self.assertEqual(tl["proj"], [1.0, 40.0])
        self.assertFalse(tl["hit"])


class FormattingTests(unittest.TestCase):
    def test_fmt_dur(self):
        self.assertEqual(fmt_dur(4 * 3600 + 23 * 60), "4h 23m")
        self.assertEqual(fmt_dur(4 * 3600 + 23 * 60, compact=True), "4h23m")
        self.assertEqual(fmt_dur(5 * 60), "5m")
        self.assertEqual(fmt_dur(10), "1m")
        self.assertEqual(fmt_dur(-5), "1m")
        self.assertEqual(fmt_dur(3 * 86400 + 2 * 3600), "3d 2h")

    def test_fmt_tokens(self):
        self.assertEqual(fmt_tokens(999), "999")
        self.assertEqual(fmt_tokens(1234), "1.2k")
        self.assertEqual(fmt_tokens(353_000), "353k")
        self.assertEqual(fmt_tokens(3_200_000), "3.2M")

    def test_levels(self):
        self.assertEqual(level(74.9), "ok")
        self.assertEqual(level(75), "warn")
        self.assertEqual(level(90), "crit")


class LogScannerTests(unittest.TestCase):
    def test_counts_and_dedupes(self):
        with tempfile.TemporaryDirectory() as tmp:
            proj = os.path.join(tmp, "proj")
            os.makedirs(proj)
            ts = dt.datetime.now(UTC).isoformat()
            line = lambda mid, rid, inp, out: json.dumps({
                "timestamp": ts, "requestId": rid, "sessionId": "s1",
                "message": {"id": mid, "usage": {"input_tokens": inp, "output_tokens": out,
                                                  "cache_creation_input_tokens": 100,
                                                  "cache_read_input_tokens": 999999}}})
            with open(os.path.join(proj, "a.jsonl"), "w", encoding="utf-8") as f:
                f.write(line("m1", "r1", 10, 20) + "\n")
                f.write(line("m1", "r1", 10, 20) + "\n")       # duplicate (streamed twice)
                f.write('{"type": "user", "message": {}}\n')    # no usage
                f.write(line("m2", "r2", 1, 2) + "\n")
                f.write(line("m3", "r3", 5, 5))                 # partial line, no newline yet
            old = data.PROJECTS_DIR
            data.PROJECTS_DIR = tmp
            try:
                s = LogScanner(SimpleNamespace(cfg={"logs_refresh_seconds": 5}))
                s._scan()
                st = s._compute()
                self.assertEqual(st["msgs"], 2)
                self.assertEqual(st["today"], (10 + 20 + 100) + (1 + 2 + 100))  # cache reads excluded
                with open(os.path.join(proj, "a.jsonl"), "a", encoding="utf-8") as f:
                    f.write("\n")  # the partial line completes
                s._scan()
                self.assertEqual(s._compute()["msgs"], 3)
                self.assertEqual(st["active"], 1)
            finally:
                data.PROJECTS_DIR = old


if __name__ == "__main__":
    unittest.main()
