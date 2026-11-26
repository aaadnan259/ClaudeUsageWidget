//! `ClaudeUsageWidget.exe --install` / `--uninstall`: make the native build the installed
//! app. Copies the .exe (and icon) to %LOCALAPPDATA%\Programs\ClaudeUsageWidget, creates
//! Start menu + Desktop shortcuts carrying the app's AppUserModelID (so a pinned shortcut
//! and the running widget share one taskbar icon), points "Start with Windows" at it, then
//! closes a running widget (Python or Rust) and starts the installed one.
//! Going back to the Python app: run Install.bat again.

use crate::config::APP;
use crate::logf;
use crate::winutil::wide;
use std::path::{Path, PathBuf};
use windows::Win32::Foundation::{LPARAM, PROPERTYKEY, WPARAM};
use windows::Win32::Storage::EnhancedStorage::PKEY_AppUserModel_ID;
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree, IPersistFile};
use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
use windows::Win32::UI::Shell::{FOLDERID_Desktop, FOLDERID_Programs, IShellLinkW, KF_FLAG_DEFAULT, SHGetKnownFolderPath, ShellLink};
use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, MB_ICONERROR, MB_ICONINFORMATION, MB_OK, MessageBoxW, PostMessageW, WM_CLOSE};
use windows::core::{GUID, HSTRING, Interface, PCWSTR, w};

const NAME: &str = "Claude Usage Widget";
const ICON: &[u8] = include_bytes!("../../assets/app.ico");

fn known_folder(id: &GUID) -> Option<PathBuf> {
    unsafe {
        let p = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        s.map(PathBuf::from)
    }
}

/// Where things go. CUW_INSTALL_TEST=<dir> puts everything (shortcuts too) in that folder
/// and leaves the Run key and running widgets alone, for testing.
struct Targets {
    dir: PathBuf,
    shortcuts: Vec<PathBuf>,
    test: bool,
}

fn targets() -> Targets {
    if let Some(t) = std::env::var_os("CUW_INSTALL_TEST") {
        let dir = PathBuf::from(t);
        return Targets { shortcuts: vec![dir.join(format!("{NAME} (start).lnk")), dir.join(format!("{NAME} (desktop).lnk"))], dir, test: true };
    }
    let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_default();
    let mut shortcuts = Vec::new();
    for id in [&FOLDERID_Programs, &FOLDERID_Desktop] {
        if let Some(f) = known_folder(id) {
            shortcuts.push(f.join(format!("{NAME}.lnk")));
        }
    }
    Targets { dir: local.join("Programs").join(APP), shortcuts, test: false }
}

fn create_shortcut(lnk: &Path, exe: &Path, icon: &Path) -> windows::core::Result<()> {
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
        link.SetPath(&HSTRING::from(exe.as_os_str()))?;
        link.SetWorkingDirectory(&HSTRING::from(exe.parent().unwrap_or(exe).as_os_str()))?;
        link.SetIconLocation(&HSTRING::from(icon.as_os_str()), 0)?;
        link.SetDescription(&HSTRING::from("Live Claude plan limits above the taskbar"))?;
        let store: IPropertyStore = link.cast()?;
        let key: PROPERTYKEY = PKEY_AppUserModel_ID;
        store.SetValue(&key, &PROPVARIANT::from(APP))?;
        store.Commit()?;
        let file: IPersistFile = link.cast()?;
        file.Save(&HSTRING::from(lnk.as_os_str()), true)
    }
}

fn message(text: &str, error: bool) {
    // CUW_INSTALL_QUIET=1: log instead of showing a message box (scripted installs/tests).
    if std::env::var_os("CUW_INSTALL_TEST").is_some() || std::env::var_os("CUW_INSTALL_QUIET").is_some() {
        logf!("install: {text}");
        return;
    }
    unsafe {
        MessageBoxW(None, &HSTRING::from(text), w!("Claude Usage Widget"), MB_OK | if error { MB_ICONERROR } else { MB_ICONINFORMATION });
    }
}

/// Ask any running widget (Python or Rust: same host window class) to quit.
fn close_running() {
    unsafe {
        for _ in 0..20 {
            match FindWindowW(crate::app::HOST_CLASS, PCWSTR::null()) {
                Ok(h) if !h.is_invalid() => {
                    let _ = PostMessageW(Some(h), WM_CLOSE, WPARAM(0), LPARAM(0));
                    std::thread::sleep(std::time::Duration::from_millis(250));
                }
                _ => return,
            }
        }
    }
}

pub fn install() {
    let t = targets();
    let result = (|| -> Result<PathBuf, String> {
        std::fs::create_dir_all(&t.dir).map_err(|e| format!("create {}: {e}", t.dir.display()))?;
        let src = std::env::current_exe().map_err(|e| e.to_string())?;
        let exe = t.dir.join(format!("{APP}.exe"));
        let icon = t.dir.join("app.ico");
        if !t.test {
            close_running(); // the installed .exe may be the one running
        }
        if src.canonicalize().ok() != exe.canonicalize().ok() {
            std::fs::copy(&src, &exe).map_err(|e| format!("copy to {}: {e}", exe.display()))?;
        }
        std::fs::write(&icon, ICON).map_err(|e| format!("icon: {e}"))?;
        for lnk in &t.shortcuts {
            create_shortcut(lnk, &exe, &icon).map_err(|e| format!("shortcut {}: {e}", lnk.display()))?;
        }
        if !t.test {
            let cmd = wide(&format!("\"{}\"", exe.display()));
            unsafe {
                use windows::Win32::System::Registry::{HKEY_CURRENT_USER, REG_SZ, RegSetKeyValueW};
                RegSetKeyValueW(HKEY_CURRENT_USER, w!(r"Software\Microsoft\Windows\CurrentVersion\Run"), w!("ClaudeUsageWidget"),
                                REG_SZ.0, Some(cmd.as_ptr() as *const _), (cmd.len() * 2) as u32).ok().map_err(|e| format!("autostart: {e}"))?;
            }
            let _ = std::process::Command::new(&exe).current_dir(&t.dir).spawn();
        }
        Ok(exe)
    })();
    match result {
        Ok(exe) => {
            logf!("installed {}", exe.display());
            message(&format!("Installed to\n{}\n\nShortcuts are in the Start menu and on the Desktop, and it starts with Windows.\n\n\
                              To go back to the Python version, run Install.bat in the project folder.", exe.display()), false);
        }
        Err(e) => {
            logf!("ERROR install: {e}");
            message(&format!("Install failed: {e}"), true);
        }
    }
}

pub fn uninstall() {
    let t = targets();
    for lnk in &t.shortcuts {
        let _ = std::fs::remove_file(lnk);
    }
    if !t.test {
        let _ = crate::winutil::set_autostart(false);
    }
    logf!("uninstalled shortcuts and autostart");
    message(&format!("Removed the shortcuts and \"Start with Windows\".\n\nThe program is still in\n{}\n(delete it when the widget isn't running). Your settings and history stay in %APPDATA%\\{APP}.",
                     t.dir.display()), false);
}
