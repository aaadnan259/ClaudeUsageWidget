"""System tray icon (pystray) with live ring icon and Windows notifications."""
from .config import log
from .winutil import ring_icon_image

try:
    import pystray
    HAVE_TRAY = True
except Exception:
    HAVE_TRAY = False


class Tray:
    def __init__(self, menu_items, on_default):
        """menu_items: list of (label, checked_fn | None, callback) or None for a separator."""
        self.icon, self.last_key = None, None
        if not HAVE_TRAY:
            return
        entries = [pystray.MenuItem("Show details", lambda: on_default(), default=True)]
        for it in menu_items:
            if it is None:
                entries.append(pystray.Menu.SEPARATOR)
            else:
                label, checked, cb = it
                entries.append(pystray.MenuItem(label, (lambda c: (lambda: c()))(cb),
                                                checked=(lambda f: (lambda _i: f()))(checked)
                                                if checked else None))
        self.icon = pystray.Icon("ClaudeUsageWidget", ring_icon_image(None, "#808080"),
                                 "Claude usage", pystray.Menu(*entries))
        self.icon.run_detached()

    def update(self, pct, color, tip):
        if not self.icon:
            return
        key = (None if pct is None else int(round(pct)), color)
        if key != self.last_key:
            self.icon.icon = ring_icon_image(pct, color)
            self.last_key = key
        if self.icon.title != tip[:127]:  # each change is a shell call; skip no-ops
            self.icon.title = tip[:127]

    def notify(self, title, msg):
        if self.icon:
            try:
                self.icon.notify(msg, title)
            except Exception as e:
                log.warning("notify failed: %r", e)

    def stop(self):
        if self.icon:
            try:
                self.icon.stop()
            except Exception:
                pass
