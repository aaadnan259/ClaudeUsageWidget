"""Win32 helpers: windows, monitors, glass backdrop capture, autostart, and the
NativeHost window (taskbar button + progress overlay + right-click menu)."""
import base64
import ctypes
import ctypes.wintypes as wt
import io
import os
import sys
import threading
import winreg

from PIL import Image, ImageDraw, ImageEnhance, ImageFilter, ImageFont, ImageGrab

from .config import APP, DATA_DIR, log

user32 = ctypes.windll.user32
dwmapi = ctypes.windll.dwmapi
kernel32 = ctypes.windll.kernel32
ole32 = ctypes.windll.ole32
shell32 = ctypes.windll.shell32

LRESULT = ctypes.c_ssize_t
WNDPROC = ctypes.WINFUNCTYPE(LRESULT, wt.HWND, wt.UINT, wt.WPARAM, wt.LPARAM)

user32.DefWindowProcW.argtypes = [wt.HWND, wt.UINT, wt.WPARAM, wt.LPARAM]
user32.DefWindowProcW.restype = LRESULT
user32.SendMessageW.argtypes = [wt.HWND, wt.UINT, wt.WPARAM, wt.LPARAM]
user32.SendMessageW.restype = LRESULT
user32.PostMessageW.argtypes = [wt.HWND, wt.UINT, wt.WPARAM, wt.LPARAM]
user32.CreateWindowExW.restype = wt.HWND
user32.CreateWindowExW.argtypes = [wt.DWORD, wt.LPCWSTR, wt.LPCWSTR, wt.DWORD, ctypes.c_int,
                                   ctypes.c_int, ctypes.c_int, ctypes.c_int, wt.HWND, wt.HMENU,
                                   wt.HINSTANCE, wt.LPVOID]
user32.GetForegroundWindow.restype = wt.HWND
user32.MonitorFromWindow.restype = wt.HMONITOR
user32.MonitorFromWindow.argtypes = [wt.HWND, wt.DWORD]
user32.MonitorFromPoint.restype = wt.HMONITOR
user32.MonitorFromPoint.argtypes = [wt.POINT, wt.DWORD]
user32.SetWindowPos.argtypes = [wt.HWND, wt.HWND, ctypes.c_int, ctypes.c_int, ctypes.c_int,
                                ctypes.c_int, wt.UINT]
user32.GetAsyncKeyState.restype = ctypes.c_short
user32.LoadImageW.restype = wt.HANDLE
user32.LoadImageW.argtypes = [wt.HINSTANCE, wt.LPCWSTR, wt.UINT, ctypes.c_int, ctypes.c_int, wt.UINT]
user32.CreatePopupMenu.restype = wt.HMENU
user32.AppendMenuW.argtypes = [wt.HMENU, wt.UINT, ctypes.c_size_t, wt.LPCWSTR]
user32.TrackPopupMenu.argtypes = [wt.HMENU, wt.UINT, ctypes.c_int, ctypes.c_int, ctypes.c_int,
                                  wt.HWND, wt.LPVOID]
user32.GetWindowLongPtrW.restype = ctypes.c_ssize_t
user32.GetWindowLongPtrW.argtypes = [wt.HWND, ctypes.c_int]
user32.SetWindowLongPtrW.argtypes = [wt.HWND, ctypes.c_int, ctypes.c_ssize_t]
user32.GetDpiForWindow.argtypes = [wt.HWND]
user32.SetWindowDisplayAffinity.argtypes = [wt.HWND, wt.DWORD]


class MONITORINFO(ctypes.Structure):
    _fields_ = [("cbSize", wt.DWORD), ("rcMonitor", wt.RECT),
                ("rcWork", wt.RECT), ("dwFlags", wt.DWORD)]


def init_process():
    try:
        ctypes.windll.shcore.SetProcessDpiAwareness(2)
    except Exception:
        pass
    try:
        shell32.SetCurrentProcessExplicitAppUserModelID(APP)
    except Exception:
        pass


def single_instance():
    kernel32.CreateMutexW(None, False, "Local\\" + APP)
    return kernel32.GetLastError() != 183


# --------------------------------------------------------------------------- geometry
def cursor_pos():
    pt = wt.POINT()
    user32.GetCursorPos(ctypes.byref(pt))
    return pt.x, pt.y


def mouse_down(right_too=False):
    down = bool(user32.GetAsyncKeyState(0x01) & 0x8000)
    if right_too:
        down = down or bool(user32.GetAsyncKeyState(0x02) & 0x8000)
    return down


def monitor_info(x, y):
    mon = user32.MonitorFromPoint(wt.POINT(int(x), int(y)), 2)
    mi = MONITORINFO()
    mi.cbSize = ctypes.sizeof(MONITORINFO)
    user32.GetMonitorInfoW(mon, ctypes.byref(mi))
    r, w = mi.rcMonitor, mi.rcWork
    return mon, (r.left, r.top, r.right, r.bottom), (w.left, w.top, w.right, w.bottom)


def primary_work_area():
    r = wt.RECT()
    user32.SystemParametersInfoW(0x30, 0, ctypes.byref(r), 0)
    return r.left, r.top, r.right, r.bottom


def virtual_screen():
    x, y = user32.GetSystemMetrics(76), user32.GetSystemMetrics(77)
    return x, y, x + user32.GetSystemMetrics(78), y + user32.GetSystemMetrics(79)


def window_rect(hwnd):
    r = wt.RECT()
    user32.GetWindowRect(hwnd, ctypes.byref(r))
    return r.left, r.top, r.right, r.bottom


def dpi_scale(hwnd=None):
    try:
        if hwnd:
            return user32.GetDpiForWindow(hwnd) / 96.0
        return user32.GetDpiForSystem() / 96.0
    except Exception:
        return 1.0


def find_window(title):
    return user32.FindWindowW(None, title)


def place(hwnd, x, y, w=None, h=None, topmost=True):
    flags = 0x0010  # NOACTIVATE
    if w is None:
        flags |= 0x0001  # NOSIZE
    user32.SetWindowPos(hwnd, wt.HWND(-1 if topmost else 0), int(x), int(y),
                        int(w or 0), int(h or 0), flags)


def set_topmost(hwnd):
    user32.SetWindowPos(hwnd, wt.HWND(-1), 0, 0, 0, 0, 0x0002 | 0x0001 | 0x0010)


def style_popup(hwnd, dark=True):
    """Tool window (no taskbar / Alt-Tab entry), rounded corners, subtle border."""
    ex = user32.GetWindowLongPtrW(hwnd, -20)
    ex = (ex | 0x00000080) & ~0x00040000  # +WS_EX_TOOLWINDOW, -WS_EX_APPWINDOW
    user32.SetWindowLongPtrW(hwnd, -20, ex)
    try:
        v = ctypes.c_int(2)
        dwmapi.DwmSetWindowAttribute(hwnd, 33, ctypes.byref(v), 4)
        border = ctypes.c_uint(0x00504848 if dark else 0x00DADADA)
        dwmapi.DwmSetWindowAttribute(hwnd, 34, ctypes.byref(border), 4)
        d = ctypes.c_int(1 if dark else 0)
        dwmapi.DwmSetWindowAttribute(hwnd, 20, ctypes.byref(d), 4)
    except Exception:
        pass
    user32.SetWindowPos(hwnd, None, 0, 0, 0, 0, 0x0001 | 0x0002 | 0x0004 | 0x0010 | 0x0020)


def foreground_is_fullscreen(own_hwnds, on_monitor):
    h = user32.GetForegroundWindow()
    if not h or h in own_hwnds:
        return False
    cls = ctypes.create_unicode_buffer(256)
    user32.GetClassNameW(h, cls, 256)
    if cls.value in ("Progman", "WorkerW", "Shell_TrayWnd", "Shell_SecondaryTrayWnd"):
        return False
    mon = user32.MonitorFromWindow(h, 2)
    if mon != on_monitor:
        return False
    l, t, r, b = window_rect(h)
    mi = MONITORINFO()
    mi.cbSize = ctypes.sizeof(MONITORINFO)
    user32.GetMonitorInfoW(mon, ctypes.byref(mi))
    m = mi.rcMonitor
    return l <= m.left and t <= m.top and r >= m.right and b >= m.bottom


def windows_uses_light_theme():
    try:
        with winreg.OpenKey(winreg.HKEY_CURRENT_USER,
                            r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize") as k:
            return winreg.QueryValueEx(k, "SystemUsesLightTheme")[0] == 1
    except OSError:
        return False


# --------------------------------------------------------------------------- glass backdrop
def capture_backdrop(rect, light, exclude_hwnd=None):
    """Screenshot what's behind `rect`, frost it, return a JPEG data URL.
    exclude_hwnd is hidden from the capture for a moment so we don't blur ourselves."""
    l, t, r, b = rect
    if r - l < 4 or b - t < 4:
        return None
    try:
        if exclude_hwnd:
            user32.SetWindowDisplayAffinity(exclude_hwnd, 0x11)  # WDA_EXCLUDEFROMCAPTURE
        img = ImageGrab.grab(bbox=(l, t, r, b), all_screens=True).convert("RGB")
    except Exception as e:
        log.warning("capture failed: %r", e)
        return None
    finally:
        if exclude_hwnd:
            user32.SetWindowDisplayAffinity(exclude_hwnd, 0)
    w, h = img.size
    small = img.resize((max(8, w // 6), max(8, h // 6)), Image.BILINEAR)
    small = small.filter(ImageFilter.GaussianBlur(5))
    small = ImageEnhance.Color(small).enhance(1.5)
    small = ImageEnhance.Brightness(small).enhance(1.05 if light else 0.9)
    buf = io.BytesIO()
    small.save(buf, "JPEG", quality=80)
    return "data:image/jpeg;base64," + base64.b64encode(buf.getvalue()).decode()


# --------------------------------------------------------------------------- autostart
RUN_KEY = r"Software\Microsoft\Windows\CurrentVersion\Run"


def autostart_command(entry):
    exe = sys.executable
    if exe.lower().endswith("python.exe"):
        exe = exe[:-10] + "pythonw.exe"
    return f'"{exe}" "{entry}"'


def autostart_enabled():
    try:
        with winreg.OpenKey(winreg.HKEY_CURRENT_USER, RUN_KEY) as k:
            winreg.QueryValueEx(k, APP)
            return True
    except OSError:
        return False


def set_autostart(on, entry):
    with winreg.OpenKey(winreg.HKEY_CURRENT_USER, RUN_KEY, 0, winreg.KEY_SET_VALUE) as k:
        if on:
            winreg.SetValueEx(k, APP, 0, winreg.REG_SZ, autostart_command(entry))
        else:
            try:
                winreg.DeleteValue(k, APP)
            except OSError:
                pass


# --------------------------------------------------------------------------- icons
def ring_icon_image(pct, color, size=64):
    s = size * 2
    img = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    pad, wd = s // 16, s // 8
    d.ellipse((pad, pad, s - pad, s - pad), outline=(90, 90, 96, 255), width=wd)
    if pct is not None:
        rgb = tuple(int(color[i:i + 2], 16) for i in (1, 3, 5))
        d.arc((pad, pad, s - pad, s - pad), -90, -90 + 360 * min(pct, 100) / 100,
              fill=rgb + (255,), width=wd)
        txt = str(int(round(pct)))
        try:
            fnt = ImageFont.truetype("segoeuib.ttf", int(s * (0.42 if len(txt) < 3 else 0.32)))
        except OSError:
            fnt = ImageFont.load_default()
        d.text((s / 2, s / 2 + 2), txt, font=fnt, anchor="mm", fill=(255, 255, 255, 255),
               stroke_width=max(2, s // 32), stroke_fill=(0, 0, 0, 220))
    return img.resize((size, size), Image.LANCZOS)


# --------------------------------------------------------------------------- native host
class GUID(ctypes.Structure):
    _fields_ = [("d1", ctypes.c_ulong), ("d2", ctypes.c_ushort),
                ("d3", ctypes.c_ushort), ("d4", ctypes.c_ubyte * 8)]


def _guid(s):
    g = GUID()
    ole32.CLSIDFromString(s, ctypes.byref(g))
    return g


class WNDCLASSW(ctypes.Structure):
    _fields_ = [("style", wt.UINT), ("lpfnWndProc", WNDPROC), ("cbClsExtra", ctypes.c_int),
                ("cbWndExtra", ctypes.c_int), ("hInstance", wt.HINSTANCE), ("hIcon", wt.HICON),
                ("hCursor", wt.HANDLE), ("hbrBackground", wt.HBRUSH),
                ("lpszMenuName", wt.LPCWSTR), ("lpszClassName", wt.LPCWSTR)]


WM_APP_UPDATE = 0x8001
WM_APP_MENU = 0x8002


class NativeHost(threading.Thread):
    """A hidden Win32 window on its own thread. It provides:
    * an optional minimised taskbar button with a ring icon + ITaskbarList3 progress
      (clicking it calls on_click, closing it calls on_quit)
    * native right-click menus at the cursor."""
    NOPROGRESS, NORMAL, ERROR, PAUSED = 0, 2, 4, 8

    def __init__(self, show_button, on_click, on_quit):
        super().__init__(daemon=True)
        self.show_button, self.on_click, self.on_quit = show_button, on_click, on_quit
        self.hwnd = None
        self.ready = threading.Event()
        self.pending = None      # (pct, color, title)
        self.menu_items = None   # [(label, checked, callback) | None]
        self.tb = None
        self._icon_key = None
        self._icons = []

    # -- public (any thread)
    def update(self, pct, color, title):
        self.pending = (pct, color, title)
        if self.hwnd:
            user32.PostMessageW(self.hwnd, WM_APP_UPDATE, 0, 0)

    def popup_menu(self, items):
        self.menu_items = items
        if self.hwnd:
            user32.PostMessageW(self.hwnd, WM_APP_MENU, 0, 0)

    def close(self):
        if self.hwnd:
            user32.PostMessageW(self.hwnd, 0x0010, 0, 0)

    # -- thread
    def run(self):
        ole32.CoInitialize(None)
        self._wndproc = WNDPROC(self._proc)
        hinst = kernel32.GetModuleHandleW(None)
        wc = WNDCLASSW()
        wc.lpfnWndProc = self._wndproc
        wc.hInstance = hinst
        wc.lpszClassName = APP + "Host"
        wc.hCursor = user32.LoadCursorW(None, 32512)
        user32.RegisterClassW(ctypes.byref(wc))
        self.msg_tb_created = user32.RegisterWindowMessageW("TaskbarButtonCreated")
        self.hwnd = user32.CreateWindowExW(0, wc.lpszClassName, "Claude usage",
                                           0x00CF0000,  # WS_OVERLAPPEDWINDOW
                                           -32000, -32000, 120, 40, None, None, hinst, None)
        self._init_taskbar()
        if self.show_button:
            user32.ShowWindow(self.hwnd, 7)  # SW_SHOWMINNOACTIVE
        self.ready.set()
        msg = wt.MSG()
        while user32.GetMessageW(ctypes.byref(msg), None, 0, 0) > 0:
            user32.TranslateMessage(ctypes.byref(msg))
            user32.DispatchMessageW(ctypes.byref(msg))

    def _init_taskbar(self):
        try:
            p = ctypes.c_void_p()
            hr = ole32.CoCreateInstance(ctypes.byref(_guid("{56FDF344-FD6D-11d0-958A-006097C9A090}")),
                                        None, 1,
                                        ctypes.byref(_guid("{EA1AFB91-9E28-4B86-90E9-9E9F8A5EEFAF}")),
                                        ctypes.byref(p))
            if hr == 0 and p.value:
                self.tb = p
                self._tb_call(3, [])
        except Exception as e:
            log.warning("ITaskbarList3: %r", e)

    def _tb_call(self, idx, argtypes, *args):
        vtbl = ctypes.cast(self.tb, ctypes.POINTER(ctypes.POINTER(ctypes.c_void_p))).contents
        fn = ctypes.WINFUNCTYPE(ctypes.c_long, ctypes.c_void_p, *argtypes)(vtbl[idx])
        return fn(self.tb, *args)

    def _apply(self, force=False):
        if not self.pending or not self.show_button:
            return
        pct, color, title = self.pending
        user32.SetWindowTextW(self.hwnd, title)
        key = (None if pct is None else int(round(pct)), color)
        if key != self._icon_key or force:
            self._icon_key = key
            path = os.path.join(DATA_DIR, "taskbar.ico")
            img = ring_icon_image(pct, color, 64)
            img.save(path, sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64)])
            big = user32.LoadImageW(None, path, 1, 32, 32, 0x10)
            small = user32.LoadImageW(None, path, 1, 16, 16, 0x10)
            user32.SendMessageW(self.hwnd, 0x0080, 1, big or 0)
            user32.SendMessageW(self.hwnd, 0x0080, 0, small or 0)
            for old in self._icons:
                user32.DestroyIcon(old)
            self._icons = [i for i in (big, small) if i]
        if self.tb:
            try:
                if pct is None:
                    self._tb_call(10, [wt.HWND, ctypes.c_int], self.hwnd, self.NOPROGRESS)
                else:
                    st = self.ERROR if pct >= 90 else self.PAUSED if pct >= 75 else self.NORMAL
                    self._tb_call(10, [wt.HWND, ctypes.c_int], self.hwnd, st)
                    self._tb_call(9, [wt.HWND, ctypes.c_ulonglong, ctypes.c_ulonglong],
                                  self.hwnd, max(1, int(pct)), 100)
            except Exception as e:
                log.warning("progress: %r", e)

    def _menu(self):
        items = self.menu_items or []
        hm = user32.CreatePopupMenu()
        for i, it in enumerate(items):
            if it is None:
                user32.AppendMenuW(hm, 0x0800, 0, None)
            else:
                label, checked, _cb = it
                user32.AppendMenuW(hm, 0x0008 if checked else 0, i + 1, label)
        x, y = cursor_pos()
        user32.SetForegroundWindow(self.hwnd)
        cmd = user32.TrackPopupMenu(hm, 0x0100 | 0x0002, x, y, 0, self.hwnd, None)  # RETURNCMD|RIGHTBUTTON
        user32.DestroyMenu(hm)
        if cmd and items[cmd - 1]:
            threading.Thread(target=items[cmd - 1][2], daemon=True).start()

    def _proc(self, hwnd, msg, wp, lp):
        try:
            if msg == WM_APP_UPDATE:
                self._apply()
                return 0
            if msg == WM_APP_MENU:
                self._menu()
                return 0
            if msg == self.msg_tb_created:
                self._apply(force=True)
                return 0
            if msg == 0x0112 and (wp & 0xFFF0) in (0xF120, 0xF030):  # SC_RESTORE / SC_MAXIMIZE
                threading.Thread(target=self.on_click, daemon=True).start()
                return 0
            if msg == 0x0010:  # WM_CLOSE
                threading.Thread(target=self.on_quit, daemon=True).start()
                return 0
            if msg == 0x0002:  # WM_DESTROY
                user32.PostQuitMessage(0)
                return 0
        except Exception:
            log.exception("host proc")
        return user32.DefWindowProcW(hwnd, msg, wp, lp)
