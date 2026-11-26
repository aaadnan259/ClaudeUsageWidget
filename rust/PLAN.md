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

Run it (the Python widget must be closed first, since they share a lock):
```
cd rust
cargo build --release
target\release\ClaudeUsageWidget.exe --demo
cargo test
```
In phase 1, clicking the pill does nothing; the card arrives in phase 2. Phase 1 doesn't touch autostart.

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
