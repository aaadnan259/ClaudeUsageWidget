# Claude Usage Widget: architecture and project context

This is the reference for anyone, human or coding agent, changing this project.
It covers:
- what the app does
- how it works, end to end
- why it's built the way it is
- what's fragile
- everything that has changed so far

Keep it updated when you change behaviour. The *Gotchas* and *Change history* sections matter most.

---

> **Two implementations.**
> - **The native Rust app in `rust/` is the installed one, at feature parity:** about 15 MB, under 1% of one CPU core. Its plan, measurements and lessons are in `rust/PLAN.md`.
> - **The Python + WebView2 version in `cuw/` is the fallback.** Most of this document describes it.
>
> Both share the data files, the data sources, the view-model rules (§6.4) and the gotchas about the API, `reset_key()`, freshness and sleep. Rust-specific notes:
> - drawing is Direct2D in software (WARP) over DirectComposition + DWM acrylic
> - animations run in the compositor
> - demo mode and tests never write the config/state files
> - Quit terminates the process right after the message loop (destroying the graphics objects at thread exit hangs)

## 1. What it is

A Windows 11 desktop widget that shows the user's **Claude plan usage limits live**.
These are the same "5-hour limit" and "Weekly · all models" percentages the Claude app shows under *Settings → Usage*.
It's a small frosted-glass **pill** that sits just above the taskbar near the clock.
Clicking it opens a **card** with:
- rings or bars
- a pace forecast
- a 5-hour timeline
- Claude Code activity
- a usage heatmap
- settings

**Owner context.** These facts drove the design:
- The user is on the **Claude Pro** plan and mostly uses **Claude Code**: the CLI, plus the Code tab of the Claude desktop app.
- The tool is for **personal use, polished**. It isn't distributed, so there's no code signing and no store listing.
- The repo is private on GitHub: `aaadnan259/ClaudeUsageWidget`.
- Development machine: Windows 11 at 2880×1800 with 175% scaling.
  It has *several* Python installs: python.org 3.14, the Python install manager's 3.14 (`pythoncore-3.14-64`), and 3.12.
  That matters (see §11).

---

## 2. Feature list

### Always-visible pill
- Sits bottom-right above the taskbar by default, with an 8 px margin from the work area.
- **Three styles**, chosen in settings or from the right-click menu:
  - `bars` (236×52): two rows, `5h ▬▬ 57% 1h48` and `Wk ▬ 36% Sun 10PM`
  - `ring` (196×52): a mini ring, a big %, and "Session · 1h48" / "Weekly 36%"
  - `text` (214×40): spark icon, %, countdown and weekly %
- **Frosted-glass look**: a blurred, tinted snapshot of whatever is behind it, refreshed about every 6 s.
- Colours shift green → amber at 75% → red at 90%, with gradient fills.
- **Fades when idle** (optional): content opacity 0.62 unless the mouse is over it.
- **Draggable** anywhere, including onto the taskbar. The position is saved and clamped onto a visible monitor. *Reset position* puts it back.
- **Click** toggles the card. **Right-click** opens a native Windows menu: show details, refresh, pill style, start with Windows, reset position, data folder, quit.
- **Stale = muted.** When the values aren't fresh, the numbers and fills turn grey and an amber dot appears. Hovering the pill shows a tooltip such as "Offline · Last updated 14m ago". Stale means:
  - showing cached or last-known values
  - offline, rate-limited or sign-in expired
  - an unexpected response
  - the last success being more than 2 × the refresh interval + 60 s old
- A window that **has reset since the last fetch** shows "—" and "updating" instead of the old %.
- When there's no data it shows a status message on one line: "Sign in to see limits · run `claude` → `/login`", "Rate-limited, retrying", "Offline", and so on.

### Details card (360 px wide, height fits content)
- **Header**: Claude spark logo, "Claude usage", plan chip (e.g. "Pro"), a view toggle (rings ↔ bars) and a settings gear.
- **Rings view**: Session and Weekly rings (120 px) with gradient strokes, a glow, count-up numbers, a sweep-in animation when opened, and a pulse at 90% or more.
- **Last week's peak** (weekly row only): "All models · last week 82%" in bars view, "Last week peaked at 82%" under the weekly ring. It's the highest weekly % *the widget recorded* in the previous weekly window (it only records while running, so it can under-read; the tooltip says so). Hidden when history has no earlier window, and while the weekly row is pending.
- **Bars view**: TokenEater-style rows. Each has an icon, title, subtitle ("5h sliding window" / "All models"), a big gradient %, "Reset in 1h48" or "Reset Sun 10:00 PM", and a wide bar.
- **Pace forecast** line. Deliberately coarse and honest:
  - "At this pace, limit in ~1h 15m" (amber; red under 30 min). ETAs are rounded: "under 10 min", 5-minute steps under an hour, then 15 or 30 minutes.
  - "At this pace, you'll reach the limit around reset time": the estimate is within ±10% of the reset, a band that stops it flip-flopping.
  - "At this pace, you'll stay under the limit until reset" (green)
  - "Steady · no current pace toward the limit": flat or falling usage, so no exhaustion time is made up.
  - "Not enough recent activity to estimate": thin history.
  - "Forecast paused until data refreshes": stale data.
  - "Limit reached · resets in 1h 48m"
- **5-hour timeline** (SVG):
  - a legend: **Actual** (solid accent line) and **Projected** (thin, dashed, muted grey; it never looks like measured data)
  - the actual line runs from the window start to the **last fetch**, not to "now". A faint dotted tick marks "now" when the two differ
  - the projection starts at the last fetch and uses the same estimate as the forecast text, so where it meets 100% is the displayed ETA
  - the projection is hidden when there's no estimate or the data is stale, and never goes past the reset
  - an area fill under the actual line
  - a dotted 100% line and grid ticks
  - a "now" dot with a pulsing halo
  - a dashed projection to where 100% is reached, or to the window's end
  - start, now and end time labels, with collision avoidance
- **Claude Code activity**: three stat tiles with explicit units. "tokens today" (plus N responses), "tokens, 7 days", and "tokens/min" (plus active sessions or idle).
  A hover tooltip explains what's counted: input + output + cache-write tokens, no cache reads, no claude.ai chats; responses include tool steps; tokens/min is the 10-minute average.
- **Heatmap**: 7 days × 24 hours of Claude Code tokens, with a hover tooltip ("Tue Nov 18 3 PM · 45k tokens").
  - A header shows the week ("Last 7 days" or "Nov 13 – Nov 19") and its token total, with ‹ › buttons to page back up to **3 earlier 7-day blocks** (4 weeks in all).
  - ‹ is disabled past the oldest week that has any data; Claude Code deletes transcripts after ~30 days by default, so there's nothing older to show.
  - All weeks share one colour scale so they can be compared by eye. Re-opening the card returns to the current week.
  - The three stat tiles always describe today / the last 7 days, whichever week the grid shows.
- **Footer**: "Updated just now / Nm ago / Nh Mm ago". When stale it turns amber and reads like "Last updated 14m ago · Offline", the live dot turns amber, and the limits section is dimmed. Same rule as the pill.
- **Settings panel** (gear), saved immediately:
  - pill style
  - theme (Auto / Dark / Light)
  - Start with Windows
  - alerts at 75% and 90%
  - alert on reset
  - alert when on pace to hit the limit
  - fade when idle
  - hide during fullscreen
  - taskbar button (needs a restart)
  - refresh interval (30 s / 1 m / 2 m / 5 m)
  - Done / Data folder / Quit
- Closes on Esc, or on any click outside the card and pill.

### Windows integration
- **Tray icon** (pystray): a ring icon with the session % as a number, coloured by level. Its tooltip reads "Claude · Session 57% (resets in 1h48) · Weekly 36%". Its menu mirrors the main actions.
- **Taskbar button** (optional): a minimised, invisible native window whose taskbar button shows:
  - a live ring icon
  - a Windows **progress overlay**: normal under 75%, *paused* (yellow) from 75%, *error* (red) from 90%

  Clicking the button toggles the card. *Close window* quits.
- **Notifications** (Windows toasts via the tray icon):
  - crossing 75% and 90%, separately for session and weekly, once per window. Saved in `state.json`, so restarts and wake-ups don't repeat them. Never sent from cached or stale values.
  - when a window resets, once per transition, and only if the reset was within the last 30 minutes. A reset that happened while the PC was asleep doesn't produce a late toast.
  - when you're on pace to hit the 5-hour limit within 30 minutes. It needs the "hit" verdict on fresh data and is sent at most once per window (also saved in `state.json`).
- **Start with Windows** through the HKCU `Run` registry key, pointing at the project's `.venv\Scripts\pythonw.exe`.
- **Hides itself during fullscreen apps** (games, video) on the pill's monitor.
- **Follows the Windows light/dark theme** when theme is `auto`. It checks every second and restyles live.
- Stays topmost, steals no focus, and has no Alt-Tab entry (the pill and card are tool windows).
- **Single instance**: launching it again while it runs opens the card.
- **Installed as an app**:
  - Start menu and Desktop shortcuts with a custom icon
  - the shortcut carries the same AppUserModelID as the running app, so a pinned shortcut and the live taskbar button are one icon

### Reliability
- **Offline / rate-limit fallback**: on start-up and on any error, it shows the last known values from history, clearly marked stale.
- **Unrecognised response**: if the API answers with no known windows, it's treated as an error ("Unexpected response"). The last values are kept and marked stale, never replaced by blanks or 0%. Only the response's key names are logged.
- **Reset boundary**: when a reset time passes, the widget asks for fresh data **once** for that window. It used to request every second while the API still reported the old reset.
- **429 backoff**: waits double each time, from 120 s up to 900 s, on top of the interval.
- **Sleep-safe timing**: waits are split into 5-second steps against the real clock, because Windows timed waits pause during sleep.
- **Wake detection**: a gap of more than 30 s between loop ticks logs "resumed after Ns", refreshes immediately and re-captures the glass backdrop.
- **Watchdog**: if the limits thread misses its schedule by more than 150 s, it logs every thread's stack and replaces the thread.
- **Automatic token refresh** when the CLI's access token expires. The new token is written back to the CLI's credentials file.
- **Start-up error reporting**: any start-up failure, including import errors, shows a message box and is written to `startup.log`.

---

## 3. Tech stack

| Layer | Choice | Version | Notes |
|---|---|---|---|
| Language | Python | 3.14 (works on 3.11+) | Runs as `pythonw.exe` (no console) |
| UI host | **pywebview** (EdgeChromium backend) | 6.2.1 | Hosts HTML/CSS/JS in **Microsoft Edge WebView2** inside WinForms |
| .NET bridge | pythonnet + clr_loader | 3.2.1 / 0.3.1 | Needed by pywebview's WinForms backend |
| Web runtime | WebView2 Runtime | built into Win 11 (157.x seen) | Launched with custom flags (§9) |
| UI | Plain HTML/CSS/JS, no framework, no build step | n/a | `cuw/web/*.html`, `glass.css` |
| Imaging | Pillow | 13.0.0 | Glass backdrops, tray/taskbar/app icons |
| Tray | pystray | 0.19.5 | Tray icon, menu, notifications |
| Win32 | `ctypes` (no pywin32) | stdlib | Windows, DWM, GDI, COM (ITaskbarList3, IPropertyStore), registry |
| HTTP | Windows' built-in **`curl.exe`** via `subprocess` | system | Falls back to `urllib`. See §11 for why not `urllib` |
| Tests | `unittest` | stdlib | `tests/test_core.py` |
| Packaging | private **`.venv`** + `Install.bat` / `install.py` | n/a | No PyInstaller yet |

Other pinned transitive packages: `typing_extensions 4.16.0`, `bottle 0.13.4`, `proxy_tools 0.1.0`.

---

## 4. Repository layout

```
claude_usage_widget.pyw   Entry point. Start-up log, hand-off to .venv, single instance, error box.
Install.bat               One-click setup: build .venv, pip install -r requirements.txt, run install.py, start.
install.py                Generates assets/app.ico, Start menu + Desktop .lnk (with AppUserModelID),
                          points autostart at .venv. `--remove` uninstalls the shortcuts and autostart.
requirements.txt          Pinned versions.
assets/app.ico            App icon (dark rounded square, teal→green ring, orange spark). Generated by install.py.
cuw/__init__.py           Package version.
cuw/config.py             Paths, DEFAULT_CONFIG, Store (config.json + state.json), rotating logger.
cuw/data.py               http_json (curl), LimitsFetcher thread, History (history.jsonl + forecast),
                          LogScanner thread (Claude Code logs).
cuw/app.py                App: creates the 2 WebView windows, JS API, drag, card, glass, main loop,
                          watchdog, view-model builder, timeline, alerts, tray/taskbar updates.
cuw/winutil.py            ctypes Win32: DPI, monitors, placement, popup styling, fullscreen check, theme,
                          GDI region grab + frosted backdrop, autostart, ring icons, NativeHost.
cuw/tray.py               pystray wrapper.
cuw/web/glass.css         Shared design tokens (dark/light), glass layers, gradient helpers.
cuw/web/pill.html         Pill UI (bars/ring/text) + JS.
cuw/web/card.html         Card UI (rings/bars, forecast, timeline, stats, heatmap, settings) + JS.
tests/test_core.py        14 unit tests.
legacy/claude_usage_widget_v2.pyw   Previous single-file tkinter version (reference only, not used).
docs/ARCHITECTURE.md      This file.
```

**Outside the repo**, in `%APPDATA%\ClaudeUsageWidget\`:

| File | Contents |
|---|---|
| `config.json` | User settings (§8) |
| `state.json` | Runtime state: pill position, card view, alert bookkeeping |
| `history.jsonl` | Limit samples, one JSON per line, kept 35 days |
| `widget.log` | App log, rotating 512 KB × 2 |
| `startup.log` | Launch trace; deleted when over 200 KB |
| `taskbar.ico` | Current taskbar ring icon, regenerated |
| `webview/` | WebView2 user-data folder (private mode, so mostly empty) |
| `.req-*.json` | Short-lived temp request bodies (token refresh only) |

The user's Claude data is read from `~/.claude/`: `.credentials.json` (read/write, token refresh only) and `projects/**/*.jsonl` (read-only).

---

## 5. Runtime architecture

### Processes
```
pythonw.exe (.venv redirector) ─► pythonw.exe (base interpreter, the actual app)
                                    └─ msedgewebview2.exe  browser process
                                         ├─ renderer (ONE, shared by pill + card: --renderer-process-limit=1)
                                         ├─ gpu-process (small: GPU disabled)
                                         ├─ utility: network service, storage service
                                         └─ crashpad handler
curl.exe  (short-lived, one per HTTP call)
```

### Threads inside the Python process
| Thread | Owner | Job |
|---|---|---|
| Main | `webview.start()` | WinForms/WebView2 message loop. Must stay on the main thread. |
| `_after_start` | pywebview `func` | Finds window handles, styles them, starts NativeHost and tray, shows the pill, starts `loop`. |
| `loop` | `App.loop` | Every 1 s: theme check, wake detection, watchdog, build model, push to JS, alerts, fullscreen check, topmost, pill glass refresh (≤ every 6 s), re-place the pill every 15 s. |
| `LimitsFetcher` | `cuw/data.py` | Polls Anthropic every `limits_refresh_seconds` (default 120 s) plus any backoff. `refresh_now()` wakes it. |
| `LogScanner` | `cuw/data.py` | Every 5 s: incremental tail of Claude Code JSONL logs, then computes stats. |
| `NativeHost` | `cuw/winutil.py` | Own Win32 message loop: taskbar button and ITaskbarList3 progress, native popup menus, second-instance "show" message. |
| pystray | `cuw/tray.py` | Tray icon (`run_detached`). |
| `_drag_loop` | temporary | Polls the mouse while the left button is held; moves the pill. |
| `_watch_outside` | temporary, while the card is open | Closes the card on a click outside it and the pill. |

### Data flow
```mermaid
flowchart LR
  subgraph Sources
    A[Anthropic /api/oauth/usage]:::ext
    C[~/.claude/.credentials.json]
    L[~/.claude/projects/**/*.jsonl]
    S[Screen pixels behind widget]
  end
  C -->|access token| F[LimitsFetcher]
  F -->|curl GET| A
  F -->|samples| H[(History\nhistory.jsonl)]
  L --> LS[LogScanner]
  F --> M[App.build_model]
  H -->|series + ETA| M
  LS --> M
  M -->|slim pill_model JSON| P[pill.html update()]
  M -->|full model JSON, if open| K[card.html update()]
  M --> T[Tray + NativeHost icons/progress]
  M --> AL[Alerts → toasts]
  S -->|GDI grab + blur| G[setBackdrop(dataURL)]
  G --> P & K
  P & K -->|pywebview.api.*| API[Api → App]
  classDef ext fill:#eee,stroke:#999
```

### Start-up sequence
1. `claude_usage_widget.pyw` appends "start exe=…" to `startup.log` and `chdir`s to the project.
2. If it isn't running inside `.venv`, it re-launches itself with `.venv\Scripts\pythonw.exe` (detached) and exits. This is the **hand-off**.
3. It imports `cuw.winutil` and calls `init_process()`: per-monitor DPI awareness v2 and `SetCurrentProcessExplicitAppUserModelID("ClaudeUsageWidget")`.
4. It takes the single-instance mutex `Local\ClaudeUsageWidget`. If that's taken, it posts `WM_APP_SHOW` to the running instance's NativeHost window (class `ClaudeUsageWidgetHost`) and exits.
5. It imports `cuw.app`. An `ImportError` shows a message box with the exact `pip install` command.
6. `App.__init__`:
   - `Store`
   - first-run autostart
   - `History`
   - `LimitsFetcher`, seeded with the last known windows from history
   - `LogScanner`
   - theme
7. `App.run`:
   - sets `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS`
   - starts the data threads
   - creates the hidden pill and card windows
   - `webview.start(private_mode=True)`
8. `_after_start`:
   - finds both window handles **by title** (`ClaudeUsagePill` / `ClaudeUsageCard`)
   - styles them as tool windows with rounded corners
   - starts NativeHost and the tray
   - waits 0.8 s for the pages
   - places and shows the pill
   - captures the first backdrop
   - starts `loop`

---

## 6. Data sources in detail

### 6.1 Plan limits (Anthropic)
- **Endpoint**: `GET https://api.anthropic.com/api/oauth/usage`.
  Headers: `Authorization: Bearer <accessToken>`, `anthropic-beta: oauth-2025-04-20`, `User-Agent: claude-usage-widget/3.0`.
  It's **undocumented**. It's what Claude Code's `/usage` uses, and other community widgets use it too. It may change without notice.
- **Auth**: the Claude Code CLI's OAuth token, read from `~/.claude/.credentials.json` → `claudeAiOauth.{accessToken, refreshToken, expiresAt, subscriptionType, …}`.
  The user signs in once with `claude` → `/login`. `subscriptionType` (for example `"pro"`) becomes the plan chip.
- **Token refresh**: runs when the token expires within 60 s, or on a 401.
  - It POSTs `grant_type=refresh_token` with the Claude Code public client id `9d1c250a-e61b-44d9-88ed-5944d1962f5e`.
  - It tries `console.anthropic.com/v1/oauth/token`, then `platform.claude.com/v1/oauth/token`.
  - The result is written back to the credentials file, but only if the CLI hasn't refreshed it in the meantime (it compares refresh tokens).
- **Response shape** (fields used):
  `{"five_hour": {"utilization": 57.0, "resets_at": "ISO"}, "seven_day": {...}, "seven_day_sonnet": {...}|null, ...}`.
  `utilization` is a **percentage from 0 to 100**.
  `seven_day_*` per-model windows are shown only when `show_model_limits` is true; there's no UI toggle for it yet.
  Other keys, such as `extra_usage`, are ignored.
- **Status values** shown in the UI:
  - `loading`
  - `ok`
  - `signin` (no token)
  - `expired` (401/403 after a refresh attempt)
  - `error` (HTTP 429 → "Rate-limited, retrying"; network → "Offline"; no known windows → "Unexpected response"; other → "HTTP n"; also "Starting" (shown as "Connecting") or "Reconnecting" while showing cached values)
- **Rate limits**: polling every 60 s plus several quick restarts did trigger 429s. That's why the default is 120 s and there's exponential backoff.
  **Don't restart the app in a tight loop while testing against the real API.** Use `--demo`.

### 6.2 History and forecast (`History`)
- Each successful fetch appends `{"t": epoch, "w": {"five_hour": [pct, resets_iso], "seven_day": [...]}}` to `history.jsonl`.
  It's skipped if unchanged and the previous sample is less than 300 s old.
  At start-up, entries older than 35 days are dropped and the file is rewritten.
- **Window identity**: `reset_key(resets) = round(resets.timestamp()/600)`.
  `resets_at` jitters by fractions of a second between polls (for example `09:59:59.66`), so exact comparison would split one window into many.
  Use `reset_key` everywhere you compare windows.
- **`series(key, resets)`**: the samples in the same window.
- **`eta_seconds(key, w)`**: a least-squares slope over the last 30 minutes of the window's samples.
  It needs at least 3 samples spanning at least 240 s (20 s in demo mode).
  It returns `None` (not enough data), `inf` (slope ≤ 0.0005%/min) or `(100 − pct) / slope`.
- **`last_windows()`**: the last known windows whose reset time is still in the future. They're used as the cached fallback.

### 6.3 Claude Code activity (`LogScanner`)
- It reads `~/.claude/projects/**/*.jsonl`, which Claude Code writes, CLI and desktop Code tab alike.
  - It keeps a byte offset per file and reads only appended data.
  - A partial last line is buffered until its newline arrives.
  - If a file shrinks, it's re-read from the start.
  - Files untouched for more than `KEEP_DAYS` (29) days are skipped, and older events are dropped from memory.
- Only lines containing `"usage"` are parsed.
  - Event key: `(message.id, requestId)`, which de-duplicates streamed repeats.
  - Tokens counted: `input + output + cache_creation`. **Cache reads are excluded**, otherwise they'd swamp everything.
- Stats:
  - today's tokens and replies, since local midnight
  - last 7 days
  - burn rate: tokens in the last 10 min ÷ 10
  - active sessions: distinct `sessionId` in the last 5 min
  - daily totals, and a 7×24 hour-by-day heatmap grid
  - `weeks[4]`, newest first: rolling 7-day blocks ending today, each `{heat[7][24], day_labels, day_dates, label, total}`. `heat`/`daily`/`day_labels` at the top level are `weeks[0]`, kept for existing callers.
- **claude.ai chat usage is not in these logs**, but it *is* counted in the plan %.

### 6.4 View model (`App.build_model`)
A plain dict, serialised to JSON and passed to `update(m)` in the pages. Keys:
- `theme`, `status`, `error`, `plan`
- `rows[]`: `{key, title, sub, short, pct, pct_i, level ok|warn|crit, pending, reset_short, reset_card}`. Session first, then weekly, then any model windows.
  - `pct_i = display_pct(pct)`: rounds half up, and **never shows 100 below 100%** (99.6 → 99).
  - `pending = True` when the window's reset time has passed but the last fetch is from before it. `pct_i` is then `None` (shown as "—").
  - `reset_short` is compact for the pill ("2h49m", "Sun 10PM", "updating"); `reset_card` is full ("Resets in 2h 49m", "Resets Sun 10:00 PM").
- `fresh{state fresh|stale|none, label, reason}`: from `freshness()`. **The single source of truth** for staleness in the pill, card, tray and taskbar.
- `forecast{text, level}` and `verdict`:
  - `verdict` is one of `reached | unknown | steady | hit | around_reset | under | stale`, from `forecast_verdict()`
  - `eta` is the seconds from now to 100% (`eta_seconds − age of the last fetch`)
- `timeline{points[[x,pct]], last_x, now_x, now_pct, proj[x,pct]|null, hit, start_label, end_label, level}`. x runs from 0 to 1 across the 5 hours. The actual data ends at `last_x`, which is the last fetch.
- `stats{today, msgs, week, burn, active, heat[7][24], day_labels, weeks[]}`: `weeks[i].total` is formatted ("6.6M") and `weeks[i].empty` marks a week with no events.
- `rows[i].prev_peak`: last weekly window's peak (`display_pct` rounding), `seven_day` only, from `History.prev_window_peak()` (cached per window and sample count, since the model is rebuilt every second).
- `updated`, `interval`, `view`
- `settings`: the config plus `autostart`
- `eta`

The **pill gets a slim subset**: `theme, status, error, rows, settings, fresh`, plus `stats.today` only when there are no rows.
Its JSON is compared with the last one sent, and the pill is only re-rendered when it changes.
The card gets the full model, only while open and only when it changed.

---

## 7. UI implementation

### Windows
Two pywebview windows created at start-up, both **hidden** at first:

| | Pill | Card |
|---|---|---|
| Title (used for `FindWindowW`) | `ClaudeUsagePill` | `ClaudeUsageCard` |
| Size (logical px) | per style (`PILL_SIZES`) | 360 × content height (JS reports it) |
| Flags | frameless, on_top, `focus=False`, `min_size=(100,30)` | frameless, on_top, `min_size=(200,150)` |
| Win32 styling | `WS_EX_TOOLWINDOW`, DWM rounded corners (attr 33), border colour (34), dark mode (20) | same |

- Position and size are set with **`SetWindowPos` in physical pixels**: logical size × `GetDpiForWindow/96`. Don't use pywebview's `move`/`resize`, which scale inconsistently.
- The card sits above the pill, right-aligned. If there's no room above, it goes below. It's clamped to the monitor.

### "Glass"
WebView2 inside WinForms **can't be truly see-through**. `transparent=True` renders opaque, and DWM Acrylic or Mica gives a flat grey. So the glass is faked:
1. `grab_region()` copies just the widget's rectangle with **GDI `BitBlt`**. That takes about 2 ms; `ImageGrab` took about 95 ms because it copies the whole desktop.
2. Before the pill's capture, `SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE)` hides the pill so it doesn't blur itself. It's restored right after.
3. The image is scaled to 1/6, blurred with Gaussian radius 7, saturation ×1.5, and brightness ×1.3 (light) or ×0.9 (dark). Then it becomes a JPEG data URL.
4. JS `setBackdrop(url)` sets it as `#bd`'s background, under a `#tint` gradient (`--tint-a/b`) plus a radial sheen.
   **There is no CSS `filter: blur`**: under software rendering it costs real CPU.
- The pill refreshes every 6 s while the card is closed, and only repaints when the image changed.
- The card captures once when opened, and again if its height changes.
- Known trade-off: the glass shows a slightly stale picture if things behind it move.

### Theme tokens (`glass.css`)
- `:root` holds the dark theme; `body.light` overrides it.
- Level colours:
  - ok: teal `#5eead4` → green `#4ade80`
  - warn: `#fde047` → `#f59e0b`
  - crit: `#fb7185` → `#ef4444`
  - light mode uses darker counterparts
- Brand orange `#e8743b`.
- `.l-ok`, `.l-warn` and `.l-crit` set `--c1/--c2/--cg`, which `.grad-text`, `.bar > i` and the SVG gradients consume.

### Python ⇄ JS bridge
- **Python → JS** with `window.evaluate_js(...)`, from the `loop` thread.
  - Pill: `update(m)`, `setBackdrop(url)`
  - Card: `update(m)`, `setBackdrop(url)`, `opened()` (runs the sweep and count-up animation), `closed()`
- **JS → Python** with `pywebview.api.<method>()`, through `class Api`. Its only attribute is `_app`.
  **pywebview exposes every public attribute of the js_api object**, so keep internals underscored.

| Method | Called from | Effect |
|---|---|---|
| `pill_down()` | pill mousedown (left) | Starts `_drag_loop`. A release without movement toggles the card. |
| `pill_menu()` | pill contextmenu | Native popup menu via NativeHost |
| `toggle_card()` / `close_card()` | card | Show or hide the card |
| `card_height(h)` | card after each render | Resize and reposition the card, re-capture glass |
| `set_view(v)` | card toggle button | `state.view = rings|bars` |
| `save_settings(dict)` | card settings | `Store.update_cfg` (type-checked against defaults); `autostart` handled specially; repositions if the pill style changed |
| `refresh()`, `open_data_folder()`, `quit()` | card / menu | |

### NativeHost (`winutil.NativeHost`)
A raw Win32 window, class `ClaudeUsageWidgetHost`, running on its own thread with its own `GetMessage` loop.
- **Taskbar button**: `WS_OVERLAPPEDWINDOW` at (−32000, −32000), shown with `SW_SHOWMINNOACTIVE`.
  - Clicking a minimised button sends `WM_SYSCOMMAND/SC_RESTORE`, which is swallowed and turned into `on_click`, so the window never actually restores.
  - `WM_CLOSE` calls `on_quit`.
- **Icon and progress**: on `WM_APP_UPDATE` it writes `taskbar.ico`, `LoadImageW`s it, then sends `WM_SETICON` and calls ITaskbarList3 `SetProgressState` (vtable 10) and `SetProgressValue` (vtable 9).
  These are re-applied on the `TaskbarButtonCreated` message, which fires when Explorer restarts.
- **Menus**: `WM_APP_MENU` runs `TrackPopupMenu(TPM_RETURNCMD)` at the cursor. The callback runs on a worker thread.
- **Second instance**: `WM_APP_SHOW` (0x8003) calls `show_card`.

---

## 8. Configuration (`%APPDATA%\ClaudeUsageWidget\config.json`)

| Key | Default | Meaning |
|---|---|---|
| `limits_refresh_seconds` | `120` | Anthropic poll interval. Minimum 30 is enforced. |
| `logs_refresh_seconds` | `5` | Log scan interval |
| `alert_thresholds` | `[75, 90]` | Toast thresholds. `[]` turns them off. |
| `alert_on_reset` | `true` | Toast when a window resets |
| `alert_on_forecast` | `true` | Toast when the ETA to 100% is under 30 min and before the reset |
| `dim_when_idle` | `true` | Fade the pill content unless hovered |
| `hide_in_fullscreen` | `true` | Hide during fullscreen apps on the pill's monitor |
| `taskbar_button` | `true` | NativeHost taskbar button. Applies on restart. |
| `show_model_limits` | `false` | Show `seven_day_*` per-model windows (e.g. Sonnet). No UI toggle yet. |
| `pill_style` | `"bars"` | `bars` / `ring` / `text` |
| `theme` | `"auto"` | `auto` / `dark` / `light` |
| `margin_px` | `8` | Gap from the work-area edge for the default position |

Unknown keys are dropped. Values must match the type of the default. Missing keys are added on start-up.

**`state.json`** holds `pill_pos [x,y]` (physical px), `view`, `autostart_initialized`, `alerts {"<key>|<reset_key>": [fired thresholds]}` (last 40 kept) and `last_windows`, which is used to detect resets.

---

## 9. Performance

Measured on the development machine, idle, card closed.

| Version | Private memory | Processes | Idle CPU (1 core) |
|---|---|---|---|
| Before tuning (v3.0) | 285 MB | 8 | 4.3% |
| Now | **~175 MB** | 7 | **~1.9%** (~0.2% of the machine) |

How:
- **WebView2 flags** (`WEBVIEW2_FLAGS` in `app.py`):
  - `--disable-gpu --disable-gpu-compositing` (GPU process 106 → 15 MB)
  - `--renderer-process-limit=1` (pill and card share one renderer)
  - `--disable-extensions --disable-background-networking`
  - `--disable-features=…BackForwardCache,Translate,msSmartScreenProtection`
  - plus pywebview's own `--allow-file-access-from-files`
- `private_mode=True`, so nothing is cached on disk. This also fixed stale-CSS bugs.
- GDI region grab, blur done in Python, repaint skipped when the image is unchanged, the 6 s glass interval.
- The slim pill payload, and the tray tooltip is only set when it changed.
- Unloading the hidden card was considered and **rejected**: with one shared renderer it saves little and would add delay when opening.

What's left: about 72 MB is Python plus .NET/WinForms, about 37 MB the WebView2 browser process, about 25 MB the renderer.

---

## 10. Install, launch and packaging

- **`Install.bat`** (double-click, safe to re-run):
  1. creates `.venv` (`py -3 -m venv`, falling back to `python -m venv`)
  2. `pip install -r requirements.txt`
  3. `install.py`
  4. starts the widget
- **`install.py`**:
  - generates `assets/app.ico`
  - creates `%APPDATA%\Microsoft\Windows\Start Menu\Programs\Claude Usage Widget.lnk` and `<Desktop>\Claude Usage Widget.lnk`
    - it uses the real Desktop folder, which may be in OneDrive
    - the target is `.venv\Scripts\pythonw.exe "…\claude_usage_widget.pyw"`, with the working directory set to the project
  - sets `PKEY_AppUserModel_ID = "ClaudeUsageWidget"` on each shortcut through `IPropertyStore`, using ctypes
  - writes the `Run` key to use `.venv`
  - `--remove` deletes the shortcuts and turns autostart off
- **Entry-point hand-off**: double-clicking the `.pyw` may run *any* registered Python. It re-launches through `.venv` if `.venv` exists. See §11 for why.
- **No frozen `.exe`** (PyInstaller) yet. It's possible, but pythonnet and WebView2 need care.

---

## 11. Design decisions and why

| Decision | Why | Rejected alternatives |
|---|---|---|
| Use the CLI's OAuth token and `/api/oauth/usage` for limits | It's the only source of the *real* % numbers that match the Claude app | Estimating from local logs (wrong, misses chat usage); scraping claude.ai with a browser session cookie (fragile, credential handling) |
| `curl.exe` for HTTP | Cloudflare can block Python's TLS fingerprint (a tip from niccolo-sabato/claude-usage-widget). Secrets go via stdin (`-H @-`) or a temp file, **never on the command line** | `urllib` (kept only as a fallback) |
| WebView2 (pywebview) UI | Gradients, glow, animations, SVG charts, easy theming. The user chose "Python + WebView2" over a C#/WinUI rewrite | tkinter + Pillow (v2: hit a visual ceiling, no animation); WinUI 3 (full rewrite) |
| Fake glass (blurred screenshot) | Real transparency or Acrylic doesn't work for WebView2 in WinForms (tested) | Transparent window, DWM backdrop types |
| A pill above the taskbar, not inside it | Windows 11 removed deskbands, so third-party apps can't embed in the taskbar | Deskband; Widgets Board (needs a C# Windows App SDK provider, as Wburn does) |
| A private `.venv` plus hand-off | Machines with several Pythons: the `.pyw` association (`pyw.exe` → install-manager 3.14) lacked `typing_extensions`, and the widget died silently | Relying on the global Python; PyInstaller (deferred) |
| Raw ctypes for Win32 and COM | No pywin32 or comtypes dependency | pywin32 |
| Glass design "A · Glass" | Picked by the user from three mock-ups (Glass, Neon, Fluent light) | n/a |
| 120 s default refresh | 60 s caused HTTP 429s during testing; reference apps use 3–5 min | 60 s |

---

## 12. Gotchas: read before changing things

1. **Handles via `FindWindowW` by window title.** Don't rename `ClaudeUsagePill` / `ClaudeUsageCard` without updating `app.py`.
2. **pywebview `min_size` defaults to (200,100)**, which silently made the 52 px pill 175 px tall. Always pass a small `min_size`.
3. **Size and position windows with `SetWindowPos` in physical pixels**, not pywebview's `move`/`resize`.
4. **WebView2 cached old CSS** before `private_mode=True`. Without private mode, CSS changes may not appear.
5. **No CSS `filter: blur()` or heavy CSS effects on the pill**: the GPU is disabled, so everything renders on the CPU. Small `drop-shadow`s are fine.
6. **`ImageDraw` doesn't alpha-blend** when drawing semi-transparent colours: it writes the RGBA straight in and leaves see-through holes. Use solid colours in icons.
7. **ctypes on 64-bit**: set `argtypes`/`restype` for every function that takes handles (HDC, HBITMAP, HWND…), or you'll get `OverflowError`s. See the GDI block in `winutil.py`.
8. **Windows timed waits pause during sleep.** Never do one long `Event.wait(n)` for scheduling. Use short steps against `time.time()`, as `LimitsFetcher.run` does.
9. **After wake, reset `limits.heartbeat`**, or the watchdog misreads time asleep as a hang. This was a real bug.
10. **Temp request files must be unique per call**: pid + thread + ns. Concurrent threads once deleted each other's file.
11. **Compare limit windows with `reset_key()`**, never raw `resets_at`.
12. **`Api` must have no public attributes** other than methods; pywebview walks them.
13. **JS naming**: the card once had both `let opened` and `function opened()`, which is a SyntaxError that kills the whole page script. Keep globals unique.
14. **SVG gradient ids must be unique per element** (`ig0`, `ig1`, `rg0`…). Duplicate ids make every element use the first one's colours.
15. **Don't hammer the real API while testing** (restarts poll immediately); use `--demo`. 429s last minutes.
16. **`.pyw` association ≠ your test Python.** Test launches through the shortcut or `Start-Process <file>.pyw`, not only `python file.pyw`.
17. **Pill payload is deliberately slim.** If the pill needs a new field, add it to `pill_model` in `App.push`.
18. A screen capture during testing **won't show the pill's own pixels** during its glass grab, which happens every 6 s. That's harmless.
19. **Never build all forecast texts eagerly.** `fmt_eta(inf)` raises. An eager dict of every verdict's text once crashed `build_model` on flat usage. Only format the ETA in the `hit` branch.
20. **Staleness has one rule: `freshness()`.** Don't add status checks in JS (the old pill only checked `status === 'error'`, so "expired" looked fresh). Read `m.fresh.state`.
21. **The ETA is measured from the last sample, not from now.** Use `eta − (now − updated)` for anything shown to the user, and start the projection at `last_x`.
22. **Don't trigger a refresh on every tick.** Anything that calls `refresh_now()` from `loop` or `check_alerts` must be limited (see `reset_refreshed`).
23. Tests use `App.__new__` with fake limits, tray and store (`tests/test_refinement.py`). Keep `build_model` and `check_alerts` free of window/Win32 calls apart from `W.autostart_enabled()`, which is read-only.

---

## 13. Testing

- **Unit tests**: run `.venv\Scripts\python.exe -m unittest discover -s tests -v`. **42 tests**, about 0.05 s.
  - `tests/test_history_views.py` (9) covers heatmap week bucketing and paging data (boundaries, 4-week cut-off, the 7-day tile unchanged, formatted totals) and last week's peak (only the previous window, reset jitter, cache refresh, never a fake 100, hidden while pending).
  - `tests/test_refinement.py` (19) covers:
    - display rounding and coarse ETAs
    - the freshness states, including expired and rate-limited
    - every forecast verdict
    - API values reaching the model unchanged
    - a window that reset since the last fetch
    - stale data pausing the forecast and projection
    - **forecast text = chart intersection**
    - not hitting before reset, flat usage, thin history
    - alerts: once per window, independent session/weekly, surviving a restart, none from cached values, the forecast alert once, the reset alert only when timely, one refresh per reset
    - an unrecognised response keeping the last values
  - `tests/test_core.py` covers:
  - ISO parsing
  - `reset_key` jitter tolerance
  - usage-response parsing, including hidden and visible model limits and null windows
  - forecast: not enough data, a steady rise (+1%/min → 60 min), flat → inf, other windows ignored
  - `last_windows` skipping expired windows
  - timeline projection, hit and no-hit
  - `fmt_dur`, `fmt_tokens`, `level`
  - LogScanner de-duplication, cache-read exclusion, partial lines and active sessions
- **Simulated outage**: set `CUW_DEMO_OFFLINE=1` before starting demo mode, and after 25 s the demo fetcher reports "Offline". That lets you check the stale UI (muted values, amber footer, paused forecast, no projection) for real.
- **Demo mode**: `.venv\Scripts\pythonw.exe claude_usage_widget.pyw --demo`.
  The 5-hour % rises about 6%/min, the history is seeded so the forecast and timeline show right away, nothing is written to `history.jsonl`, and the real API is never called.
- **Manual or visual checks used so far**:
  - launch via `Start-Process` and read `startup.log`
  - find the windows with `FindWindowW`, click with `SetCursorPos` and `mouse_event`, capture with `ImageGrab`
  - measure memory by summing `PrivateMemorySize64` over the process tree
  - measure CPU with `TotalProcessorTime` deltas over 30 s
- **Launch matrix** (all passed on 2026-11-15):
  - Start menu shortcut
  - shortcut while running (opens the card)
  - `.pyw` double-click through the install-manager Python (hands off)
  - `Install.bat` from scratch

---

## 14. Known limitations and risks

- **Undocumented endpoint and client id.** Anthropic can change or block them, and it would break silently apart from the "error" state. Check `widget.log` first.
- It only covers the account the **Claude Code CLI** is signed in to. There's no multi-account support.
- Glass is a periodic snapshot, so it's not a live blur.
- Fullscreen detection only checks the pill's monitor and the foreground window.
- Not verified:
  - multiple monitors
  - a taskbar on another edge
  - an auto-hiding taskbar
  - 100% / 125% DPI (only 175% tested)
- The hidden card window still exists, sharing the renderer. The first `opened()` animation runs while the card fades in.
- Toasts come from pystray's tray balloon. They work on Windows 11, but there's no click action.
- The `legacy/` folder isn't maintained.

---

## 15. Ideas / backlog
- Settings toggle and third ring for **per-model limits** (Sonnet); the data path already exists (`show_model_limits`).
- **Weekly history chart**: the weekly % over past weeks. `history.jsonl` already has the data. (Only last week's peak is shown today, as one line on the weekly row.)
- Global **hotkey** (for example Win+Shift+C) to open the card.
- Verify and fix multi-monitor, other taskbar edges, auto-hide taskbar, and other DPI scales.
- Optional **PyInstaller** one-folder build plus an Inno Setup installer, for a Python-free install.
- Toast click → open the card. Use a proper toast API instead of the tray balloon.
- Show claude.ai **extra usage / spend** if the endpoint exposes it (`extra_usage`).
- A "copy diagnostics" button that zips the logs and config.

---

## 16. Change history

### Requirements gathering
The user asked for a taskbar widget showing Claude usage, updated live, and asked to be questioned rather than given a one-shot build. Decisions:
- **Data**: the real plan limits from the Claude app's usage screen (user's screenshot) → Claude Code CLI sign-in.
- **Form**: a pill above the taskbar plus a tray icon.
- **Metrics**: the 5-hour window, today/week, live activity.
- **Extras**: start with Windows, limit alerts, auto-hide in fullscreen, dim when idle.
- **Style**: match the Windows theme. Plan: Pro. Mostly Claude Code.
- **References** the user supplied: TokenEater (screenshot: bold cards, green, rings), `niccolo-sabato/claude-usage-widget` (taskbar strip, progress overlay, curl), and Wburn (Windows Widgets Board, offline caching).
- **Later picks**: bars + rings toggle, green with amber/red, taskbar progress overlay, drag and remember position, a time-to-limit forecast. Then: Python + WebView2, animations and gradients, the session timeline, history and heatmap, pill style options, mock-ups first → **A · Glass**.

### v1: tkinter, single file (not committed)
- A pill above the clock and a tray icon (pystray). It read Claude Code logs for tokens and the CLI token for limits.
- Alerts, autostart, fullscreen hide, hover dim, a theme-following details panel.
- Fixes: borderless windows don't get focus events, so a mouse-poll replaced `<FocusOut>` for closing the panel; a sign-in message that overflowed.

### v2: tkinter + Pillow, TokenEater style (`legacy/claude_usage_widget_v2.pyw`)
- Moved the project to `Downloads\Projects\ClaudeUsageWidget`.
- A card antialiased by Pillow, a bars/rings toggle, green accent, dragging, a taskbar button with an ITaskbarList3 progress overlay, a pace forecast, `curl` for HTTP.
- Fixes:
  - the forecast kept resetting because of `resets_at` jitter → `reset_key`
  - the taskbar button showed the Python icon → AppUserModelID
- First real limits shown after the user ran `claude` → `/login` (Session 18%, Weekly 22%).

### v3: WebView2 "Glass" (commit `16d9129`, 2026-11-12)
- Restructured into the `cuw/` package and the HTML/CSS UI. Fake-glass backdrops. NativeHost. History plus timeline. Heatmap. In-card settings. Three pill styles.
- Fixes:
  - a JS name clash (`opened`)
  - duplicate SVG gradient ids
  - pywebview `min_size` inflating the pill
  - a long pill message wrapping (now ellipsised)
  - "now" overlapping the timeline labels
- **Rate limiting**: 429s from rapid restarts. Default refresh 60 → 120 s, cached last values on start-up and on errors, "Rate-limited, retrying" message.
- **Overnight stall**: after sleep the fetch loop stopped retrying. Fixed with sleep-safe stepped waits, wake detection that refreshes at once, and a watchdog with stack-dump logging.
- Git repo created; pushed to a private GitHub repo with `gh`.

### `dde5610` (2026-11-12): performance, theme, tests
- Memory 285 → 175 MB and idle CPU 4.3% → 1.9% of a core. Covered by the WebView2 flags, private mode, GDI grab, Python-side blur, skip-unchanged, slim pill payload and tooltip de-duplication (§9).
- Theme setting: Auto / Dark / Light. Brighter light-mode glass.
- Found that WebView2's disk cache had hidden earlier CSS changes; fixed by private mode.
- Added `tests/test_core.py` (14 tests).

### `5fcfddc` (2026-11-15): launchable app, silent start failure fixed
- **Root cause of "double-clicking shows nothing"**: Windows opens `.pyw` with `pyw.exe` → the Python install manager's 3.14. That copy shared the user site-packages (so pywebview loaded) but lacked `typing_extensions`, and crashed with no console.
- Added:
  - a private `.venv` with pinned requirements
  - the entry-point hand-off to `.venv`
  - `startup.log` plus error message boxes
  - a second launch opens the card (`WM_APP_SHOW`)
  - `Install.bat`/`install.py`, the app icon, Start menu and Desktop shortcuts with the AppUserModelID, autostart through `.venv`
- Fixed (found in the logs from the first real sleep/wake test, which also confirmed wake detection works):
  - the watchdog firing after every sleep
  - concurrent threads clobbering the temp request file
- Fixed an icon artifact (ImageDraw alpha).

### This document (2026-11-15)
- Added `docs/ARCHITECTURE.md` and a short root `CLAUDE.md` for coding agents.

### Trust-and-clarity refinement (2026-11-15, from `claude_usage_widget_final_prd.md`)
The goal was an audit-first pass with minimal changes; no new features or sections.
- **Defects fixed:**
  - a sign-in-expired (401/403) state showed old values as if fresh
  - cached values whose window had already reset showed the old % with a "1m" countdown
  - the chart drew stale data as measured "now", and the projection and ETA were measured from now instead of the last sample
  - `check_alerts` called `refresh_now()` every second after a reset while the API lagged (a 429 risk)
  - an unrecognised API response became "ok" with no rows
  - the forecast alert could repeat after a restart
  - reset toasts arrived hours late after sleep
  - 99.6% showed as "100%", and banker's rounding applied
- **Clarity changes:**
  - one `freshness()` rule; stale values are muted everywhere, with an amber footer like "Last updated … · reason"
  - coarse ETAs plus an "around reset time" band
  - PRD wording for the no-estimate and steady states
  - an Actual/Projected legend; the projection is thin, dashed and grey, starts at the last fetch and is hidden when stale or unknown
  - "Resets in 2h 49m" / "Resets Sun 10:00 PM" copy
  - activity tiles with units and a hover note on what's counted
  - the ring pill uses short labels so it doesn't wrap
- **Unchanged:** the authentication and refresh flow, curl, `reset_key()`, backoff, sleep/wake handling, the watchdog, threshold defaults (75/90), the visual identity and the pill sizes.
- **Added:** `tests/test_refinement.py` (19 tests) and the demo-only `CUW_DEMO_OFFLINE` switch. Two bugs in the new code were caught by those tests before shipping: the eager-forecast crash and "~60 min".

### Past activity views (2026-11-26)
Added at the user's request after weighing them against the glance-and-go principle; both reuse existing UI rather than adding sections.
- **Heatmap week paging**: ‹ › on the existing heatmap pages back through 4 weeks of Claude Code activity with a per-week token total. `LogScanner.KEEP_DAYS` went from 8 to 29 days.
- **Last week's peak**: one muted line on the weekly row, from `history.jsonl` (`History.prev_window_peak`). The demo seeds a previous weekly window peaking at 81.6%.
- Added `tests/test_history_views.py` (9 tests, 42 total).

### Native Rust port, phase 1 (2026-11-26, branch `rust-port`)
The user chose a fully native rewrite: Direct2D + DirectComposition, real Windows 11 acrylic, in `rust/`, delivered in phases. The plan, measurements and lessons are in `rust/PLAN.md`.
- **Phase 1 delivers:** data, model, pill, tray and menus. It uses the same data files, the same single-instance mutex and the same host window class as the Python app, so the two never run together and switching keeps history, alert de-duplication and position.
- **Measured:** 12.2 MB private, 0.1% of one core idle, a 515 KB `.exe`, against about 205 MB for the Python app. Rendering uses the WARP software rasterizer, because a hardware D3D device alone costs about 63 MB.
- The Python app stays the daily driver until the card (phase 2) and the rest (phase 3) reach parity.

### Native Rust port, phase 2 (2026-11-26, branch `rust-port`)
- **The details card is ported:** bars and rings views, forecast, timeline, last week's peak, the stale state, and the open/close behaviour.
- **Cost:** 0.31% of one core with the card open, against 0.05% closed.
- **How it got there:** the timeline halo is a DirectComposition layer with a compositor-run animation. The card redraws only on visible change; its redraw key leaves out the per-second `eta` and rounds "now" to whole chart pixels.
- **Focus:** the pill is activatable, so the card can take focus for Esc and close-on-deactivate.
- **Test switch:** demo-only `CUW_DEMO_STILL=1` (in `rust/`) holds values steady for performance checks.
- **Details:** `rust/PLAN.md`.

### Native Rust port, phase 3: parity (2026-11-26, branch `rust-port`)
- **Ported:**
  - `LogScanner`, the stat tiles and the 4-week heatmap (paging, tooltip)
  - the settings panel
  - alerts (same `state.json` shapes, so switching apps never repeats one)
  - the taskbar button with `ITaskbarList3` progress
  - autostart
  - a self-installer: `ClaudeUsageWidget.exe --install` / `--uninstall`, with shortcuts carrying the AppUserModelID
- **Measured under the same conditions:**

  | | Python app | Rust app |
  |---|---|---|
  | Memory | ~181 MB, 8 processes | 14.6 MB, 1 process |
  | CPU, card closed (one core) | 1.69% | 0.44% |
  | First log scan | 2.8 s | 0.6 s |

- `rust/PLAN.md` has the details. Going back to Python is `Install.bat`.

### Audit fixes after the Rust switch-over (2026-11-26)
- **Quit:** Quit and the installer's close request now exit properly. Before, the process hung while destroying the graphics objects at thread exit. It now terminates right after the message loop, and the host handles `WM_CLOSE`.
- **Second launch:** opens the card rather than toggling it.
- **No writes from demo or tests:** `--demo` and the tests never write `config.json` or `state.json`. The alert tests used to overwrite the real `state.json`.
- **Pill right-click:** keeps an open card open.
- **Logging:** one line per log entry.
- **CPU on real data:** halved, 1.28% → 0.64%, by caching the history series.
- **Ported:** the card open animation (compositor), the stale footer tooltip, the exe icon and version info, and menus that follow the widget's theme.
- **Docs:** README, this header and `CLAUDE.md` now describe the Rust app as the installed one.
