# Rust native port: plan

Goal: the same widget, much lighter. The Python app uses about **205 MB**: roughly 85 MB for Python plus pythonnet, and 120 MB for WebView2. The target is **under 30 MB** for a single `.exe`, with no Python, `.venv` or browser engine. Nothing the user sees is dropped: every feature, every honesty rule (freshness, pending, forecast wording) and the look are kept.

## Status
**Phase 1 is done (2026-11-26).** The data layer, view model, pill (3 styles), tray, menus, drag, idle dim, stale/pending states, fullscreen hiding, wake handling and watchdog all work, and 20 Rust tests pass. It has been checked in `--demo`, with `CUW_DEMO_OFFLINE`, and once against the real API.

| Measured (release, idle) | Python app | Rust phase 1 |
|---|---|---|
| Memory | ~205 MB across 7 processes (85 MB Python, ~120 MB WebView2) | **12.2 MB private**, 1 process |
| CPU | ~0.2% of the machine | **0.1% of one core** |
| Disk | `.venv` + Python | **515 KB `.exe`** |

Learned in phase 1:
- **The GPU device costs 63 MB.** A hardware D3D11 device loads the graphics driver (76 MB private versus 12.5 MB). Rendering uses **WARP**, the software rasterizer; set `CUW_GPU=hw` to compare. A widget this small doesn't need the GPU.
- **Acrylic works** on a `WS_POPUP` + `WS_EX_NOREDIRECTIONBITMAP` window with DirectComposition content (`DWMSBT_TRANSIENTWINDOW` + `DwmExtendFrameIntoClientArea(-1)`).
- **Overloaded COM methods:** `ID2D1Factory1::CreateStrokeStyle` and `ID2D1DeviceContext::CreateGradientStopCollection` take the newer structs, so the code calls the base-interface versions explicitly.
- **Win32 menus follow the dark theme** via uxtheme `SetPreferredAppMode(AllowDark)` (ordinal 135).
- **Drag uses the cursor position at button-up**, because move messages coalesce on fast drags.

**Phase 2 is done (2026-11-26).** The details card:
- the header (plan chip, view toggle)
- bars and rings views, with the sweep, fill and count-up animations on open
- last week's peak
- the forecast line with its level dot
- the 5-hour timeline: actual line drawing in, area fill, dashed projection with the hit marker, now tick, axis labels, Actual/Projected legend, pulsing halo on the last-fetch point
- the stale state: dimmed limits, paused forecast, amber footer
- the footer with the live dot

It opens above the pill (below it if there's no room) and grows with its content. It closes on Esc, on a click outside, on a second pill click, when focus moves away, when you start dragging the pill and in fullscreen. A second launch brings it up. It has been checked in `--demo`, `CUW_DEMO_OFFLINE` and once against the real API. 21 tests pass.

| Measured (release) | Card closed | Card open |
|---|---|---|
| CPU (one core) | 0.05% | **0.31%** (the Python card used a WebView renderer) |
| Private memory | 12 MB | 17–21 MB |

Learned in phase 2:
- **Software rendering makes full redraws expensive:** about 26 ms for the whole card at 175%. The card first cost **51% of a core**, because the halo redrew everything at 60 fps. The fixes:
  1. The halo and the point are **separate DirectComposition layers**, and the halo's grow-and-fade is a **compositor animation** (`IDCompositionAnimation`) that Windows runs. It costs the app nothing per frame.
  2. The card redraws only on a **visible** change. The redraw key leaves out `eta`, which ticks every second, and rounds "now" to whole chart pixels.
  3. Frame-by-frame redraws happen only during the opening sweep and count-up, and for the pulse at 90% or more.
- **Focus:** the pill must be activatable, not `WS_EX_NOACTIVATE`. Otherwise Windows' foreground lock stops the card taking focus, and Esc and close-on-deactivate break. A 50 ms outside-click watcher is the fallback. A second launch calls `AllowSetForegroundWindow` for the running instance.
- **Demo switches:** `CUW_DEMO_STILL=1` holds the demo values steady, for measuring idle cost; the plain demo changes every second. `CUW_GPU=hw` compares against hardware rendering.

Run it (the Python widget must be closed first, since they share a lock):
```
cd rust
cargo build --release
target\release\ClaudeUsageWidget.exe --demo
cargo test
target\release\ClaudeUsageWidget.exe --install     (make it the installed app)
```
**Phase 3 is done (2026-11-26): feature parity.**
- **Activity:** the `LogScanner` port, three stat tiles with an explanatory tooltip, and the 4-week heatmap with paging, a shared colour scale, glow and a hover tooltip.
- **Settings panel (the gear):** pill style, theme, Start with Windows, the alert toggles, fade when idle, hide in fullscreen, taskbar button, refresh interval, plus Done, Data folder and Quit.
- **Alerts:** 75/90 thresholds, the timely reset notice and the "limit ahead" pace alert. They're saved in `state.json` in the same shapes, so switching between the apps never repeats one.
- **Taskbar button:** the ring icon, with `ITaskbarList3` progress that's green, amber "paused" at 75% and red at 90%. Clicking it toggles the card; closing it quits.
- **Autostart:** the first real run switches it on, as the Python app does.
- **Installer:** `ClaudeUsageWidget.exe --install` / `--uninstall`.
- **Tests:** 27, plus a timing test against the real logs that's normally skipped: `cargo test --release real_logs_timing -- --ignored --nocapture`.

| Measured, same conditions | Python app | Rust app |
|---|---|---|
| Memory | ~181 MB (80 Python + 101 WebView2), 8 processes | **14.6 MB** private, 1 process |
| CPU, card closed | 1.69% of one core | **0.44%** (mostly log scanning while Claude Code is busy) |
| CPU, card open | — | 1.15% |
| First log scan (190 MB) | 2.8 s | **0.6 s** |
| Install size | `.venv` + Python | **~720 KB `.exe`** |

**Install / switch over** (`--install`):
1. Copies the `.exe` and icon to `%LOCALAPPDATA%\Programs\ClaudeUsageWidget`.
2. Creates Start menu and Desktop shortcuts carrying the `ClaudeUsageWidget` AppUserModelID.
3. Points the Run key at the `.exe`.
4. Closes any running widget (Python or Rust) with `WM_CLOSE` to its host window, then starts the installed one.

`CUW_INSTALL_TEST=<dir>` installs into a folder without touching the shortcuts, the Run key or running widgets. **To go back** to the Python app, run `Install.bat`.

Learned in phase 3:
- `serde_json` needs `preserve_order`. `state.json`'s `alerts` map is trimmed oldest-first like the Python dict, and BTreeMap ordering would drop the wrong entries.
- The taskbar button must be its own unowned window, because a minimised owner hides the windows it owns (the pill and the card are owned by the hidden host).
- The heatmap tooltip sits next to the hovered **cell**, not the cursor, so the card only redraws when the cell changes and not on every mouse move.

**Testing note:** back up `%APPDATA%\ClaudeUsageWidget\config.json` and `state.json` with `Copy-Item`, never PowerShell `Set-Content -Encoding utf8`. That writes a BOM, and the Python app then resets the settings to defaults. It happened once during phase 2 testing and was fixed by hand.

## Decisions (2026-11-26)
| Topic | Choice | Why |
|---|---|---|
| UI | **Direct2D + DirectWrite + DirectComposition**, drawn by hand through the official `windows` crate | Lightest option, and it gives full control over gradients, glow and animation |
| Glass | **Real Windows 11 acrylic** (`DWMWA_SYSTEMBACKDROP_TYPE = DWMSBT_TRANSIENTWINDOW`), with our tint gradient drawn over it at partial alpha | A live blur with no screenshots. It replaces the 6-second GDI capture, the "exclude from capture" trick and their CPU cost |
| Location | `rust/` in this repo, on branch `rust-port` | The Python app stays the daily driver until the Rust app matches it |
| Rollout | Three phases, each checked in `--demo` | Problems show up early |

## Ground rules carried over from the Python app
These come from docs §12, *Gotchas*.
- **The same data files** live in `%APPDATA%\ClaudeUsageWidget\` (`config.json`, `state.json`, `history.jsonl`, `widget.log`), so switching between the two apps keeps history, alert de-duplication and position.
- **The same single-instance mutex** (`Local\ClaudeUsageWidget`) and the same host window class (`ClaudeUsageWidgetHost`, `WM_APP_SHOW` = 0x8003). The Python and Rust apps can **never run at the same time**, so they can never double-poll the API and cause 429s.
- **HTTP goes through `curl.exe`**, with headers on stdin and the POST body in a temp file. The Python app had Cloudflare trouble with a non-Windows TLS fingerprint, and curl.exe is proven. Secrets never appear on a command line or in the log.
- **Window comparisons use `reset_key()`**; sleep-safe waits use 5-second steps against the wall clock; wake is detected as a gap of more than 30 s, which resets the heartbeat; there's a watchdog; 429 backoff is 120 s growing to 900 s.
- **One `freshness()` rule.** The ETA is measured from the last fetch. A window is *pending* when its reset passed after the last fetch. Displayed % is rounded half up, and never shows 100 below 100.
- **Glance and go.** No new UI in the port.
- `--demo` and `CUW_DEMO_OFFLINE=1` behave as they do today.

## Architecture
```
rust/
  Cargo.toml           windows, serde, serde_json, chrono (local-time formatting)
  src/main.rs          entry: DPI awareness, AppUserModelID, mutex, --demo, panic → log + message box
  src/config.rs        paths, Config (defaults + config.json), State (state.json), logger (rotating)
  src/data/fetch.rs    curl HTTP, OAuth read + refresh, parse, demo, LimitsFetcher thread
  src/data/history.rs  history.jsonl, series, eta_seconds, last_windows, prev_window_peak
  src/data/logs.rs     LogScanner (phase 3)
  src/model.rs         pure: fmt_*, display_pct, freshness, forecast_verdict, build_model, timeline
  src/ui/gfx.rs        D3D11 + DXGI composition swap chain + D2D device context + DComp + DWrite
  src/ui/theme.rs      the glass.css tokens as colours (dark/light), level gradients
  src/ui/anim.rs       eased tweens (bar widths, ring sweep, count-up), pulse; 60 fps only while animating
  src/ui/pill.rs       pill window: bars / ring / text styles, stale mute, error dot, idle dim, drag, menu
  src/ui/card.rs       card window (phase 2/3)
  src/tray.rs          Shell_NotifyIcon ring icon + tooltip + menu + balloon toasts
  src/host.rs          hidden host window: 1 s tick, app messages, taskbar button + progress (phase 3)
  src/winutil.rs       placement, monitors, fullscreen check, theme, autostart, menus
```
- **Threads:** the UI thread runs every window and the message loop. A fetcher thread runs the blocking `curl`. A log-scanner thread arrives in phase 3. Shared state sits behind `Arc<Mutex<_>>`; worker threads `PostMessage` the host to trigger a repaint.
- **Rendering:** each window is `WS_POPUP` with `WS_EX_NOREDIRECTIONBITMAP`, using a premultiplied-alpha composition swap chain, plus the DWM acrylic backdrop and rounded corners. Frames are drawn only when the model changes or an animation is running, so it sits idle between ticks.

## Phases
**Phase 1: data, pill and tray**
- config, state and log
- fetcher (curl, refresh, 429 backoff, sleep-safe), history, model helpers
- the pill in all three styles, with:
  - stale and pending states
  - the amber dot
  - idle dimming
  - drag and remembered position
  - the right-click menu
  - topmost handling and hiding in fullscreen
  - the theme following Windows
- the tray icon, tooltip and menu
- single instance, `--demo`, `CUW_DEMO_OFFLINE`
- ported tests for the pure helpers

**Phase 2: the card**
- bars and rings views with count-up and sweep animations
- the forecast line
- the 5-hour timeline (actual/projected, legend, now tick, halo)
- last week's peak
- close on outside click, open above or below the pill

**Phase 3: the rest, then switch over**
- LogScanner, the stat tiles and the 4-week heatmap with tooltip
- the in-card settings panel
- alerts: thresholds, reset, forecast
- taskbar button, progress and overlay
- watchdog
- autostart pointing at the `.exe`
- an `Install.bat` update, with shortcuts pointing at the `.exe`
- docs
- memory and CPU measured against the Python app, then retire `cuw/` once the Rust app matches it

## Risks
- **Acrylic on a borderless `WS_POPUP` with DirectComposition content** should work on Windows 11 22H2+, but needs verifying. Fallback: an opaque tint (`--body` colour).
- **Hand-drawn UI** means more code. The CSS glow (`box-shadow`, `drop-shadow`) is approximated with layered translucent strokes rather than blur effects, to keep frames cheap.
- **Text metrics** come from DirectWrite rather than Chromium, so spacing is tuned by eye against screenshots of the Python app.
