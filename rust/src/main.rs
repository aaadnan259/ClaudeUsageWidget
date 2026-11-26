//! Claude Usage Widget — native Rust port. Entry point: process setup, single instance,
//! crash logging, then the app's message loop.
#![cfg_attr(not(test), windows_subsystem = "windows")]
// Drawing helpers take position, size, alignment, colour and runs; a parameter struct
// for each would only add noise.
#![allow(clippy::too_many_arguments)]

mod alerts;
mod app;
mod config;
mod data;
mod install;
mod model;
mod taskbar;
mod tray;
mod ui;
mod winutil;

use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError, LPARAM, WPARAM};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
use windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, FindWindowW, GetWindowThreadProcessId, MB_ICONERROR, MB_OK, MessageBoxW, PostMessageW,
};
use windows::core::{HSTRING, w};

/// After a crash on the UI thread, start a fresh copy (unless it also crashed within the
/// last 2 minutes, so a persistent bug can't turn into a restart loop).
fn restart_after_crash() {
    let mark = config::data_dir().join(".last_crash");
    let recent = std::fs::metadata(&mark).and_then(|m| m.modified()).ok()
        .and_then(|t| t.elapsed().ok()).is_some_and(|age| age.as_secs() < 120);
    let _ = std::fs::write(&mark, b"");
    if recent {
        logf!("ERROR crashed twice within 2 minutes: not restarting");
        return;
    }
    if let Ok(exe) = std::env::current_exe()
        && std::process::Command::new(exe).arg("--restarted").spawn().is_ok() {
            logf!("restarting after the crash");
        }
}

fn main() {
    let demo = std::env::args().any(|a| a == "--demo");
    let install = std::env::args().any(|a| a == "--install");
    let uninstall = std::env::args().any(|a| a == "--uninstall");
    let restarted = std::env::args().any(|a| a == "--restarted");
    std::panic::set_hook(Box::new(move |info| {
        logf!("ERROR panic: {info}");
        // Worker threads are replaced (watchdog) or recover on their own; only a crash on
        // the UI thread takes the widget down, so only that one restarts it.
        if !demo && std::thread::current().name() == Some("main") {
            restart_after_crash();
        }
    }));
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _ = SetCurrentProcessExplicitAppUserModelID(w!("ClaudeUsageWidget"));
        if install || uninstall {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            if install { install::install() } else { install::uninstall() }
            return;
        }
        // Same mutex as the Python app: the two can never run (and poll the API) together.
        // After a crash restart, give the old process a moment to finish exiting.
        let mut _mutex = CreateMutexW(None, false, w!("Local\\ClaudeUsageWidget"));
        let mut tries = 0;
        while restarted && GetLastError() == ERROR_ALREADY_EXISTS && tries < 20 {
            if let Ok(h) = _mutex {
                let _ = windows::Win32::Foundation::CloseHandle(h);
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
            _mutex = CreateMutexW(None, false, w!("Local\\ClaudeUsageWidget"));
            tries += 1;
        }
        if GetLastError() == ERROR_ALREADY_EXISTS {
            if let Ok(h) = FindWindowW(app::HOST_CLASS, None) {
                // This launch was the user's action, so pass the right to take focus on.
                let mut pid = 0u32;
                GetWindowThreadProcessId(h, Some(&mut pid));
                let _ = AllowSetForegroundWindow(pid);
                let _ = PostMessageW(Some(h), app::WM_APP_SHOW, WPARAM(0), LPARAM(0));
            }
            return;
        }
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        if let Err(e) = app::run(demo) {
            logf!("ERROR start-up failed: {e}");
            let text = HSTRING::from(format!(
                "Claude Usage Widget couldn't start:\n\n{e}\n\nDetails are in %APPDATA%\\ClaudeUsageWidget\\widget.log"
            ));
            MessageBoxW(None, &text, w!("Claude Usage Widget"), MB_OK | MB_ICONERROR);
        }
    }
}
