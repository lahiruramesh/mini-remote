#!/usr/bin/env python3
"""Optional Mini Remote StatusNotifier tray icon (GTK 3 + AppIndicator)."""

import os
from pathlib import Path
import subprocess
import sys

try:
    import gi
    gi.require_version("Gtk", "3.0")
    from gi.repository import GLib, Gtk
    try:
        gi.require_version("AyatanaAppIndicator3", "0.1")
        from gi.repository import AyatanaAppIndicator3 as AppIndicator
    except (ImportError, ValueError):
        gi.require_version("AppIndicator3", "0.1")
        from gi.repository import AppIndicator3 as AppIndicator
except (ImportError, ValueError) as error:
    print(f"Mini Remote tray unavailable: {error}", file=sys.stderr)
    raise SystemExit(0)

HOME = Path.home()
VIEWER = HOME / ".local/bin/remote-desktop-viewer"
UNIT = "miniremote-connector.service"
ICON = "mini-remote"


def call(*args):
    return subprocess.run(args, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=False)


def active():
    return call("systemctl", "--user", "is-active", "--quiet", UNIT).returncode == 0


def open_viewer(*args):
    subprocess.Popen([str(VIEWER), *args], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)


class Tray:
    def __init__(self):
        self.indicator = AppIndicator.Indicator.new(
            "mini-remote", ICON, AppIndicator.IndicatorCategory.APPLICATION_STATUS
        )
        data = Path(os.environ.get("XDG_DATA_HOME", HOME / ".local/share"))
        icon_dir = data / "icons/hicolor/scalable/apps"
        if icon_dir.exists():
            self.indicator.set_icon_theme_path(str(icon_dir))
        self.indicator.set_title("Mini Remote")
        self.indicator.set_status(AppIndicator.IndicatorStatus.ACTIVE)
        self.menu = Gtk.Menu()
        self.status = Gtk.MenuItem(label="Mini Remote — checking…")
        self.status.set_sensitive(False)
        self.menu.append(self.status)
        self.menu.append(Gtk.SeparatorMenuItem())
        self.toggle = Gtk.MenuItem(label="Start Connector")
        self.toggle.connect("activate", self.toggle_service)
        self.menu.append(self.toggle)
        self.restart = Gtk.MenuItem(label="Restart Connector")
        self.restart.connect("activate", lambda *_: self.service("restart"))
        self.menu.append(self.restart)
        self.menu.append(Gtk.SeparatorMenuItem())
        self.add("Open Viewer", lambda: open_viewer())
        self.add("Pair Phone…", lambda: open_viewer("--pair-phone"))
        self.add("Setup…", lambda: open_viewer("--setup"))
        self.menu.append(Gtk.SeparatorMenuItem())
        self.add("Quit Status Icon", lambda: Gtk.main_quit())
        self.menu.show_all()
        self.indicator.set_menu(self.menu)
        self.refresh()
        GLib.timeout_add_seconds(3, self.refresh)

    def add(self, title, callback):
        item = Gtk.MenuItem(label=title)
        item.connect("activate", lambda *_: callback())
        self.menu.append(item)

    def service(self, action):
        call("systemctl", "--user", action, UNIT)
        self.refresh()

    def toggle_service(self, *_):
        self.service("stop" if active() else "start")

    def refresh(self):
        running = active()
        self.status.set_label("Mini Remote — Running" if running else "Mini Remote — Stopped")
        self.toggle.set_label("Stop Connector" if running else "Start Connector")
        self.restart.set_sensitive(running)
        return True



Tray()
Gtk.main()
