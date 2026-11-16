"""Claude Usage Widget -- entry point.  Run:  pythonw claude_usage_widget.pyw  [--demo]"""
import os
import sys
import time
import traceback

HERE = os.path.dirname(os.path.abspath(__file__))
STARTUP_LOG = os.path.join(os.environ.get("APPDATA", HERE), "ClaudeUsageWidget", "startup.log")


def _startup_note(msg):
    """Written before anything else can fail, so silent pythonw crashes leave a trace."""
    try:
        os.makedirs(os.path.dirname(STARTUP_LOG), exist_ok=True)
        with open(STARTUP_LOG, "a", encoding="utf-8") as f:
            f.write(f"{time.strftime('%Y-%m-%d %H:%M:%S')} [{os.getpid()}] {msg}\n")
    except OSError:
        pass


def _fatal(title, detail):
    _startup_note(f"FATAL {title}\n{detail}")
    try:
        import ctypes
        ctypes.windll.user32.MessageBoxW(None, f"{title}\n\n{detail[-1500:]}\n\nDetails: {STARTUP_LOG}",
                                         "Claude Usage Widget", 0x10)
    except Exception:
        pass
    sys.exit(1)


try:  # keep the startup log small
    if os.path.getsize(STARTUP_LOG) > 200_000:
        os.remove(STARTUP_LOG)
except OSError:
    pass
_startup_note(f"start exe={sys.executable} cwd={os.getcwd()} argv={sys.argv[1:]}")
os.chdir(HERE)  # behave the same however it's launched (double-click, shortcut, autostart)

# Windows may open .pyw files with any installed Python. Hand off to the project's own
# environment (.venv), which is guaranteed to have every package the widget needs.
VENV_DIR = os.path.join(HERE, ".venv")
VENV_PYW = os.path.join(VENV_DIR, "Scripts", "pythonw.exe")
if os.path.exists(VENV_PYW) and os.path.normcase(os.path.abspath(sys.prefix)) != os.path.normcase(VENV_DIR):
    import subprocess
    try:
        subprocess.Popen([VENV_PYW, os.path.abspath(__file__), *sys.argv[1:]], cwd=HERE,
                         creationflags=0x00000008 | 0x00000200)  # DETACHED_PROCESS | NEW_PROCESS_GROUP
        _startup_note(f"handed off to {VENV_PYW}")
        sys.exit(0)
    except OSError as e:
        _startup_note(f"handoff failed ({e}); continuing with {sys.executable}")
sys.path.insert(0, HERE)

try:
    from cuw import winutil
except Exception:
    _fatal("Couldn't load the widget.", traceback.format_exc())

winutil.init_process()
if not winutil.single_instance():
    # Already running: ask it to open its card instead of silently doing nothing.
    winutil.signal_running_instance()
    _startup_note("already running; signalled existing instance")
    sys.exit(0)

try:
    from cuw.app import App
    from cuw.config import log
except ImportError as e:
    _fatal(f"A required package is missing: {e.name}",
           f"Run this once in a terminal:\n\n  \"{sys.executable}\" -m pip install --user -r "
           f"\"{os.path.join(HERE, 'requirements.txt')}\"\n\n{traceback.format_exc()}")
except Exception:
    _fatal("Couldn't load the widget.", traceback.format_exc())

if __name__ == "__main__":
    try:
        App(os.path.abspath(__file__), demo="--demo" in sys.argv).run()
    except Exception:
        log.exception("crashed")
        _fatal("The widget crashed.", traceback.format_exc())
