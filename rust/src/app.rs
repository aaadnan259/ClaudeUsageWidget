//! The app: a hidden host window (1 s tick, tray, app messages), the pill and the details
//! card, all on the UI thread; the limits fetcher runs on its own thread and pokes the host.

use crate::alerts;
use crate::config::{Store, data_dir};
use crate::data::fetch::LimitsFetcher;
use crate::data::history::History;
use crate::data::logs::{self, Stats};
use crate::data::{now, now_utc, reset_key};
use crate::logf;
use crate::model::{FreshState, Level, Model, build_model, fmt_tokens};
use crate::tray::{Tray, WM_TRAY};
use crate::taskbar::Taskbar;
use crate::ui::card::{CARD_W, CardView, Hit};
use crate::ui::card_extra::{CardCtx, PEAK_TIP, STALE_TIP, STATS_TIP};
use crate::ui::gfx::{Gfx, Halo, Surface};
use crate::ui::pill::{PillView, pill_key, size_for};
use crate::ui::theme::{self, Theme};
use crate::winutil::{self as W, MenuItem, item};
use serde_json::{Value, json};
use std::cell::RefCell;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, PAINTSTRUCT};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::*;
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, TRACKMOUSEEVENT, TME_LEAVE, TrackMouseEvent};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

pub const HOST_CLASS: PCWSTR = w!("ClaudeUsageWidgetHost"); // same as the Python app
pub const WM_APP_UPDATE: u32 = 0x8001;
pub const WM_APP_SHOW: u32 = 0x8003;
/// Same as the menu's "Refresh now" (scripts and tests).
pub const WM_APP_REFRESH: u32 = 0x8004;
const TICK_TIMER: usize = 1;
const ANIM_TIMER: usize = 2;
const CARD_ANIM_TIMER: usize = 3;
const CARD_WATCH_TIMER: usize = 4;

// menu ids
const M_SHOW: u32 = 1;
const M_REFRESH: u32 = 2;
const M_BARS: u32 = 3;
const M_RING: u32 = 4;
const M_TEXT: u32 = 5;
const M_RESET_POS: u32 = 6;
const M_DATA: u32 = 7;
const M_QUIT: u32 = 8;
const M_AUTOSTART: u32 = 9;

const OK: u32 = 0x3DDC97;
const WARN: u32 = 0xF5A524;
const CRIT: u32 = 0xF43F5E;
const NONE: u32 = 0x808080;

struct Drag {
    start_cursor: (i32, i32),
    start_pos: (i32, i32),
    moved: bool,
}

pub struct App {
    gfx: Gfx,
    store: Arc<Store>,
    history: Arc<History>,
    limits: Arc<LimitsFetcher>,
    demo: bool,
    host: HWND,
    pill: HWND,
    surface: Option<Surface>,
    tooltip: HWND,
    tooltip_text: String,
    view: PillView,
    pos: (i32, i32),
    scale: f32,
    light: bool,
    theme: Theme,
    model: Model,
    drawn_key: String,
    drag: Option<Drag>,
    tray: Tray,
    last_tick: f64,
    tick: u64,
    hidden_fs: bool,
    reset_refreshed: HashSet<String>,
    anim_on: bool,
    taskbar_created: u32,
    card: HWND,
    card_surface: Option<Surface>,
    card_view: CardView,
    card_open: bool,
    card_scale: f32,
    card_size: (i32, i32),
    card_closed_at: Instant,
    card_anim_on: bool,
    watch_was_down: bool,
    card_key: String,
    card_timer_ms: u32,
    halo: Option<Halo>,
    stats: Arc<Mutex<Option<Stats>>>,
    card_tip: HWND,
    autostart: bool,
    taskbar: Option<Taskbar>,
    taskbar_button_created: u32,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

/// Run `f` with the app, unless it's already borrowed (re-entrant message during a
/// nested loop such as a menu or SetWindowPos): then return None.
fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|a| a.try_borrow_mut().ok().and_then(|mut g| g.as_mut().map(f)))
}

fn spawn_fetcher(f: &Arc<LimitsFetcher>, host: HWND) {
    let h = host.0 as isize;
    *f.on_update.lock().unwrap() = Some(Box::new(move || unsafe {
        let _ = PostMessageW(Some(HWND(h as *mut _)), WM_APP_UPDATE, WPARAM(0), LPARAM(0));
    }));
    let f = f.clone();
    std::thread::Builder::new().name("limits".into()).spawn(move || f.run()).ok();
}

pub fn run(demo: bool) -> windows::core::Result<()> {
    let gfx = Gfx::new()?;
    let store = Arc::new(Store::load(!demo));
    let history = Arc::new(History::load(demo));
    let limits = Arc::new(LimitsFetcher::new(store.clone(), history.clone(), demo));
    unsafe {
        let hinst = GetModuleHandleW(None)?;
        let wc = WNDCLASSW { lpfnWndProc: Some(host_proc), hInstance: hinst.into(), lpszClassName: HOST_CLASS, ..Default::default() };
        RegisterClassW(&wc);
        let wc = WNDCLASSW {
            lpfnWndProc: Some(pill_proc), hInstance: hinst.into(), lpszClassName: w!("ClaudeUsagePillClass"),
            hCursor: LoadCursorW(None, IDC_ARROW)?, ..Default::default()
        };
        RegisterClassW(&wc);
        let wc = WNDCLASSW {
            lpfnWndProc: Some(card_proc), hInstance: hinst.into(), lpszClassName: w!("ClaudeUsageCardClass"),
            hCursor: LoadCursorW(None, IDC_ARROW)?, ..Default::default()
        };
        RegisterClassW(&wc);
        let host = CreateWindowExW(WINDOW_EX_STYLE(0), HOST_CLASS, w!("Claude usage"), WS_OVERLAPPED,
                                   0, 0, 0, 0, None, None, Some(hinst.into()), None)?;
        let pill = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOREDIRECTIONBITMAP,
            w!("ClaudeUsagePillClass"), w!("ClaudeUsagePill"), WS_POPUP,
            0, 0, 100, 40, Some(host), None, Some(hinst.into()), None)?;
        let card = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOREDIRECTIONBITMAP,
            w!("ClaudeUsageCardClass"), w!("ClaudeUsageCard"), WS_POPUP,
            0, 0, 360, 400, Some(host), None, Some(hinst.into()), None)?;
        let card_tip = CreateWindowExW(WS_EX_TOPMOST, TOOLTIPS_CLASSW, None, WINDOW_STYLE(TTS_ALWAYSTIP | TTS_NOPREFIX),
                                       CW_USEDEFAULT, CW_USEDEFAULT, CW_USEDEFAULT, CW_USEDEFAULT, Some(card), None, Some(hinst.into()), None)
            .unwrap_or_default();
        let tooltip = CreateWindowExW(WS_EX_TOPMOST, TOOLTIPS_CLASSW, None, WINDOW_STYLE(TTS_ALWAYSTIP | TTS_NOPREFIX),
                                      CW_USEDEFAULT, CW_USEDEFAULT, CW_USEDEFAULT, CW_USEDEFAULT, Some(pill), None, Some(hinst.into()), None)
            .unwrap_or_default();
        let light = want_light(&store);
        W::set_menu_theme(!light);
        let mut app = App {
            gfx, store: store.clone(), history, limits: limits.clone(), demo, host, pill, surface: None,
            tooltip, tooltip_text: String::new(), view: PillView::new(), pos: (0, 0), scale: W::dpi_scale(pill),
            light, theme: if light { theme::light() } else { theme::dark() },
            model: build_model(&limits.snapshot(), &History::new(vec![], demo), 120.0, now()),
            drawn_key: String::new(), drag: None, tray: Tray::new(host), last_tick: now(), tick: 0,
            hidden_fs: false, reset_refreshed: HashSet::new(), anim_on: false,
            taskbar_created: RegisterWindowMessageW(w!("TaskbarCreated")),
            card, card_surface: None, card_view: CardView::new(), card_open: false, card_scale: 1.0,
            card_size: (0, 0), card_closed_at: Instant::now() - Duration::from_secs(5), card_anim_on: false,
            watch_was_down: true, card_key: String::new(), card_timer_ms: 0, halo: None,
            stats: Arc::new(Mutex::new(None)), card_tip, autostart: W::autostart_enabled(), taskbar: None,
            taskbar_button_created: RegisterWindowMessageW(w!("TaskbarButtonCreated")),
        };
        // First real run: start with Windows (like the Python app); the user can turn it off.
        if !demo && !store.state.lock().unwrap().get("autostart_initialized").and_then(Value::as_bool).unwrap_or(false) {
            if let Err(e) = W::set_autostart(true) {
                logf!("WARNING autostart: {e}");
            }
            store.state.lock().unwrap().insert("autostart_initialized".into(), json!(true));
            store.save_state();
            app.autostart = W::autostart_enabled();
        }
        if store.cfg_bool("taskbar_button") {
            match Taskbar::new(hinst.into(), Some(button_proc)) {
                Ok(t) => app.taskbar = Some(t),
                Err(e) => logf!("WARNING taskbar button: {e}"),
            }
        }
        app.add_card_tips();
        app.card_view.view = match store.state.lock().unwrap().get("view").and_then(Value::as_str) {
            Some("bars") => "bars".into(),
            _ => "rings".into(),
        };
        app.card_view.acrylic = W::style_glass(card, !light);
        app.view.acrylic = W::style_glass(pill, !light);
        if !app.view.acrylic {
            logf!("WARNING acrylic backdrop unavailable; using an opaque fill");
        }
        app.add_tooltip();
        app.place_pill();
        app.push(true);
        let _ = ShowWindow(pill, SW_SHOWNOACTIVATE);
        app.place_pill();
        SetTimer(Some(host), TICK_TIMER, 1000, None);
        APP.with(|a| *a.borrow_mut() = Some(app));
        spawn_fetcher(&limits, host);
        let stats = with_app(|a| a.stats.clone()).unwrap();
        let h = host.0 as isize;
        logs::spawn(store.clone(), stats, Box::new(move || {
            let _ = PostMessageW(Some(HWND(h as *mut _)), WM_APP_UPDATE, WPARAM(0), LPARAM(0));
        }));
        logf!("started (rust{})", if demo { ", demo" } else { "" });

        let mut msg = MSG::default();
        loop {
            // 0 = WM_QUIT, -1 = error: either way the loop is over (BOOL(-1) is "true")
            let r = GetMessageW(&mut msg, None, 0, 0).0;
            if r == 0 || r == -1 {
                break;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    // Exit right away, skipping teardown. Destroying the thread-local App (Direct2D /
    // DirectComposition / COM objects) during thread shutdown hung the process (Quit left
    // it running invisibly), and a normal exit faults in DLL detach. Everything worth
    // keeping is already on disk and the tray icon was removed in quit().
    unsafe {
        let _ = windows::Win32::System::Threading::TerminateProcess(windows::Win32::System::Threading::GetCurrentProcess(), 0);
    }
    std::process::exit(0);
}

/// Why there are no numbers to show yet (tray tooltip / taskbar title).
fn no_data_reason(status: &str, error: &str) -> String {
    match status {
        "loading" => "connecting…".into(),
        "signin" => "not signed in: run `claude`, then /login".into(),
        "expired" => "sign-in expired: run `claude`, then /login".into(),
        _ if !error.is_empty() => error.to_string(),
        _ => "no data yet".into(),
    }
}

fn want_light(store: &Store) -> bool {
    match store.cfg_str("theme").as_str() {
        "light" => true,
        "dark" => false,
        _ => W::windows_uses_light_theme(),
    }
}

impl App {
    fn interval(&self) -> f64 {
        if self.demo { 3.0 } else { self.store.cfg_u64("limits_refresh_seconds") as f64 }
    }

    fn px(&self, v: f32) -> i32 {
        (v * self.scale).round() as i32
    }

    fn pill_size(&self) -> (i32, i32) {
        let (w, h) = size_for(&self.view.style);
        (self.px(w), self.px(h))
    }

    // ---------------------------------------------------------------- placement
    fn place_pill(&mut self) {
        self.view.style = self.store.cfg_str("pill_style");
        self.scale = W::dpi_scale(self.pill);
        let (w, h) = self.pill_size();
        let saved = self.store.state.lock().unwrap().get("pill_pos").cloned();
        let (x, y) = match saved.as_ref().and_then(Value::as_array) {
            Some(p) if p.len() == 2 => {
                let v = W::virtual_screen();
                let x = (p[0].as_i64().unwrap_or(0) as i32).clamp(v.l, (v.r - w).max(v.l));
                let y = (p[1].as_i64().unwrap_or(0) as i32).clamp(v.t, (v.b - h).max(v.t));
                let (_, mon, _) = W::monitor_info(x + w / 2, y + h / 2);
                (x.clamp(mon.l, (mon.r - w).max(mon.l)), y.clamp(mon.t, (mon.b - h).max(mon.t)))
            }
            _ => {
                let wa = W::primary_work_area();
                let m = self.px(self.store.cfg_u64("margin_px") as f32);
                (wa.r - w - m, wa.b - h - m)
            }
        };
        self.pos = (x, y);
        W::place(self.pill, x, y, Some((w, h)));
        let s = W::dpi_scale(self.pill); // the move may land on another monitor
        if s != self.scale {
            self.scale = s;
            let (w, h) = self.pill_size();
            W::place(self.pill, x, y, Some((w, h)));
        }
        self.drawn_key.clear();
        self.draw();
    }

    fn reset_position(&mut self) {
        self.store.state.lock().unwrap().remove("pill_pos");
        self.store.save_state();
        self.place_pill();
    }

    // ---------------------------------------------------------------- drawing
    fn draw(&mut self) {
        let (w, h) = self.pill_size();
        if self.surface.is_none() {
            match Surface::new(&self.gfx, self.pill, w as u32, h as u32, self.scale) {
                Ok(s) => self.surface = Some(s),
                Err(e) => {
                    logf!("ERROR surface: {e}");
                    return;
                }
            }
        }
        let Some(s) = self.surface.as_mut() else { return };
        if s.resize(w as u32, h as u32, self.scale).is_err() {
            self.surface = None;
            return;
        }
        let (dw, dh) = size_for(&self.view.style);
        let mut c = s.begin(&self.gfx);
        self.view.draw(&mut c, &self.theme, &self.model, dw, dh);
        if let Err(e) = s.present() {
            logf!("WARNING present: {e}; recreating surface");
            self.surface = None;
        }
        self.update_anim_timer();
    }

    fn update_anim_timer(&mut self) {
        let want = self.view.animating(&self.model);
        if want && !self.anim_on {
            unsafe { SetTimer(Some(self.pill), ANIM_TIMER, 16, None) };
        } else if !want && self.anim_on {
            unsafe {
                let _ = KillTimer(Some(self.pill), ANIM_TIMER);
            }
        }
        self.anim_on = want;
    }

    // ---------------------------------------------------------------- model → UI
    fn push(&mut self, force: bool) {
        let lim = self.limits.snapshot();
        self.model = build_model(&lim, &self.history, self.interval(), now());
        self.view.dim_setting = self.store.cfg_bool("dim_when_idle");
        self.view.today = self.stats.lock().unwrap().as_ref().map(|s| fmt_tokens(s.today));
        self.view.sync(&self.model);
        let key = format!("{}|{:?}", pill_key(&self.model, &self.view.style, self.view.dim_setting, self.light),
                          if self.model.rows.is_empty() { self.view.today.clone() } else { None });
        if force || key != self.drawn_key {
            self.drawn_key = key;
            self.draw();
        }
        self.set_tooltip(&PillView::tooltip(&self.model));
        self.update_tray();
        if self.card_open {
            self.card_view.sync(&self.model);
            self.draw_card();
        }
    }

    fn update_tray(&mut self) {
        let m = &self.model;
        let five = m.rows.first();
        let (pct, color, tip) = match five {
            Some(f) if !f.pending => {
                let stale = m.fresh.state == FreshState::Stale;
                let color = if stale { NONE } else { match f.level { Level::Ok => OK, Level::Warn => WARN, Level::Crit => CRIT } };
                let mut tip = format!("Claude · {} {}", f.title, f.pct_text());
                if !f.reset_short.is_empty() {
                    tip += &format!(" (resets in {})", f.reset_short);
                }
                if let Some(wk) = m.rows.get(1).filter(|r| r.pct_i.is_some()) {
                    tip += &format!(" · {} {}", wk.title, wk.pct_text());
                }
                if stale {
                    tip += &format!(" · {}, {}", m.fresh.reason, m.fresh.label.to_lowercase());
                }
                (Some(f.pct), color, tip)
            }
            Some(_) => (None, NONE, "Claude · limit reset, updating…".to_string()),
            None => (None, NONE, format!("Claude usage · {}", no_data_reason(&m.status, &m.error))),
        };
        self.tray.update(&self.gfx, pct, color, &tip);
        if let Some(tb) = self.taskbar.as_mut() {
            let title = match five {
                Some(f) if !f.pending => format!("Claude {} · {}{}", f.pct_text(), f.reset_short,
                                                 if m.fresh.state == FreshState::Stale { " (stale)" } else { "" }),
                Some(_) => "Claude usage · updating".to_string(),
                None => format!("Claude usage · {}", no_data_reason(&m.status, &m.error)),
            };
            tb.update(&self.gfx, pct, color, &title, m.fresh.state != FreshState::Stale);
        }
    }

    /// Native tooltips over the card's stat tiles and last week's peak.
    fn add_card_tips(&mut self) {
        if self.card_tip.is_invalid() {
            return;
        }
        for (id, text) in [(1usize, STATS_TIP), (2, PEAK_TIP), (3, STALE_TIP)] {
            let mut buf = W::wide(text);
            let mut ti = TTTOOLINFOW {
                cbSize: std::mem::size_of::<TTTOOLINFOW>() as u32, uFlags: TTF_SUBCLASS, hwnd: self.card, uId: id,
                lpszText: windows::core::PWSTR(buf.as_mut_ptr()), ..Default::default()
            };
            unsafe {
                SendMessageW(self.card_tip, TTM_ADDTOOLW, None, Some(LPARAM(&mut ti as *mut _ as isize)));
            }
        }
        unsafe {
            SendMessageW(self.card_tip, TTM_SETMAXTIPWIDTH, None, Some(LPARAM(320)));
        }
    }

    fn update_card_tips(&mut self) {
        if self.card_tip.is_invalid() {
            return;
        }
        let rects = self.card_view.tip_rects();
        let s = self.card_scale;
        for id in [1usize, 2, 3] {
            let want = match id { 1 => Hit::StatsTip, 2 => Hit::PeakTip, _ => Hit::StaleTip };
            let r = rects.iter().find(|(_, h)| *h == want).map(|(r, _)| *r).unwrap_or((0.0, 0.0, 0.0, 0.0));
            let mut ti = TTTOOLINFOW {
                cbSize: std::mem::size_of::<TTTOOLINFOW>() as u32, hwnd: self.card, uId: id,
                rect: RECT { left: (r.0 * s) as i32, top: (r.1 * s) as i32, right: ((r.0 + r.2) * s) as i32, bottom: ((r.1 + r.3) * s) as i32 },
                ..Default::default()
            };
            unsafe {
                SendMessageW(self.card_tip, TTM_NEWTOOLRECTW, None, Some(LPARAM(&mut ti as *mut _ as isize)));
            }
        }
    }

    fn add_tooltip(&mut self) {
        if self.tooltip.is_invalid() {
            return;
        }
        let mut ti = TTTOOLINFOW {
            cbSize: std::mem::size_of::<TTTOOLINFOW>() as u32,
            uFlags: TTF_IDISHWND | TTF_SUBCLASS,
            hwnd: self.pill,
            uId: self.pill.0 as usize,
            lpszText: windows::core::PWSTR(w!("").as_ptr() as *mut u16),
            ..Default::default()
        };
        unsafe {
            SendMessageW(self.tooltip, TTM_ADDTOOLW, None, Some(LPARAM(&mut ti as *mut _ as isize)));
            SendMessageW(self.tooltip, TTM_SETMAXTIPWIDTH, None, Some(LPARAM(400)));
        }
    }

    fn set_tooltip(&mut self, text: &str) {
        if self.tooltip.is_invalid() || text == self.tooltip_text {
            return;
        }
        self.tooltip_text = text.to_string();
        let mut buf = W::wide(text);
        let mut ti = TTTOOLINFOW {
            cbSize: std::mem::size_of::<TTTOOLINFOW>() as u32,
            hwnd: self.pill,
            uId: self.pill.0 as usize,
            lpszText: windows::core::PWSTR(buf.as_mut_ptr()),
            ..Default::default()
        };
        unsafe {
            SendMessageW(self.tooltip, TTM_UPDATETIPTEXTW, None, Some(LPARAM(&mut ti as *mut _ as isize)));
        }
    }

    // ---------------------------------------------------------------- main tick (1 s)
    fn on_tick(&mut self) {
        // CUW_TEST_CRASH=1 (testing only): crash the UI thread 5 s in, to check the restart.
        if self.tick == 5 && std::env::var_os("CUW_TEST_CRASH").is_some() {
            panic!("CUW_TEST_CRASH: deliberate test crash");
        }
        let t = now();
        if t - self.last_tick > 30.0 {
            // woke from sleep: fresh numbers now; time asleep isn't a stuck thread
            logf!("resumed after {:.0}s", t - self.last_tick);
            self.limits.set_heartbeat(t);
            self.limits.refresh_now();
        }
        self.last_tick = t;
        self.watchdog();
        let light = want_light(&self.store);
        if light != self.light {
            self.light = light;
            self.theme = if light { theme::light() } else { theme::dark() };
            self.view.acrylic = W::style_glass(self.pill, !light);
            self.card_view.acrylic = W::style_glass(self.card, !light);
            W::set_menu_theme(!light);
            self.drawn_key.clear();
        }
        self.push(false);
        let alerts = alerts::check(&self.store, &self.limits.snapshot(), &self.model, now_utc());
        for (title, msg) in alerts {
            logf!("alert: {title}");
            self.tray.notify(&title, &msg);
        }
        if self.tick.is_multiple_of(5) {
            self.autostart = W::autostart_enabled();
        }
        self.reset_boundary_refresh();
        self.fullscreen_check();
        if !self.hidden_fs && self.drag.is_none() {
            W::set_topmost(self.pill);
        }
        self.tick += 1;
        if self.tick.is_multiple_of(15) && self.drag.is_none() {
            self.place_pill(); // follows taskbar / resolution changes
        }
    }

    /// Replace a stuck limits thread (it can't be killed; it's told to stop and abandoned).
    fn watchdog(&mut self) {
        if !self.limits.overdue() {
            return;
        }
        logf!("WARNING limits thread stuck; starting a new one");
        let old = self.limits.clone();
        old.stopped.store(true, Ordering::Relaxed);
        let fresh = Arc::new(LimitsFetcher::new(self.store.clone(), self.history.clone(), self.demo));
        {
            let o = old.state.lock().unwrap();
            if !o.windows.is_empty() {
                let mut n = fresh.state.lock().unwrap();
                n.windows = o.windows.clone();
                n.plan = o.plan.clone();
                n.status = "error".into();
                n.error = "Reconnecting".into();
                n.updated = o.updated;
            }
        }
        self.limits = fresh;
        spawn_fetcher(&self.limits, self.host);
    }

    /// A reset just passed: fetch the new window once (not every second while the API
    /// still reports the old reset time). Same rule as the Python check_alerts.
    fn reset_boundary_refresh(&mut self) {
        let lim = self.limits.snapshot();
        if lim.status != "ok" {
            return;
        }
        let n = now_utc();
        for key in ["five_hour", "seven_day"] {
            let Some(w) = lim.windows.get(key) else { continue };
            let rkey = format!("{key}|{}", reset_key(w.resets.as_ref()));
            if w.resets.is_some_and(|r| r <= n) && self.reset_refreshed.insert(rkey) {
                self.limits.refresh_now();
            }
        }
    }

    fn fullscreen_check(&mut self) {
        if !self.store.cfg_bool("hide_in_fullscreen") {
            if self.hidden_fs {
                unsafe {
                    let _ = ShowWindow(self.pill, SW_SHOWNOACTIVATE);
                }
                self.hidden_fs = false;
            }
            return;
        }
        let (w, h) = self.pill_size();
        let (mon, _, _) = W::monitor_info(self.pos.0 + w / 2, self.pos.1 + h / 2);
        let fs = W::foreground_is_fullscreen(&[self.pill, self.host, self.card], mon);
        if fs && !self.hidden_fs {
            self.hide_card();
            unsafe {
                let _ = ShowWindow(self.pill, SW_HIDE);
            }
            self.hidden_fs = true;
        } else if !fs && self.hidden_fs {
            unsafe {
                let _ = ShowWindow(self.pill, SW_SHOWNOACTIVATE);
            }
            self.place_pill();
            self.hidden_fs = false;
        }
    }

    // ---------------------------------------------------------------- menus / actions
    fn pill_menu(&self) -> Vec<MenuItem> {
        let style = self.view.style.as_str();
        vec![
            item(M_SHOW, if self.card_open { "Hide details" } else { "Show details" }, false),
            item(M_REFRESH, "Refresh now", false),
            MenuItem::Sep,
            item(M_BARS, "Pill: bars", style == "bars"),
            item(M_RING, "Pill: ring", style == "ring"),
            item(M_TEXT, "Pill: text", style == "text"),
            MenuItem::Sep,
            item(M_RESET_POS, "Reset position", false),
            item(M_DATA, "Open data folder", false),
            MenuItem::Sep,
            item(M_QUIT, "Quit", false),
        ]
    }

    fn tray_menu(&self) -> Vec<MenuItem> {
        vec![
            item(M_SHOW, if self.card_open { "Hide details" } else { "Show details" }, false),
            item(M_REFRESH, "Refresh now", false),
            item(M_AUTOSTART, "Start with Windows", self.autostart),
            item(M_RESET_POS, "Reset position", false),
            item(M_DATA, "Open data folder", false),
            MenuItem::Sep,
            item(M_QUIT, "Quit", false),
        ]
    }

    /// Apply settings from the card or menus (same keys as config.json, plus "autostart").
    fn save_settings(&mut self, mut changes: serde_json::Map<String, Value>) {
        if let Some(on) = changes.remove("autostart").and_then(|v| v.as_bool()) {
            if let Err(e) = W::set_autostart(on) {
                logf!("WARNING autostart: {e}");
            }
            self.autostart = W::autostart_enabled();
        }
        let old_style = self.store.cfg_str("pill_style");
        self.store.update_cfg(&changes);
        if self.store.cfg_str("pill_style") != old_style {
            self.place_pill();
        }
        if changes.contains_key("limits_refresh_seconds") {
            self.limits.refresh_now();
        }
        if changes.contains_key("theme") {
            self.last_tick = now(); // the tick applies the theme; do it now
            let light = want_light(&self.store);
            if light != self.light {
                self.light = light;
                self.theme = if light { theme::light() } else { theme::dark() };
                self.view.acrylic = W::style_glass(self.pill, !light);
                self.card_view.acrylic = W::style_glass(self.card, !light);
                W::set_menu_theme(!light);
                self.halo = None;
                self.card_surface = None;
            }
        }
        self.push(true);
    }

    fn on_menu(&mut self, id: u32) {
        match id {
            M_SHOW => self.toggle_card(),
            M_REFRESH => self.limits.refresh_now(),
            M_BARS | M_RING | M_TEXT => {
                let s = match id { M_BARS => "bars", M_RING => "ring", _ => "text" };
                let mut ch = serde_json::Map::new();
                ch.insert("pill_style".into(), json!(s));
                self.save_settings(ch);
            }
            M_AUTOSTART => {
                let mut ch = serde_json::Map::new();
                ch.insert("autostart".into(), json!(!self.autostart));
                self.save_settings(ch);
            }
            M_RESET_POS => self.reset_position(),
            M_DATA => W::open_folder(&data_dir()),
            M_QUIT => self.quit(),
            _ => {}
        }
    }

    // ---------------------------------------------------------------- card
    fn toggle_card(&mut self) {
        if self.card_open {
            self.hide_card();
        } else if self.card_closed_at.elapsed() > Duration::from_millis(350) {
            // A click that just closed the card by deactivating it shouldn't reopen it.
            self.show_card();
        }
    }

    /// Use compact mode (no activity section) if the full card is taller than the screen.
    fn fit_card(&mut self, scale: f32) {
        let (stats, cfg) = (self.stats.lock().unwrap().clone(), self.store.cfg());
        let ctx = CardCtx { stats: stats.as_ref(), cfg: &cfg, autostart: self.autostart };
        let (pw, ph) = self.pill_size();
        let (_, _, work) = W::monitor_info(self.pos.0 + pw / 2, self.pos.1 + ph / 2);
        let gap = (10.0 * scale).round() as i32;
        let was = self.card_view.compact;
        self.card_view.compact = false;
        let full = self.card_view.height(&self.gfx, &self.model, &ctx);
        self.card_view.compact = crate::ui::card::needs_compact(full, scale, work.b - work.t - 2 * gap);
        if self.card_view.compact && !was {
            logf!("screen too short for the full card: hiding the activity section");
        }
    }

    /// Above the pill, right-aligned with it; below it if there's no room above.
    fn card_rect(&self, scale: f32) -> (i32, i32, i32, i32) {
        let w = (CARD_W * scale).round() as i32;
        let (stats, cfg) = (self.stats.lock().unwrap().clone(), self.store.cfg());
        let ctx = CardCtx { stats: stats.as_ref(), cfg: &cfg, autostart: self.autostart };
        let h = (self.card_view.height(&self.gfx, &self.model, &ctx) * scale).ceil() as i32;
        let gap = (10.0 * scale).round() as i32;
        let (pw, ph) = self.pill_size();
        let (px, py) = self.pos;
        let (_, _, work) = W::monitor_info(px + pw / 2, py + ph / 2);
        let x = (px + pw - w).clamp(work.l + gap, (work.r - w - gap).max(work.l + gap));
        let mut y = py - h - gap;
        if y < work.t + gap {
            y = (py + ph + gap).min(work.b - h - gap);
        }
        (x, y, w, h)
    }

    fn show_card(&mut self) {
        if self.card_open || self.hidden_fs {
            return;
        }
        self.card_open = true;
        self.card_view.settings_open = false;
        self.card_view.hm_week.set(0);
        *self.card_view.hover.borrow_mut() = None;
        self.push(false);
        self.card_scale = W::dpi_scale(self.pill);
        self.fit_card(self.card_scale);
        let (x, y, w, h) = self.card_rect(self.card_scale);
        W::place(self.card, x, y, Some((w, h)));
        let s = W::dpi_scale(self.card);
        if s != self.card_scale {
            self.card_scale = s;
            self.fit_card(s);
            let (x, y, w, h) = self.card_rect(s);
            W::place(self.card, x, y, Some((w, h)));
        }
        self.card_size = (w, h);
        self.card_view.replay(&self.model);
        self.draw_card();
        if let Some(s) = self.card_surface.as_ref() {
            let _ = s.animate_in(&self.gfx); // fade + slide up, run by the compositor
        }
        unsafe {
            let _ = ShowWindow(self.card, SW_SHOW);
            let _ = SetForegroundWindow(self.card);
            // Fallback for when Windows refuses focus: watch for clicks outside (as the
            // Python app does), so the card still closes like a flyout.
            SetTimer(Some(self.card), CARD_WATCH_TIMER, 50, None);
        }
        self.watch_was_down = true;
    }

    fn hide_card(&mut self) {
        if !self.card_open {
            return;
        }
        self.card_open = false;
        self.card_closed_at = Instant::now();
        unsafe {
            let _ = KillTimer(Some(self.card), CARD_WATCH_TIMER);
        }
        unsafe {
            let _ = ShowWindow(self.card, SW_HIDE);
        }
        self.update_card_timer();
    }

    fn draw_card(&mut self) {
        if !self.card_open {
            return;
        }
        // grow/shrink with the content
        self.fit_card(self.card_scale);
        let (x, y, w, h) = self.card_rect(self.card_scale);
        if (w, h) != self.card_size {
            self.card_size = (w, h);
            W::place(self.card, x, y, Some((w, h)));
        }
        if self.card_surface.is_none() {
            match Surface::new(&self.gfx, self.card, w as u32, h as u32, self.card_scale) {
                Ok(s) => self.card_surface = Some(s),
                Err(e) => {
                    logf!("ERROR card surface: {e}");
                    return;
                }
            }
        }
        let Some(s) = self.card_surface.as_mut() else { return };
        if s.resize(w as u32, h as u32, self.card_scale).is_err() {
            self.card_surface = None;
            return;
        }
        // Redraw only on a visible change (software rendering makes full redraws costly):
        // "now" is rounded to chart pixels (it moves one every ~56 s), the halo is a
        // compositor layer, and only the open animations redraw every frame.
        let mut km = self.model.clone();
        if let Some(t) = km.timeline.as_mut() {
            t.now_x = (t.now_x * 320.0).round() / 320.0;
        }
        km.eta = None; // seconds tick every second; the forecast text it feeds is coarse
        let (stats, cfg) = (self.stats.lock().unwrap().clone(), self.store.cfg());
        let ctx = CardCtx { stats: stats.as_ref(), cfg: &cfg, autostart: self.autostart };
        let key = format!("{:?}|{}|{}|{}|{:?}|{:?}|{}|{}|{:?}", km, self.card_view.view, self.card_view.settings_open,
                          self.card_view.hm_week.get(), self.card_view.hover.borrow(), stats, self.autostart, self.light,
                          serde_json::to_string(&cfg).unwrap_or_default());
        let full = key != self.card_key || self.card_view.content_animating(&self.model);
        if full {
            self.card_key = key;
            let mut c = s.begin(&self.gfx);
            self.card_view.draw(&mut c, &self.theme, &self.model, &ctx);
            if let Err(e) = s.present() {
                logf!("WARNING card present: {e}; recreating surface");
                self.card_surface = None;
                self.halo = None;
                self.card_key.clear();
                return;
            }
            self.update_card_tips();
        }
        self.update_halo();
        self.update_card_timer();
    }

    /// The timeline's last-fetch point and its pulsing halo (a compositor animation).
    fn update_halo(&mut self) {
        let Some(s) = self.card_surface.as_ref() else { return };
        if self.halo.is_none() {
            match Halo::new(&self.gfx, &s.visual) {
                Ok(h) => self.halo = Some(h),
                Err(e) => {
                    logf!("WARNING halo layer: {e}");
                    return;
                }
            }
        }
        let (scale, t) = (self.card_scale, self.theme);
        let point = self.card_view.point();
        let Some(h) = self.halo.as_mut() else { return };
        let r = match point {
            Some((x, y, fresh, c2)) => h.show(&self.gfx, x, y, scale, t.body, if fresh { c2 } else { t.sub }, fresh.then_some(c2)),
            None => h.hide(&self.gfx),
        };
        if let Err(e) = r {
            logf!("WARNING halo: {e}");
        }
    }

    fn update_card_timer(&mut self) {
        // 60 fps only while the content animates (open sweep, count-up, 90%+ pulse);
        // the halo is animated by the compositor.
        let ms = if self.card_open && self.card_view.animating(&self.model) { 16 } else { 0 };
        if ms != self.card_timer_ms {
            unsafe {
                if ms == 0 {
                    let _ = KillTimer(Some(self.card), CARD_ANIM_TIMER);
                } else {
                    SetTimer(Some(self.card), CARD_ANIM_TIMER, ms, None);
                }
            }
            self.card_timer_ms = ms;
        }
        self.card_anim_on = ms != 0;
    }

    /// A mouse button went down outside the card and the pill: close.
    fn watch_outside(&mut self) {
        let down = W::mouse_down(true);
        if down && !self.watch_was_down {
            let (x, y) = W::cursor_pos();
            let inside = [self.card, self.pill].iter().any(|h| {
                let r = W::window_rect(*h);
                x >= r.l && x < r.r && y >= r.t && y < r.b
            });
            if !inside {
                self.hide_card();
            }
        }
        self.watch_was_down = down;
    }

    /// Mouse position in card DIPs.
    fn card_point(&self, lp: LPARAM) -> (f32, f32) {
        let x = (lp.0 & 0xFFFF) as i16 as f32;
        let y = ((lp.0 >> 16) & 0xFFFF) as i16 as f32;
        (x / self.card_scale, y / self.card_scale)
    }

    fn on_card_move(&mut self, lp: LPARAM) {
        let (x, y) = self.card_point(lp);
        self.card_view.cursor.set(Some((x, y)));
        let over = self.card_view.hit_at(x, y).filter(|h| !matches!(h, Hit::StatsTip | Hit::PeakTip | Hit::StaleTip));
        if over != *self.card_view.hover.borrow() {
            *self.card_view.hover.borrow_mut() = over;
            let mut tme = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: self.card, dwHoverTime: 0 };
            unsafe {
                let _ = TrackMouseEvent(&mut tme);
            }
            self.draw_card();
        }
    }

    fn on_card_click(&mut self, lp: LPARAM) {
        let (x, y) = self.card_point(lp);
        match self.card_view.hit_at(x, y) {
            Some(Hit::View) => {
                let v = if self.card_view.view == "bars" { "rings" } else { "bars" };
                self.card_view.view = v.into();
                self.card_view.settings_open = false;
                self.store.state.lock().unwrap().insert("view".into(), json!(v));
                self.store.save_state();
                self.card_view.replay(&self.model);
            }
            Some(Hit::Gear) => self.card_view.settings_open = !self.card_view.settings_open,
            Some(Hit::Done) => self.card_view.settings_open = false,
            Some(Hit::WeekPrev) => self.card_view.hm_week.set(self.card_view.hm_week.get() + 1),
            Some(Hit::WeekNext) => self.card_view.hm_week.set(self.card_view.hm_week.get().saturating_sub(1)),
            Some(Hit::Set(k, v)) => {
                let mut ch = serde_json::Map::new();
                ch.insert(k.to_string(), v);
                self.save_settings(ch);
            }
            Some(Hit::Folder) => W::open_folder(&data_dir()),
            Some(Hit::Quit) => {
                self.quit();
                return;
            }
            _ => return,
        }
        self.draw_card();
    }

    fn quit(&mut self) {
        logf!("quit");
        self.tray.remove();
        if let Some(t) = self.taskbar.as_mut() {
            t.destroy();
        }
        unsafe { PostQuitMessage(0) };
    }

    // ---------------------------------------------------------------- pill input
    fn on_lbutton_down(&mut self) {
        unsafe { SetCapture(self.pill) };
        self.drag = Some(Drag { start_cursor: W::cursor_pos(), start_pos: self.pos, moved: false });
    }

    fn on_mouse_move(&mut self) {
        if !self.view.hover {
            self.view.hover = true;
            let mut tme = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: self.pill, dwHoverTime: 0 };
            unsafe {
                let _ = TrackMouseEvent(&mut tme);
            }
            self.view.sync(&self.model);
            self.draw();
        }
        let threshold = self.px(5.0);
        let Some(d) = self.drag.as_mut() else { return };
        let (cx, cy) = W::cursor_pos();
        let (dx, dy) = (cx - d.start_cursor.0, cy - d.start_cursor.1);
        let mut started = false;
        if !d.moved && dx.abs() + dy.abs() > threshold {
            d.moved = true;
            started = true;
        }
        if d.moved {
            self.pos = (d.start_pos.0 + dx, d.start_pos.1 + dy);
            W::place(self.pill, self.pos.0, self.pos.1, None);
        }
        if started {
            self.hide_card();
        }
    }

    /// The pill crossed onto a monitor with another scale.
    fn on_dpi_changed(&mut self) {
        if self.drag.is_some() {
            // mid-drag: resize in place; placing it at the saved spot would snap it back
            self.scale = W::dpi_scale(self.pill);
            let (w, h) = self.pill_size();
            W::place(self.pill, self.pos.0, self.pos.1, Some((w, h)));
            self.drawn_key.clear();
            self.draw();
        } else {
            self.place_pill();
        }
    }

    /// Mouse capture was taken away mid-drag (another window, a system dialog): finish
    /// the drag where it is, without treating it as a click.
    fn on_capture_lost(&mut self) {
        let Some(d) = self.drag.take() else { return };
        if d.moved {
            self.store.state.lock().unwrap().insert("pill_pos".into(), json!([self.pos.0, self.pos.1]));
            self.store.save_state();
            self.place_pill();
        }
    }

    fn on_lbutton_up(&mut self) {
        let Some(d) = self.drag.take() else {
            unsafe {
                let _ = ReleaseCapture();
            }
            return;
        };
        unsafe {
            let _ = ReleaseCapture(); // (our WM_CAPTURECHANGED sees no drag now)
        }
        if d.moved {
            // Use the final cursor position: mouse-move messages coalesce on fast drags.
            let (cx, cy) = W::cursor_pos();
            self.pos = (d.start_pos.0 + cx - d.start_cursor.0, d.start_pos.1 + cy - d.start_cursor.1);
            self.store.state.lock().unwrap().insert("pill_pos".into(), json!([self.pos.0, self.pos.1]));
            self.store.save_state();
            self.place_pill(); // re-clamps and adopts the new monitor's DPI
        } else {
            self.toggle_card();
        }
    }

    fn on_mouse_leave(&mut self) {
        self.view.hover = false;
        self.view.sync(&self.model);
        self.draw();
    }
}

fn show_menu(owner: HWND, which: &str) {
    let Some((items, above, owner)) = with_app(|a| {
        // With the card open, the card owns the menu so it keeps focus and stays open.
        let owner = if which == "pill" && a.card_open { a.card } else { owner };
        if which == "tray" { (a.tray_menu(), None, owner) } else { (a.pill_menu(), Some(a.pos.1 - a.px(6.0)), owner) }
    }) else { return };
    let cmd = W::popup_menu(owner, &items, above);
    if cmd != 0 {
        with_app(|a| a.on_menu(cmd));
    }
}

unsafe extern "system" fn host_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_TIMER if wp.0 == TICK_TIMER => {
            with_app(|a| a.on_tick());
            LRESULT(0)
        }
        WM_APP_UPDATE => {
            with_app(|a| a.push(false));
            LRESULT(0)
        }
        WM_APP_SHOW => {
            with_app(|a| {
                if a.card_open {
                    unsafe {
                        let _ = SetForegroundWindow(a.card);
                    }
                } else {
                    a.card_closed_at = Instant::now() - Duration::from_secs(5);
                    a.show_card();
                }
            });
            LRESULT(0)
        }
        WM_APP_REFRESH => {
            with_app(|a| a.limits.refresh_now());
            LRESULT(0)
        }
        // Asked to close (the installer updating it, or Windows): quit cleanly so the tray
        // icon is removed instead of lingering.
        WM_CLOSE => {
            with_app(|a| a.quit());
            LRESULT(0)
        }
        WM_TRAY => {
            // NOTIFYICON_VERSION_4: the event is in the low word of lParam.
            match (lp.0 as u32) & 0xFFFF {
                x if x == WM_LBUTTONUP => {
                    with_app(|a| a.toggle_card());
                }
                x if x == WM_RBUTTONUP || x == WM_CONTEXTMENU => show_menu(hwnd, "tray"),
                _ => {}
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => {
            let readd = with_app(|a| msg == a.taskbar_created && msg != 0).unwrap_or(false);
            if readd {
                with_app(|a| a.tray.readd());
            }
            unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
        }
    }
}

unsafe extern "system" fn pill_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        // Clicking the pill activates it (that's what lets the card take focus when it
        // opens) — except while the card is open, so the card keeps focus.
        WM_MOUSEACTIVATE if with_app(|a| a.card_open).unwrap_or(false) => LRESULT(MA_NOACTIVATE as isize),
        WM_LBUTTONDOWN => {
            with_app(|a| a.on_lbutton_down());
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            with_app(|a| a.on_mouse_move());
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            with_app(|a| a.on_lbutton_up());
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            with_app(|a| a.on_mouse_leave());
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            let host = with_app(|a| a.host).unwrap_or(hwnd);
            show_menu(host, "pill");
            LRESULT(0)
        }
        WM_TIMER if wp.0 == ANIM_TIMER => {
            with_app(|a| a.draw());
            LRESULT(0)
        }
        WM_DPICHANGED => {
            with_app(|a| a.on_dpi_changed());
            LRESULT(0)
        }
        WM_CAPTURECHANGED => {
            with_app(|a| a.on_capture_lost());
            LRESULT(0)
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            unsafe {
                BeginPaint(hwnd, &mut ps);
                let _ = EndPaint(hwnd, &ps);
            }
            with_app(|a| a.draw());
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

unsafe extern "system" fn card_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_MOUSEMOVE => {
            with_app(|a| a.on_card_move(lp));
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            with_app(|a| {
                a.card_view.cursor.set(None);
                if a.card_view.hover.borrow().is_some() {
                    *a.card_view.hover.borrow_mut() = None;
                    a.draw_card();
                }
            });
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            with_app(|a| a.on_card_click(lp));
            LRESULT(0)
        }
        WM_KEYDOWN if wp.0 == 0x1B => {
            with_app(|a| a.hide_card());
            LRESULT(0)
        }
        // Clicking anywhere outside (another window, the desktop, the taskbar) closes it.
        WM_ACTIVATE if (wp.0 & 0xFFFF) as u32 == WA_INACTIVE => {
            with_app(|a| a.hide_card());
            LRESULT(0)
        }
        WM_TIMER if wp.0 == CARD_WATCH_TIMER => {
            with_app(|a| a.watch_outside());
            LRESULT(0)
        }
        WM_TIMER if wp.0 == CARD_ANIM_TIMER => {
            with_app(|a| a.draw_card());
            LRESULT(0)
        }
        WM_RBUTTONUP => LRESULT(0),
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            unsafe {
                BeginPaint(hwnd, &mut ps);
                let _ = EndPaint(hwnd, &ps);
            }
            with_app(|a| a.draw_card());
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

unsafe extern "system" fn button_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        // Clicking the (minimised) taskbar button asks to restore it: toggle the card instead.
        WM_SYSCOMMAND if matches!((wp.0 & 0xFFF0) as u32, SC_RESTORE | SC_MAXIMIZE) => {
            with_app(|a| a.toggle_card());
            LRESULT(0)
        }
        WM_CLOSE => {
            with_app(|a| a.quit());
            LRESULT(0)
        }
        _ => {
            let reapply = with_app(|a| msg == a.taskbar_button_created && msg != 0).unwrap_or(false);
            if reapply {
                with_app(|a| {
                    if let Some(t) = a.taskbar.as_mut() {
                        t.reapply(&a.gfx);
                    }
                });
            }
            unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
        }
    }
}
