# Claude Usage Widget (Windows)

A frosted-glass widget above your taskbar that shows your Claude plan limits live: **Session** (5-hour) and **Weekly**, the same numbers as the Claude app's usage screen.
Click it for the full card. The card has animated gradient rings or bars, a 5-hour timeline with a pace projection, Claude Code activity, a usage heatmap and settings.

## Setup
1. Install dependencies (one time): `python -m pip install --user -r requirements.txt`
   This installs pywebview, which uses Microsoft Edge WebView2. WebView2 is built into Windows 11.
2. Sign the Claude Code CLI in to your account (one time): run `claude` in a terminal, then type `/login`.
3. Start it: `pythonw claude_usage_widget.pyw`. It adds itself to *Start with Windows*.

Try it with fake numbers: `pythonw claude_usage_widget.pyw --demo`

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

## Project layout
```
claude_usage_widget.pyw   entry point
cuw/app.py                windows, update loop, alerts, view model
cuw/data.py               Anthropic limits, history + forecast, log scanner
cuw/winutil.py            Win32: placement, glass capture, autostart, taskbar button/menu
cuw/tray.py               tray icon + notifications
cuw/web/                  pill.html, card.html, glass.css
legacy/                   previous tkinter version
```
