//! The app: a hidden host window (1 s tick, tray, app messages) and the pill window, all
//! on the UI thread; the limits fetcher runs on its own thread and pokes the host.

use crate::config::{Store, data_dir};
use crate::data::fetch::LimitsFetcher;
use crate::data::history::History;
use crate::data::{now, now_utc, reset_key};
use crate::logf;
use crate::model::{FreshState, Level, Model, build_model};
use crate::tray::{Tray, WM_TRAY};
use crate::ui::gfx::{Gfx, Surface};
use crate::ui::pill::{PillView, pill_key, size_for};
use crate::ui::theme::{self, Theme};
use crate::winutil::{self as W, MenuItem, item};
use serde_json::{Value, json};
use std::cell::RefCell;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::Ordering;
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
const TICK_TIMER: usize = 1;
const ANIM_TIMER: usize = 2;

// menu ids
const M_SHOW: u32 = 1;
const M_REFRESH: u32 = 2;
const M_BARS: u32 = 3;
const M_RING: u32 = 4;
const M_TEXT: u32 = 5;
const M_RESET_POS: u32 = 6;
const M_DATA: u32 = 7;
const M_QUIT: u32 = 8;

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
    let store = Arc::new(Store::load());
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
        let host = CreateWindowExW(WINDOW_EX_STYLE(0), HOST_CLASS, w!("Claude usage"), WS_OVERLAPPED,
                                   0, 0, 0, 0, None, None, Some(hinst.into()), None)?;
        let pill = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_NOREDIRECTIONBITMAP,
            w!("ClaudeUsagePillClass"), w!("ClaudeUsagePill"), WS_POPUP,
            0, 0, 100, 40, Some(host), None, Some(hinst.into()), None)?;
        let tooltip = CreateWindowExW(WS_EX_TOPMOST, TOOLTIPS_CLASSW, None, WINDOW_STYLE(TTS_ALWAYSTIP | TTS_NOPREFIX),
                                      CW_USEDEFAULT, CW_USEDEFAULT, CW_USEDEFAULT, CW_USEDEFAULT, Some(pill), None, Some(hinst.into()), None)
            .unwrap_or_default();
        let light = want_light(&store);
        let mut app = App {
            gfx, store: store.clone(), history, limits: limits.clone(), demo, host, pill, surface: None,
            tooltip, tooltip_text: String::new(), view: PillView::new(), pos: (0, 0), scale: W::dpi_scale(pill),
            light, theme: if light { theme::light() } else { theme::dark() },
            model: build_model(&limits.snapshot(), &History::new(vec![], demo), 120.0, now()),
            drawn_key: String::new(), drag: None, tray: Tray::new(host), last_tick: now(), tick: 0,
            hidden_fs: false, reset_refreshed: HashSet::new(), anim_on: false,
            taskbar_created: RegisterWindowMessageW(w!("TaskbarCreated")),
        };
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
        logf!("started (rust{})", if demo { ", demo" } else { "" });

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    Ok(())
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
        self.view.sync(&self.model);
        let key = pill_key(&self.model, &self.view.style, self.view.dim_setting, self.light);
        if force || key != self.drawn_key {
            self.drawn_key = key;
            self.draw();
        }
        self.set_tooltip(&PillView::tooltip(&self.model));
        self.update_tray();
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
            None => (None, NONE, "Claude usage — run `claude` then /login".to_string()),
        };
        self.tray.update(&self.gfx, pct, color, &tip);
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
            self.drawn_key.clear();
        }
        self.push(false);
        self.reset_boundary_refresh();
        self.fullscreen_check();
        if !self.hidden_fs && self.drag.is_none() {
            W::set_topmost(self.pill);
        }
        self.tick += 1;
        if self.tick % 15 == 0 && self.drag.is_none() {
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
        let fs = W::foreground_is_fullscreen(&[self.pill, self.host], mon);
        if fs && !self.hidden_fs {
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
            MenuItem::Item { id: M_SHOW, text: "Show details (coming in phase 2)".into(), checked: false, enabled: false },
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
            item(M_REFRESH, "Refresh now", false),
            item(M_RESET_POS, "Reset position", false),
            item(M_DATA, "Open data folder", false),
            MenuItem::Sep,
            item(M_QUIT, "Quit", false),
        ]
    }

    fn on_menu(&mut self, id: u32) {
        match id {
            M_REFRESH => self.limits.refresh_now(),
            M_BARS | M_RING | M_TEXT => {
                let s = match id { M_BARS => "bars", M_RING => "ring", _ => "text" };
                let mut ch = serde_json::Map::new();
                ch.insert("pill_style".into(), json!(s));
                self.store.update_cfg(&ch);
                self.place_pill();
                self.push(true);
            }
            M_RESET_POS => self.reset_position(),
            M_DATA => W::open_folder(&data_dir()),
            M_QUIT => self.quit(),
            _ => {}
        }
    }

    fn toggle_card(&mut self) {
        // The details card arrives in phase 2.
    }

    fn quit(&mut self) {
        logf!("quit");
        self.tray.remove();
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
        if !d.moved && dx.abs() + dy.abs() > threshold {
            d.moved = true;
        }
        if d.moved {
            self.pos = (d.start_pos.0 + dx, d.start_pos.1 + dy);
            W::place(self.pill, self.pos.0, self.pos.1, None);
        }
    }

    fn on_lbutton_up(&mut self) {
        unsafe {
            let _ = ReleaseCapture();
        }
        let Some(d) = self.drag.take() else { return };
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
    let Some((items, above)) = with_app(|a| {
        if which == "tray" { (a.tray_menu(), None) } else { (a.pill_menu(), Some(a.pos.1 - a.px(6.0))) }
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
            with_app(|a| a.toggle_card());
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
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
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
            with_app(|a| a.place_pill());
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
