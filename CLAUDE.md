# Claude Usage Widget: agent notes

A Windows 11 taskbar widget that shows live Claude plan limits: the 5-hour session % and the weekly %.
It's a frosted-glass pill above the taskbar, plus a details card with rings or bars, a pace forecast, a 5-hour timeline, Claude Code activity, a heatmap and settings.
Python 3.14 + pywebview (WebView2) + plain HTML/CSS/JS + ctypes Win32.
It's personal use for one user (Claude Pro, mostly Claude Code).

**Read `docs/ARCHITECTURE.md` before non-trivial changes.** It has:
- the full feature list
- the architecture and threads
- the data sources
- the JS⇄Python bridge
- config keys
- design decisions
- gotchas
- the change history

## Commands
- Run: `.venv\Scripts\pythonw.exe claude_usage_widget.pyw` (add `--demo` for fake data that never calls the API; also set `CUW_DEMO_OFFLINE=1` and demo mode goes "offline" after 25 s, so you can check the stale UI)
- Tests: `.venv\Scripts\python.exe -m unittest discover -s tests -v`
- Install / repair the app (venv, shortcuts, icon, autostart): `Install.bat`
- Stop it: tray icon → Quit, or the right-click pill menu → Quit
- Logs: `%APPDATA%\ClaudeUsageWidget\widget.log`, plus `startup.log` for launch problems

## Map
- `claude_usage_widget.pyw`: entry point. Hands off to `.venv`, single instance, start-up error box.
- `cuw/app.py`: windows, main loop, view model (`build_model`), timeline, alerts, JS API (`class Api`).
- `cuw/data.py`: Anthropic usage fetch (curl), token refresh, `History` + forecast, `LogScanner`.
- `cuw/winutil.py`: Win32 via ctypes. Placement, glass capture (GDI), autostart, `NativeHost` (taskbar button, progress, menus).
- `cuw/web/`: `pill.html`, `card.html`, `glass.css` (no framework, no build step).
- `install.py` / `Install.bat`: app install. `tests/`: unit tests.
- `rust/`: native Rust port in progress (branch `rust-port`). Read `rust/PLAN.md` first; the Python app is still the daily driver.

## Rules that bite (details in docs §12)
- Test with `--demo`. Restarting against the real API polls immediately and triggers HTTP 429s.
- Test launches the way the user launches: the shortcut, or `Start-Process` on the `.pyw`. The `.pyw` association runs a *different* Python.
- Always use `.venv`. Add new packages to the pinned `requirements.txt`.
- Window handles are found by title (`ClaudeUsagePill` / `ClaudeUsageCard`).
- Size and position windows with `SetWindowPos` in physical px.
- Always pass a small pywebview `min_size`.
- The GPU is disabled. Don't use CSS `filter: blur` or heavy effects; the glass is pre-blurred in Python.
- Compare limit windows with `reset_key()`, never raw `resets_at`, which jitters.
- Scheduling waits must be short steps against wall-clock time, because waits pause during sleep. Reset `limits.heartbeat` on wake.
- `Api` must expose only methods; pywebview walks its public attributes.
- Keep JS globals and SVG gradient ids unique.
- If the pill needs a new field, add it to `pill_model` in `App.push`.
- Product principle: **glance and go; trust over extra information.** Session % and weekly % are the story. Don't add analytics to the pill or new card sections without a demonstrated need.
- Staleness has one rule, `freshness()`; read `m.fresh.state` in JS. The ETA is measured from the last fetch (`m.eta` is already "from now"). Actual and projected data must stay visually distinct.
- Secrets (the OAuth token) never go on a command line, in the log or in the repo. Credentials live in `~/.claude/.credentials.json`.
- Keep `docs/ARCHITECTURE.md` updated, especially the *Gotchas* and *Change history* sections, when behaviour changes.
