# Claude Usage Widget: agent notes

A Windows 11 taskbar widget that shows live Claude plan limits: the 5-hour session % and the weekly %.
It's a frosted-glass pill above the taskbar, plus a details card with rings or bars, a pace forecast, a 5-hour timeline, Claude Code activity, a heatmap and settings.
**The installed app is the native Rust version in `rust/`** (Direct2D + DirectComposition + Win32 via the `windows` crate). Read `rust/PLAN.md` before changing it.
The original Python 3.14 + pywebview (WebView2) version in `cuw/` is kept as a fallback, and `Install.bat` switches back to it. Both share `%APPDATA%\ClaudeUsageWidget\`.
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
- **Rust (the app):**
  - Build: `cd rust && cargo build --release`
  - Demo: `target\release\ClaudeUsageWidget.exe --demo`. Demo mode never writes config/state. `CUW_DEMO_OFFLINE=1` goes stale after 25 s; `CUW_DEMO_STILL=1` holds values steady for performance checks. `CUW_DEMO_SCENARIO=warn|crit|pending` shows 80% / 93% / the "reset · updating" state.
  - Tests: `cargo test`
  - Install or update: `target\release\ClaudeUsageWidget.exe --install`. It replaces the running copy, updates the shortcuts and autostart, and restarts. `--uninstall` removes them.
- **Python (fallback):**
  - Run: `.venv\Scripts\pythonw.exe claude_usage_widget.pyw` (add `--demo`)
  - Tests: `.venv\Scripts\python.exe -m unittest discover -s tests -v`
  - Install / switch back: `Install.bat`
- **Stop it:** right-click the pill → Quit, or the tray icon → Quit.
- **Logs:** `%APPDATA%\ClaudeUsageWidget\widget.log`. Rust lines end in `[rust]`.
- **Sending `WM_CLOSE` to the host window from a script:** use Python ctypes, not PowerShell. PowerShell passes `$null` as an empty string, so `FindWindowW(class, "")` misses the window.
- **Before testing against the real app:** an installed instance holds the single-instance lock, so a `--demo` launch just opens the real card. Quit or stop the installed one first.

## Map
- `claude_usage_widget.pyw`: entry point. Hands off to `.venv`, single instance, start-up error box.
- `cuw/app.py`: windows, main loop, view model (`build_model`), timeline, alerts, JS API (`class Api`).
- `cuw/data.py`: Anthropic usage fetch (curl), token refresh, `History` + forecast, `LogScanner`.
- `cuw/winutil.py`: Win32 via ctypes. Placement, glass capture (GDI), autostart, `NativeHost` (taskbar button, progress, menus).
- `cuw/web/`: `pill.html`, `card.html`, `glass.css` (no framework, no build step).
- `install.py` / `Install.bat`: app install. `tests/`: unit tests.
- `rust/`: native Rust port at feature parity (branch `rust-port`). Read `rust/PLAN.md` first. Build with `cargo build --release`, run `--demo`, test with `cargo test`; `--install` makes it the installed app (Install.bat switches back to Python).
  - When testing, back up `config.json`/`state.json` with `Copy-Item`, never PowerShell `Set-Content -Encoding utf8` (a BOM makes the Python app reset settings).

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
