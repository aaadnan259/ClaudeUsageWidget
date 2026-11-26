r"""Tests for the past-activity views: heatmap week paging and last week's weekly peak.

Run:  .venv\Scripts\python.exe -m unittest discover -s tests -v
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

from cuw.data import History, LogScanner  # noqa: E402
from test_refinement import FakeLimits, make_app, win  # noqa: E402

UTC = dt.timezone.utc
DAY = 86400
logging.getLogger("ClaudeUsageWidget").disabled = True


def make_history(samples=()):
    h = History.__new__(History)
    h.lock, h.demo, h.samples = threading.Lock(), False, list(samples)
    return h


def wk(t, pct, resets):
    return {"t": t, "w": {"seven_day": [pct, resets.isoformat()]}}


def scanner_with(events):
    s = LogScanner(SimpleNamespace(cfg={"logs_refresh_seconds": 5}))
    s.events = {(str(i), "r"): (ts, tok, "s") for i, (ts, tok) in enumerate(events)}
    return s


def noon(days_ago):
    d = dt.datetime.now().replace(hour=12, minute=0, second=0, microsecond=0) - dt.timedelta(days=days_ago)
    return d.timestamp()


class WeekPagingTests(unittest.TestCase):
    def test_four_weeks_newest_first_with_correct_buckets(self):
        st = scanner_with([(noon(0), 10), (noon(6), 20), (noon(7), 300), (noon(27), 4000)])._compute()
        weeks = st["weeks"]
        self.assertEqual(len(weeks), 4)
        self.assertEqual([w["total"] for w in weeks], [30, 300, 0, 4000])
        self.assertEqual(weeks[0]["label"], "Last 7 days")
        self.assertEqual(weeks[0]["heat"][6][12], 10)   # today, noon
        self.assertEqual(weeks[0]["heat"][0][12], 20)   # 6 days ago is the first row of this week
        self.assertEqual(weeks[1]["heat"][6][12], 300)  # 7 days ago is the last row of last week
        for w in weeks:
            self.assertEqual(len(w["heat"]), 7)
            self.assertEqual(len(w["day_dates"]), 7)

    def test_this_week_unchanged_for_existing_tiles(self):
        st = scanner_with([(noon(0), 10), (noon(3), 5), (noon(10), 999)])._compute()
        self.assertEqual(st["week"], 15)                 # the "tokens, 7 days" tile ignores older weeks
        self.assertEqual(st["heat"], st["weeks"][0]["heat"])
        self.assertEqual(st["today"], 10)

    def test_older_events_beyond_four_weeks_are_dropped(self):
        st = scanner_with([(noon(40), 50)])._compute()
        self.assertTrue(all(w["total"] == 0 for w in st["weeks"]))

    def test_model_formats_week_totals_and_marks_empty(self):
        stats = scanner_with([(noon(0), 1500)])._compute()
        app = make_app(FakeLimits({}))
        app.logs = SimpleNamespace(snapshot=lambda: stats)
        weeks = app.build_model()["stats"]["weeks"]
        self.assertEqual(weeks[0]["total"], "1.5k")
        self.assertFalse(weeks[0]["empty"])
        self.assertTrue(weeks[1]["empty"])


class PrevPeakTests(unittest.TestCase):
    def setUp(self):
        now = time.time()
        self.cur = dt.datetime.fromtimestamp(now + 3 * DAY, UTC)
        self.prev = self.cur - dt.timedelta(days=7)
        self.older = self.cur - dt.timedelta(days=14)
        self.now = now

    def test_peak_of_previous_window_only(self):
        h = make_history([
            wk(self.now - 12 * DAY, 95.0, self.older),          # two windows ago: ignored
            wk(self.now - 6 * DAY, 40.0, self.prev),
            wk(self.now - 5 * DAY, 81.6, self.prev + dt.timedelta(seconds=20)),  # reset jitter
            wk(self.now - 4.5 * DAY, 70.0, self.prev),
            wk(self.now - DAY, 50.0, self.cur),                  # current window: ignored
        ])
        self.assertEqual(h.prev_window_peak("seven_day", self.cur), 81.6)

    def test_none_without_earlier_window(self):
        h = make_history([wk(self.now - DAY, 50.0, self.cur)])
        self.assertIsNone(h.prev_window_peak("seven_day", self.cur))
        self.assertIsNone(make_history().prev_window_peak("seven_day", self.cur))

    def test_cache_updates_when_samples_change(self):
        h = make_history([wk(self.now - 6 * DAY, 40.0, self.prev)])
        self.assertEqual(h.prev_window_peak("seven_day", self.cur), 40.0)
        h.samples.append(wk(self.now - 5 * DAY, 60.0, self.prev))
        self.assertEqual(h.prev_window_peak("seven_day", self.cur), 60.0)

    def test_model_shows_peak_on_weekly_row_only(self):
        h = make_history([wk(self.now - 5 * DAY, 99.7, self.prev)])
        lim = FakeLimits({"five_hour": win(30, dt.datetime.fromtimestamp(self.now + 3600, UTC)),
                          "seven_day": win(20, self.cur, "Weekly", "Wk")})
        rows = {r["key"]: r for r in make_app(lim, h).build_model()["rows"]}
        self.assertEqual(rows["seven_day"]["prev_peak"], 99)   # same rounding rule: never a fake 100
        self.assertIsNone(rows["five_hour"]["prev_peak"])

    def test_no_peak_while_weekly_window_is_pending(self):
        h = make_history([wk(self.now - 9 * DAY, 70.0, self.prev - dt.timedelta(days=7))])
        past = dt.datetime.fromtimestamp(self.now - 60, UTC)
        lim = FakeLimits({"seven_day": win(20, past, "Weekly", "Wk")}, updated=self.now - 3600)
        row = make_app(lim, h).build_model()["rows"][0]
        self.assertTrue(row["pending"])
        self.assertIsNone(row["prev_peak"])


if __name__ == "__main__":
    unittest.main()
