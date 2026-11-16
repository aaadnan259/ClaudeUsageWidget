# Claude Usage Widget (Windows)

A frosted-glass widget above your taskbar that shows your Claude plan limits live: **Session** (5-hour) and **Weekly**, the same numbers as the Claude app's usage screen.
Click it for the full card. The card has animated gradient rings or bars, a 5-hour timeline with a pace projection, Claude Code activity, a usage heatmap and settings.

## Setup
1. Sign the Claude Code CLI in to your account (one time): run `claude` in a terminal, then type `/login`.
2. Double-click **`Install.bat`**. It does four things:
   - creates the widget's own Python environment (`.venv`) with the exact package versions it needs
   - adds a **Claude Usage Widget** app to the Start menu and Desktop, with its own icon
   - turns on Start with Windows
   - starts the widget

From then on, open it like any app, from the Start menu or the Desktop, and pin it to the taskbar if you like.
Opening it while it's already running pops its card open.
Run `Install.bat` again any time to repair it.
To uninstall: run `.venv\Scripts\python.exe install.py --remove`, then delete the folder.

Try it with fake numbers: `.venv\Scripts\pythonw.exe claude_usage_widget.pyw --demo`

### Troubleshooting
- **Nothing appears:** look at `%APPDATA%\ClaudeUsageWidget\startup.log`. Start-up problems now also show an error box instead of failing silently.
- **Double-clicking `claude_usage_widget.pyw` directly** works too: whichever Python Windows uses, it hands off to `.venv`. This matters because Windows can have several Pythons installed, and the one it picks for `.pyw` files may not have the widget's packages.

## Using it
- **Click the pill** (or its taskbar button, or the tray icon) to open the card. Click elsewhere or press Esc to close it.
- **Drag the pill** anywhere, including onto the taskbar. It remembers the spot.
- **Right-click the pill** for a menu: refresh, pill style, start with Windows, reset position, quit.
- **Card buttons:** ◯ / ☰ switches between rings and bars. ⚙ opens settings.

## What's in the card
- **Session / Weekly %** with reset times. The gradient is teal→green, amber from 75%, red from 90%, and the ring pulses near the limit.
- **Pace forecast:** "At this pace: limit in ~1h20". It's worked out from how fast your % has risen over the last 30 minutes.
- **5-hour timeline:** your usage line across the current window, a "now" marker, and a dotted projection showing where you'd hit 100%.
- **Claude Code activity:** tokens today, over 7 days and per minute right now, plus active sessions.
- **Heatmap:** when you used Claude Code over the last 7 days, by hour. Hover a cell for its numbers.

## Settings (⚙ in the card)
- Pill style: bars, ring or text
- Theme: Auto (follows Windows), Dark or Light
- Start with Windows
- Alerts at 75% and 90%, on reset, and when you're on pace to hit the limit
- Fade the pill when idle
- Hide during fullscreen apps
- Taskbar button with progress overlay
- Refresh interval

Everything is stored in `%APPDATA%\ClaudeUsageWidget\`:
- `config.json`: settings
- `state.json`: position and view
- `history.jsonl`: limit history for the timeline
- `widget.log`: log file, rotated automatically

## How it works
| Piece | Source |
|---|---|
| Plan limits | The Claude Code CLI's sign-in token (`~/.claude/.credentials.json`) is used to ask Anthropic for the data `/usage` shows. Requests go through Windows' `curl`, with the token passed via stdin. This endpoint is undocumented and could change. Polled every 2 minutes by default; it backs off if rate-limited. |
| Activity and heatmap | Read-only parsing of Claude Code logs in `~/.claude/projects`. claude.ai chats aren't in these logs, but they *are* counted in the plan %. |
| Glass look | WebView2 can't be truly see-through. Instead, the widget captures what's behind it, blurs and tints it, and uses that as its background. It refreshes every few seconds and hides itself from the capture so it never blurs its own pixels. |

## Footprint
About 175 MB of private memory and around 2% of one CPU core when idle.
To keep it there:
- WebView2 runs without GPU acceleration, with a single shared renderer and in private mode (no disk cache).
- The glass backdrop copies only the pill's rectangle of the screen via GDI and is pre-blurred in Python.
- The pill is only repainted when something it shows changes.

## Tests
```
python -m unittest discover -s tests -v
```
They cover:
- parsing Anthropic's response
- reset-time jitter
- the pace forecast
- the timeline projection
- number formatting
- log scanning, including duplicates and half-written lines

## Project layout
```
Install.bat / install.py  one-click setup: .venv, shortcuts, icon, autostart
claude_usage_widget.pyw   entry point (hands off to .venv, start-up error reporting)
assets/app.ico            app icon
cuw/app.py                windows, update loop, alerts, view model
cuw/data.py               Anthropic limits, history + forecast, log scanner
cuw/winutil.py            Win32: placement, glass capture, autostart, taskbar button/menu
cuw/tray.py               tray icon + notifications
cuw/web/                  pill.html, card.html, glass.css
tests/                    unit tests (stdlib unittest)
legacy/                   previous tkinter version
```
