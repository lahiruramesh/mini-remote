#!/usr/bin/env python3
"""Build desktop packages for the machine running this script."""

import argparse
import os
from pathlib import Path
import plistlib
import shutil
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parent.parent
DIST = ROOT / "dist"
PACKAGE_NAMES = ("remote-connector", "remote-desktop-viewer")
APP_NAMES = ("MiniRemote Connector", "MiniRemote Viewer")
FONT_LICENSE = ROOT / "crates/desktop-viewer/assets/Inter-OFL.txt"
APP_IDS = ("local.miniremote.connector", "local.miniremote.viewer")
STATUS = ROOT / "scripts/status"


def run(*command):
    subprocess.run(command, cwd=ROOT, check=True)


def release_directory():
    target = Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target"))
    return ROOT / target / "release"


def atomic_copy(source, destination):
    destination.parent.mkdir(parents=True, exist_ok=True)
    handle, temporary = tempfile.mkstemp(dir=destination.parent, prefix=".miniremote-")
    os.close(handle)
    try:
        shutil.copy2(source, temporary)
        os.replace(temporary, destination)
    finally:
        Path(temporary).unlink(missing_ok=True)


def write_text(destination, contents):
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_text(contents, encoding="utf-8")


def make_mac_icon(output):
    output.mkdir(parents=True, exist_ok=True)
    icon = output / "mini-remote.icns"
    with tempfile.TemporaryDirectory() as temporary:
        iconset = Path(temporary) / "Mini Remote.iconset"
        iconset.mkdir()
        for base in (16, 32, 128, 256, 512):
            for scale in (1, 2):
                pixels = str(base * scale)
                suffix = "@2x" if scale == 2 else ""
                subprocess.run(["/usr/bin/sips", "-z", pixels, pixels,
                    str(ROOT / "assets/mini-remote.png"), "--out",
                    str(iconset / f"icon_{base}x{base}{suffix}.png")],
                    stdout=subprocess.DEVNULL, check=True)
        run("/usr/bin/iconutil", "-c", "icns", str(iconset), "-o", str(icon))
    return icon


def make_mac_bundle(output, package, name, identifier, app_icon):
    bundle = output / (name + ".app")
    macos = bundle / "Contents/MacOS"
    atomic_copy(release_directory() / package, macos / package)
    atomic_copy(app_icon, bundle / "Contents/Resources/mini-remote.icns")
    if package == "remote-desktop-viewer":
        atomic_copy(FONT_LICENSE, bundle / "Contents/Resources" / FONT_LICENSE.name)
    if package == "remote-connector":
        status_binary = macos / "miniremote-status"
        run("swiftc", "-O", "-framework", "AppKit", "-framework", "Foundation",
            "-o", str(status_binary), str(STATUS / "MacStatus.swift"))
        icon = ROOT / "assets/mini-remote.png"
        if icon.is_file():
            atomic_copy(icon, bundle / "Contents/Resources/mini-remote.png")
    information = {
        "CFBundleIdentifier": identifier,
        "CFBundleName": name,
        "CFBundleDisplayName": "Mini Remote Connector" if package == "remote-connector" else "Mini Remote",
        "CFBundleIconFile": "mini-remote.icns",
        "CFBundleExecutable": package,
        "CFBundlePackageType": "APPL",
        "CFBundleShortVersionString": "0.1.0",
        "CFBundleVersion": "1",
        "NSHighResolutionCapable": True,
    }
    if package == "remote-connector":
        information["LSUIElement"] = True
        information["NSScreenCaptureUsageDescription"] = (
            "Mini Remote shares your desktop with paired viewers on your Tailscale network."
        )
    with (bundle / "Contents/Info.plist").open("wb") as file:
        plistlib.dump(information, file)

    identity = os.environ.get("MINIREMOTE_CODESIGN_IDENTITY", "-")
    if package == "remote-connector":
        run("codesign", "--force", "--sign", identity, str(status_binary))
    run("codesign", "--force", "--sign", identity, str(bundle))
    run("codesign", "--verify", "--strict", str(bundle))
    return bundle


def install_mac_bundle(bundle, identifier):
    target = Path.home() / "Applications" / bundle.name
    if target.exists():
        with (target / "Contents/Info.plist").open("rb") as file:
            existing = plistlib.load(file)
        if existing.get("CFBundleIdentifier") != identifier:
            raise RuntimeError(f"Another application already exists at {target}")
    target.parent.mkdir(parents=True, exist_ok=True)
    run("ditto", str(bundle), str(target))
    run("codesign", "--verify", "--strict", str(target))
    print(f"Installed {target}")
    return target


def install_mac_connector_agent(bundle, autostart):
    home = Path.home()
    label = "local.miniremote.connector"
    autostart_plist = home / "Library/LaunchAgents" / (label + ".plist")
    plist = autostart_plist if autostart or autostart_plist.exists() else (
        home / "Library/Application Support/MiniRemote" / (label + ".plist")
    )
    log = home / "Library/Logs/MiniRemote/connector.log"
    plist.parent.mkdir(parents=True, exist_ok=True)
    log.parent.mkdir(parents=True, exist_ok=True)
    with plist.open("wb") as file:
        plistlib.dump({
            "Label": label,
            "ProgramArguments": [
                str(bundle / "Contents/MacOS/remote-connector"), "serve", "--clipboard",
            ],
            "EnvironmentVariables": {"PATH": f"{home}/.local/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin"},
            "RunAtLoad": True,
            "KeepAlive": True,
            "ProcessType": "Interactive",
            "StandardOutPath": str(log),
            "StandardErrorPath": str(log),
        }, file)
    if autostart:
        domain = f"gui/{os.getuid()}"
        subprocess.run(["launchctl", "bootout", f"{domain}/{label}"],
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=False)
        run("launchctl", "bootstrap", domain, str(plist))


def activate_mac_status(bundle):
    home = Path.home()
    label = "local.miniremote.status"
    helper = bundle / "Contents/MacOS/miniremote-status"
    plist = home / "Library/LaunchAgents" / (label + ".plist")
    plist.parent.mkdir(parents=True, exist_ok=True)
    with plist.open("wb") as file:
        plistlib.dump({
            "Label": label,
            "ProgramArguments": [str(helper)],
            "RunAtLoad": True,
            "ProcessType": "Interactive",
            "StandardOutPath": "/dev/null",
            "StandardErrorPath": "/dev/null",
        }, file)
    domain = f"gui/{os.getuid()}"
    subprocess.run(["launchctl", "bootout", f"{domain}/{label}"], stdout=subprocess.DEVNULL,
                   stderr=subprocess.DEVNULL, check=False)
    run("launchctl", "bootstrap", domain, str(plist))


def installed_linux_path(name):
    return Path.home() / ".local/bin" / name


def write_linux_connector_service(autostart):
    home = Path.home()
    config = Path(os.environ.get("XDG_CONFIG_HOME", home / ".config"))
    service = config / "systemd/user/miniremote-connector.service"
    write_text(service, """[Unit]
Description=Mini Remote desktop connector
After=graphical-session.target

[Service]
Type=simple
ExecStart=%h/.local/bin/remote-connector serve --clipboard
Environment=PATH=%h/.local/bin:/usr/local/bin:/usr/bin:/bin
Restart=on-failure
RestartSec=3

[Install]
WantedBy=default.target
""")
    subprocess.run(["systemctl", "--user", "daemon-reload"], check=autostart)
    if autostart:
        run("systemctl", "--user", "enable", "--now", service.name)
        run("systemctl", "--user", "restart", service.name)


def install_linux_status(data):
    helper = Path.home() / ".local/lib/miniremote/miniremote-tray.py"
    atomic_copy(STATUS / "linux-tray.py", helper)
    icon = ROOT / "assets/mini-remote.svg"
    icon_dir = data / "icons/hicolor/scalable/apps"
    if icon.is_file():
        atomic_copy(icon, icon_dir / "mini-remote.svg")
    try:
        available = subprocess.run(
            ["python3", "-c", "import gi; gi.require_version('Gtk','3.0'); from gi.repository import Gtk;\ntry:\n gi.require_version('AyatanaAppIndicator3','0.1'); from gi.repository import AyatanaAppIndicator3\nexcept (ImportError, ValueError):\n gi.require_version('AppIndicator3','0.1'); from gi.repository import AppIndicator3"],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=False,
        ).returncode == 0
    except OSError:
        available = False
    if available:
        config = Path(os.environ.get("XDG_CONFIG_HOME", Path.home() / ".config"))
        escaped = str(helper).replace("\\", "\\\\").replace('"', '\\"').replace("$", "\\$").replace("`", "\\`")
        write_text(config / "autostart/miniremote-status.desktop", f"""[Desktop Entry]
Type=Application
Name=Mini Remote
Exec=python3 \"{escaped}\"
Icon=mini-remote
Terminal=false
X-GNOME-Autostart-enabled=true
""")
        print("Installed optional Mini Remote tray autostart")
    else:
        print("GTK 3/AppIndicator not found; Linux tray is optional and was not enabled")


def package_linux(install, autostart):
    output = DIST / "linux"
    atomic_copy(FONT_LICENSE, output / FONT_LICENSE.name)
    atomic_copy(STATUS / "linux-tray.py", output / "status/miniremote-tray.py")
    for package in PACKAGE_NAMES:
        source = release_directory() / package
        atomic_copy(source, output / package)
        if install:
            target = installed_linux_path(package)
            atomic_copy(source, target)
            print(f"Installed {target}")
    if install:
        home = Path.home()
        data = Path(os.environ.get("XDG_DATA_HOME", home / ".local/share"))
        atomic_copy(FONT_LICENSE, data / "miniremote" / FONT_LICENSE.name)
        executable = str(installed_linux_path("remote-desktop-viewer"))
        executable = executable.replace("\\", "\\\\").replace('"', '\\"').replace("$", "\\$").replace("`", "\\`")
        write_text(data / "applications/local.miniremote.viewer.desktop", f"""[Desktop Entry]
Type=Application
Name=Mini Remote
Exec="{executable}"
Icon=mini-remote
Terminal=false
Categories=Network;RemoteAccess;
""")
        write_linux_connector_service(autostart)
        install_linux_status(data)
    print(f"Package directory: {output}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--install", action="store_true", help="install into your account")
    parser.add_argument("--autostart", action="store_true", help="also start the connector at login")
    parser.add_argument("--skip-build", action="store_true", help="package an existing release build")
    options = parser.parse_args()
    if options.autostart and not options.install:
        parser.error("--autostart requires --install")
    if sys.platform not in ("darwin", "linux"):
        parser.error("only macOS and Linux are supported")
    if not options.skip_build:
        run("cargo", "build", "--locked", "--release", *(
            element for package in PACKAGE_NAMES for element in ("-p", package)
        ))

    if sys.platform == "darwin":
        output = DIST / "macos"
        installed = []
        app_icon = make_mac_icon(output)
        for package, name, identifier in zip(PACKAGE_NAMES, APP_NAMES, APP_IDS):
            bundle = make_mac_bundle(output, package, name, identifier, app_icon)
            if options.install:
                installed.append(install_mac_bundle(bundle, identifier))
        if options.install:
            install_mac_connector_agent(installed[0], options.autostart)
            activate_mac_status(installed[0])
        print(f"Package directory: {output}")
    else:
        package_linux(options.install, options.autostart)


if __name__ == "__main__":
    main()
