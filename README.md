# Claude Usage Widget (Windows)

A frosted-glass widget above your taskbar that shows your Claude plan limits live: **Session** (5-hour) and **Weekly**. They're the same numbers as the Claude app's usage screen.
Click it for the full card. The card has rings or bars, a pace forecast, a 5-hour timeline, Claude Code activity with a 4-week heatmap, and settings.

The app is a **native Rust program** (`rust/`): one ~770 KB `.exe`, about 15 MB of memory, and under 1% of one CPU core. The original **Python + WebView2** version (`cuw/`) is kept as a fallback.

For how it all works (architecture, design decisions, gotchas and change history), see [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md). The Rust port's plan, measurements and lessons are in [rust/PLAN.md](rust/PLAN.md).

## Setup
1. Sign the Claude Code CLI in to your account (one time): run `claude` in a terminal, then type `/login`.
2. Build it. This needs Rust and the Visual Studio C++ build tools.
   ```
   cd rust
   cargo build --release
   ```
3. Install it:
   ```
   target\release\ClaudeUsageWidget.exe --install
   ```
   This does four things:
   - copies the app to `%LOCALAPPDATA%\Programs\ClaudeUsageWidget`
   - adds **Claude Usage Widget** to the Start menu and the Desktop
   - turns on Start with Windows
   - closes any running copy (Python or Rust) and starts the new one

From then on, open it like any app, from the Start menu or the Desktop, and pin it to the taskbar if you like.
Opening it while it's already running pops its card open.

- **Update after changing the code:** run `cargo build --release`, then `--install` again. It replaces the running copy.
- **Uninstall:** run `ClaudeUsageWidget.exe --uninstall` to remove the shortcuts and autostart, then delete the program folder.
- **Go back to the Python version:** double-click `Install.bat`. It rebuilds `.venv` and points the shortcuts and autostart at Python.
- **Try it with fake numbers:** `target\release\ClaudeUsageWidget.exe --demo`. Demo mode never changes your saved settings. For checks it also takes:
  - `CUW_DEMO_OFFLINE=1`: goes "offline" after 25 s, to show the stale look
  - `CUW_DEMO_STILL=1`: keeps the values steady

### Troubleshooting
- **Nothing appears:** look at `%APPDATA%\ClaudeUsageWidget\widget.log`. A start-up failure also shows an error box.
- **Numbers show grey with an amber dot:** they're the last known values. Hover the pill to see why ("Offline", "Sign-in expired", …).
  If it says the sign-in expired, run `claude` and `/login` again.

## Using it
- **Open the card:** click the pill, its taskbar button or the tray icon. Click elsewhere or press Esc to close it.
- **Drag the pill** anywhere, including onto the taskbar. It remembers the spot.
- **Right-click the pill** for a menu: show details, refresh, pill style, reset position, open data folder, quit.
- **Card buttons:** ◯ / ☰ switches between rings and bars, and ⚙ opens settings.

## What's in the card
- **Session / Weekly %** with reset times. The colour is teal→green, amber from 75% and red from 90%, and the ring pulses near the limit. The weekly row also shows last week's peak.
- **Pace forecast:** for example "At this pace, limit in ~1h 15m", "…you'll stay under the limit until reset" or "Not enough recent activity to estimate". It's worked out from the last 30 minutes, and it's paused whenever the data is stale.
- **5-hour timeline:** the actual usage line (ending at the last update), a dashed **projected** line to where you'd hit 100%, and a "now" marker.
- **Claude Code activity:** tokens today (with responses), over 7 days, and per minute right now (with active sessions).
- **Heatmap:** when you used Claude Code, by day and hour. Use ‹ › to page back up to 4 weeks, and hover a cell for its numbers.

## Settings (⚙ in the card)
- Pill style: bars, ring or text
- Theme: Auto (follows Windows), Dark or Light
- Start with Windows
- Alerts at 75% and 90%, on reset, and when you're on pace to hit the limit
- Fade the pill when idle
- Hide during fullscreen apps
- Taskbar button with progress (takes effect after a restart)
- Refresh interval

Everything is stored in `%APPDATA%\ClaudeUsageWidget\`, and both versions share it:
- `config.json`: settings
- `state.json`: position, view and which alerts were already sent
- `history.jsonl`: limit history for the timeline and forecast
- `widget.log`: log file, rotated automatically

## How it works
| Piece | Source |
|---|---|
| Plan limits | The Claude Code CLI's sign-in token (`~/.claude/.credentials.json`) is used to ask Anthropic for the data `/usage` shows. Requests go through Windows' `curl`, with the token passed via stdin and never on a command line. This endpoint is undocumented and could change. It's polled every 2 minutes by default and backs off if rate-limited. |
| Activity and heatmap | Read-only parsing of Claude Code logs in `~/.claude/projects`. claude.ai chats aren't in these logs, but they *are* counted in the plan %. |
| Drawing | Direct2D and DirectWrite in software (WARP), shown through DirectComposition over Windows 11's live **acrylic** blur. The timeline halo and the card's open animation are compositor animations, so they cost no CPU. |

## Footprint
Measured on the development machine:

| | Rust app | Python app |
|---|---|---|
| Memory | ~15 MB, 1 process | ~181 MB, 8 processes |
| CPU, card closed (one core) | ~0.6% while Claude Code is busy (log scanning), less when it's idle | ~1.7% |

## Tests
```
cd rust && cargo test                              (Rust: 28 tests)
.venv\Scripts\python.exe -m unittest discover -s tests -v   (Python version)
```

## Project layout
```
rust/                     the app (native Rust)
  src/main.rs             entry: single instance, --demo / --install / --uninstall
  src/app.rs              windows, 1 s tick, menus, card wiring
  src/model.rs            view model: freshness, forecast, timeline (pure, tested)
  src/data/               Anthropic limits (curl), history + forecast, log scanner
  src/ui/                 Direct2D drawing: pill, card, activity + settings, theme, animation
  src/tray.rs, taskbar.rs tray icon + notifications, taskbar button + progress
  src/alerts.rs           threshold / reset / pace alerts
  src/install.rs          self-installer
  PLAN.md                 port plan, measurements, lessons
Install.bat / install.py  Python version setup (.venv, shortcuts, autostart)
claude_usage_widget.pyw   Python version entry point
cuw/                      Python version (pywebview + HTML/CSS)
tests/                    Python unit tests
assets/app.ico            app icon (also compiled into the .exe)
docs/ARCHITECTURE.md      full architecture + project context
CLAUDE.md                 short notes for coding agents
```
