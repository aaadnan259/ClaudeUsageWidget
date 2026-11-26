//! Claude Usage Widget — native Rust port. Entry point: process setup, single instance,
//! crash logging, then the app's message loop.
#![cfg_attr(not(test), windows_subsystem = "windows")]

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

fn main() {
    let demo = std::env::args().any(|a| a == "--demo");
    let install = std::env::args().any(|a| a == "--install");
    let uninstall = std::env::args().any(|a| a == "--uninstall");
    std::panic::set_hook(Box::new(|info| {
        logf!("ERROR panic: {info}");
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
        let _mutex = CreateMutexW(None, false, w!("Local\\ClaudeUsageWidget"));
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
