"""Installs the widget as an app: icon, Start menu + Desktop shortcuts, Start with Windows.

Run through Install.bat (which first builds .venv). Safe to run again at any time.
    .venv\\Scripts\\python.exe install.py            install / repair
    .venv\\Scripts\\python.exe install.py --remove   remove shortcuts + autostart
"""
import ctypes
import ctypes.wintypes as wt
import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

from PIL import Image, ImageDraw  # noqa: E402

from cuw import winutil as W  # noqa: E402
from cuw.config import APP  # noqa: E402

NAME = "Claude Usage Widget"
ENTRY = os.path.join(HERE, "claude_usage_widget.pyw")
PYTHONW = os.path.join(HERE, ".venv", "Scripts", "pythonw.exe")
ICON = os.path.join(HERE, "assets", "app.ico")


# --------------------------------------------------------------------------- icon
def make_icon(path):
    s = 1024
    img = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    d.rounded_rectangle((40, 40, s - 40, s - 40), radius=220, fill=(22, 23, 30, 255))
    # gradient ring (teal -> green), 300 degrees, like the widget
    box, width = (200, 200, s - 200, s - 200), 92
    d.ellipse(box, outline=(52, 54, 66, 255), width=width)  # solid: ImageDraw does not blend alpha
    steps = 120
    for i in range(steps):
        k = i / (steps - 1)
        c = tuple(int(a + (b - a) * k) for a, b in zip((94, 234, 212), (74, 222, 128)))
        a0 = -90 + 300 * i / steps
        d.arc(box, a0, a0 + 300 / steps + 1, fill=c + (255,), width=width)
    # orange spark in the middle
    import math
    cx = cy = s / 2
    for k in range(4):
        a = math.radians(k * 45)
        dx, dy = math.cos(a) * 150, math.sin(a) * 150
        d.line((cx - dx, cy - dy, cx + dx, cy + dy), fill=(232, 116, 59, 255), width=58)
    for k in range(8):  # round the ray ends
        a = math.radians(k * 45)
        x, y = cx + math.cos(a) * 150, cy + math.sin(a) * 150
        d.ellipse((x - 29, y - 29, x + 29, y + 29), fill=(232, 116, 59, 255))
    os.makedirs(os.path.dirname(path), exist_ok=True)
    img.save(path, sizes=[(16, 16), (20, 20), (24, 24), (32, 32), (40, 40), (48, 48),
                          (64, 64), (128, 128), (256, 256)])


# --------------------------------------------------------------------------- shortcuts
def known_folder(name):
    out = subprocess.run(["powershell", "-NoProfile", "-Command",
                          f"[Environment]::GetFolderPath('{name}')"],
                         capture_output=True, text=True, creationflags=0x08000000)
    return out.stdout.strip()


def create_lnk(lnk):
    ps = ("$s=(New-Object -ComObject WScript.Shell).CreateShortcut($env:CUW_LNK);"
          "$s.TargetPath=$env:CUW_TARGET;$s.Arguments=$env:CUW_ARGS;$s.WorkingDirectory=$env:CUW_DIR;"
          "$s.IconLocation=$env:CUW_ICON;$s.Description='Live Claude plan usage on your taskbar';$s.Save()")
    env = dict(os.environ, CUW_LNK=lnk, CUW_TARGET=PYTHONW, CUW_ARGS=f'"{ENTRY}"',
               CUW_DIR=HERE, CUW_ICON=ICON + ",0")
    subprocess.run(["powershell", "-NoProfile", "-Command", ps], env=env, check=True,
                   creationflags=0x08000000)
    set_lnk_app_id(lnk, APP)


class PROPERTYKEY(ctypes.Structure):
    _fields_ = [("fmtid", W.GUID), ("pid", wt.DWORD)]


class PROPVARIANT(ctypes.Structure):
    _fields_ = [("vt", ctypes.c_ushort), ("r1", ctypes.c_ushort), ("r2", ctypes.c_ushort),
                ("r3", ctypes.c_ushort), ("p", ctypes.c_void_p), ("pad", ctypes.c_void_p)]


def set_lnk_app_id(lnk, app_id):
    """Give the shortcut the same AppUserModelID as the running widget, so a pinned
    shortcut and the widget's taskbar button are one icon."""
    W.ole32.CoInitialize(None)
    shell32 = ctypes.windll.shell32
    shell32.SHGetPropertyStoreFromParsingName.argtypes = [wt.LPCWSTR, ctypes.c_void_p, ctypes.c_int,
                                                          ctypes.c_void_p, ctypes.c_void_p]
    ps = ctypes.c_void_p()
    hr = shell32.SHGetPropertyStoreFromParsingName(
        lnk, None, 2, ctypes.byref(W._guid("{886D8EEB-8CF2-4446-8D02-CDBA1DBDCF99}")), ctypes.byref(ps))
    if hr != 0 or not ps.value:
        print(f"  (couldn't set app id: 0x{hr & 0xFFFFFFFF:08X})")
        return
    vtbl = ctypes.cast(ps, ctypes.POINTER(ctypes.POINTER(ctypes.c_void_p))).contents
    call = lambda i, *types: ctypes.WINFUNCTYPE(ctypes.c_long, ctypes.c_void_p, *types)(vtbl[i])
    key = PROPERTYKEY(W._guid("{9F4C2855-9F79-4B39-A8D0-E1D42DE1D5F3}"), 5)  # PKEY_AppUserModel_ID
    buf = ctypes.create_unicode_buffer(app_id)
    pv = PROPVARIANT(31, 0, 0, 0, ctypes.cast(buf, ctypes.c_void_p), None)  # VT_LPWSTR
    call(6, ctypes.c_void_p, ctypes.c_void_p)(ps, ctypes.byref(key), ctypes.byref(pv))  # SetValue
    call(7)(ps)                                                                        # Commit
    call(2)(ps)                                                                        # Release


def main():
    remove = "--remove" in sys.argv
    lnks = [os.path.join(known_folder("Programs"), NAME + ".lnk"),
            os.path.join(known_folder("Desktop"), NAME + ".lnk")]
    if remove:
        for p in lnks:
            if os.path.exists(p):
                os.remove(p)
                print("removed", p)
        W.set_autostart(False, ENTRY)
        print("Start with Windows: off")
        return
    if not os.path.exists(PYTHONW):
        sys.exit("Missing .venv - run Install.bat instead.")
    if not os.path.exists(ICON):  # the designed icon (assets/make_icon.py) is kept; this is only a fallback
        make_icon(ICON)
    print("icon:", ICON)
    for p in lnks:
        create_lnk(p)
        print("shortcut:", p)
    # point Start with Windows at the private environment too
    import winreg
    with winreg.OpenKey(winreg.HKEY_CURRENT_USER, W.RUN_KEY, 0, winreg.KEY_SET_VALUE) as k:
        winreg.SetValueEx(k, APP, 0, winreg.REG_SZ, f'"{PYTHONW}" "{ENTRY}"')
    print("Start with Windows: on")
    print(f"\nDone. Open '{NAME}' from the Start menu or Desktop (you can pin it to the taskbar).")


if __name__ == "__main__":
    main()
