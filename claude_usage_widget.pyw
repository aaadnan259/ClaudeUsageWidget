"""Claude Usage Widget -- entry point.  Run:  pythonw claude_usage_widget.pyw  [--demo]"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from cuw import winutil  # noqa: E402

winutil.init_process()
if not winutil.single_instance():
    sys.exit(0)

from cuw.app import App  # noqa: E402
from cuw.config import log  # noqa: E402

if __name__ == "__main__":
    try:
        App(os.path.abspath(__file__), demo="--demo" in sys.argv).run()
    except Exception:
        log.exception("crashed")
        raise
