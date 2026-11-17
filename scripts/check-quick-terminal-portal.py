#!/usr/bin/env python3
"""Opt-in GlobalShortcuts fixture. Never connects to the account's desktop bus."""
import argparse
import json
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import time
import unittest

SERVICES = (
    "org.freedesktop.portal.Desktop",
    "org.freedesktop.impl.portal.desktop.gnome",
    "org.freedesktop.impl.portal.desktop.gtk",
    "org.freedesktop.impl.portal.PermissionStore",
    "ca.desrt.dconf",
    "org.gnome.Settings.GlobalShortcutsProvider",
    "org.a11y.Bus",
)


def private_environment(root):
    root = Path(root).resolve()
    env = {
        "PATH": "/usr/local/bin:/usr/bin:/bin",
        "LANG": "en_US.UTF-8",
        "HOME": str(root / "home"),
        "XDG_RUNTIME_DIR": str(root / "run"),
        "XDG_CONFIG_HOME": str(root / "config"),
        "XDG_CACHE_HOME": str(root / "cache"),
        "XDG_STATE_HOME": str(root / "state"),
        "XDG_DATA_HOME": str(root / "data"),
        "XDG_DATA_DIRS": "/usr/local/share:/usr/share",
        "KETTLE_TEST_OS_CTL_DIR": str(root / "os-ctl"),
        "TMPDIR": str(root / "tmp"),
        "XDG_CURRENT_DESKTOP": "GNOME",
        "XDG_SESSION_TYPE": "wayland",
        "WAYLAND_DISPLAY": "kettle-portal-test",
        "GDK_BACKEND": "wayland",
        "DBUS_SESSION_BUS_ADDRESS": f"unix:path={root / 'bus'}",
        # A headless fixture must not reach the owner's login/system services.
        "DBUS_SYSTEM_BUS_ADDRESS": f"unix:path={root / 'bus'}",
    }
    validate_environment(root, env)
    return env


def validate_environment(root, env):
    root = Path(root).resolve()
    for key in (
        "HOME", "XDG_RUNTIME_DIR", "XDG_CONFIG_HOME", "XDG_CACHE_HOME",
        "XDG_STATE_HOME", "XDG_DATA_HOME", "KETTLE_TEST_OS_CTL_DIR", "TMPDIR",
    ):
        if not Path(env[key]).resolve().is_relative_to(root):
            raise ValueError(f"{key} escapes the fixture")
    expected = f"unix:path={root / 'bus'}"
    if any(env[key] != expected for key in (
        "DBUS_SESSION_BUS_ADDRESS", "DBUS_SYSTEM_BUS_ADDRESS",
    )):
        raise ValueError("D-Bus address escapes the fixture")
    if "DISPLAY" in env or Path(env["WAYLAND_DISPLAY"]).is_absolute():
        raise ValueError("display escapes the fixture")


def focus_observations_pass(observations):
    required = {
        "before": {"quick_minimized": True, "other_focused": True},
        "summoned": {"quick_minimized": False, "quick_focused": True, "other_focused": False},
        "dismissed": {"quick_minimized": True, "quick_focused": False, "other_focused": True},
        "withheld": {"quick_focused": False},
    }
    return all(
        observations.get(stage, {}).get(key) is expected
        for stage, fields in required.items() for key, expected in fields.items()
    )


def execute(binary, seed, seconds):
    binary = Path(binary).resolve(strict=True)
    if any(ord(c) < 32 or ord(c) == 127 for c in str(binary)):
        raise ValueError("binary path contains control characters")
    with tempfile.TemporaryDirectory(prefix="kettle-portal-live-") as directory:
        root = Path(directory)
        env = private_environment(root)
        for key in (
            "HOME", "XDG_RUNTIME_DIR", "XDG_CONFIG_HOME", "XDG_CACHE_HOME",
            "XDG_STATE_HOME", "XDG_DATA_HOME", "KETTLE_TEST_OS_CTL_DIR", "TMPDIR",
        ):
            Path(env[key]).mkdir(mode=0o700, parents=True, exist_ok=True)
        services = root / "services"
        services.mkdir(mode=0o700)
        for name in SERVICES:
            source = Path("/usr/share/dbus-1/services") / f"{name}.service"
            if not source.exists():
                if name.endswith("desktop.gtk") or name == "org.a11y.Bus":
                    continue
                raise RuntimeError(f"required service missing: {source}")
            # Exclude SystemdService: activation must stay on this private daemon.
            entries = [line for line in source.read_text().splitlines()
                       if line.startswith(("Name=", "Exec="))]
            (services / source.name).write_text("[D-BUS Service]\n" + "\n".join(entries) + "\n")
        config = root / "bus.conf"
        config.write_text(
            "<busconfig><type>session</type>"
            f"<listen>{env['DBUS_SESSION_BUS_ADDRESS']}</listen>"
            f"<servicedir>{services}</servicedir>"
            '<auth>EXTERNAL</auth><policy context="default">'
            '<allow user="*"/><allow own="*"/><allow send_destination="*"/>'
            '<allow receive_sender="*"/></policy></busconfig>'
        )
        applications = root / "data/applications"
        applications.mkdir(mode=0o700)
        # Desktop Entry Exec quoting, independent of shell interpretation.
        executable = str(binary).replace("\\", "\\\\").replace('"', '\\"').replace("`", "\\`").replace("$", "\\$").replace("%", "%%")
        (applications / "kettle.desktop").write_text(
            "[Desktop Entry]\nType=Application\nName=Kettle\n"
            f'Exec="{executable}"\nTryExec={binary}\nStartupNotify=true\n'
        )
        kettle_config = root / "config/kettle"
        kettle_config.mkdir(mode=0o700)
        (kettle_config / "config").write_text("quick-terminal-hotkey = ctrl+grave\n")
        children = []
        logs = []

        def start(argv, name):
            validate_environment(root, env)
            log = (root / f"{name}.log").open("w")
            logs.append(log)
            process = subprocess.Popen(argv, env=env, stdin=subprocess.DEVNULL,
                                       stdout=log, stderr=log, start_new_session=True)
            children.append(process)
            return process

        def wait_path(path, process):
            until = time.monotonic() + 20
            while not path.exists():
                if process.poll() is not None or time.monotonic() > until:
                    raise RuntimeError(f"fixture did not start: {path}; inspect {root}")
                time.sleep(0.05)

        try:
            bus = start(["dbus-daemon", f"--config-file={config}", "--nofork", "--nopidfile"], "bus")
            wait_path(root / "bus", bus)
            shell = start(["gnome-shell", "--headless", "--wayland", "--virtual-monitor", "1600x900",
                           "--wayland-display", env["WAYLAND_DISPLAY"]], "shell")
            wait_path(Path(env["XDG_RUNTIME_DIR"]) / env["WAYLAND_DISPLAY"], shell)
            if seed:
                commands = [
                    ["gsettings", "set", "org.gnome.settings-daemon.global-shortcuts", "applications", "['kettle']"],
                    ["gsettings", "set", "org.gnome.settings-daemon.global-shortcuts.application:/org/gnome/settings-daemon/global-shortcuts/kettle/",
                     "shortcuts", "[('toggle_quick_terminal', {'description': <'Toggle quick terminal'>, 'shortcuts': <['<Control>grave']>})]"],
                ]
                for command in commands:
                    validate_environment(root, env)
                    subprocess.run(command, env=env, check=True, timeout=10)
            start([str(binary), "--new-process", "--config", str(kettle_config / "config")], "kettle")
            environment_file = root / "environment.json"
            environment_file.write_text(json.dumps(env, indent=2) + "\n")
            environment_file.chmod(0o600)
            print(f"Private fixture: {root}", flush=True)
            print(f"Environment for private probes: {environment_file}", flush=True)
            print("Follow docs/TESTING.md's Wayland portal checklist. Logs stay private.", flush=True)
            time.sleep(seconds)
        finally:
            for process in reversed(children):
                if process.poll() is None:
                    os.killpg(process.pid, signal.SIGTERM)
            for process in reversed(children):
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait()
            for log in logs:
                log.close()


class IsolationTests(unittest.TestCase):
    def test_first_map_focus_alone_does_not_prove_summon_or_dismissal(self):
        self.assertFalse(focus_observations_pass({"summoned": {"quick_focused": True}}))
        complete = {
            "before": {"quick_minimized": True, "other_focused": True},
            "summoned": {"quick_minimized": False, "quick_focused": True, "other_focused": False},
            "dismissed": {"quick_minimized": True, "quick_focused": False, "other_focused": True},
            "withheld": {"quick_focused": False},
        }
        self.assertTrue(focus_observations_pass(complete))
        for stage, fields in complete.items():
            for key, value in fields.items():
                incomplete = {name: dict(values) for name, values in complete.items()}
                incomplete[stage][key] = not value
                self.assertFalse(focus_observations_pass(incomplete), (stage, key))

    def test_owner_environment_is_replaced_and_all_namespaces_are_private(self):
        # Resolved, as private_environment resolves it (/tmp is /private/tmp on macOS).
        root = Path("/tmp/kettle-portal-isolation-test").resolve()
        env = private_environment(root)
        self.assertEqual(env["XDG_RUNTIME_DIR"], str(root / "run"))
        self.assertEqual(env["KETTLE_TEST_OS_CTL_DIR"], str(root / "os-ctl"))
        self.assertNotIn("DISPLAY", env)
        self.assertNotIn("DBUS_STARTER_ADDRESS", env)
        self.assertNotIn("XDG_ACTIVATION_TOKEN", env)
        for key, value in (
            ("XDG_RUNTIME_DIR", "/run/user/1000"),
            ("KETTLE_TEST_OS_CTL_DIR", "/run/user/1000/kettle/ctl"),
            ("DBUS_SESSION_BUS_ADDRESS", "unix:path=/run/user/1000/bus"),
            ("DBUS_SYSTEM_BUS_ADDRESS", "unix:path=/run/dbus/system_bus_socket"),
        ):
            bad = dict(env, **{key: value})
            with self.assertRaises(ValueError, msg=key):
                validate_environment(root, bad)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--verify-observations", type=Path, help="check recorded focus states without desktop access")
    parser.add_argument("--execute", action="store_true", help="start only the private headless fixture")
    parser.add_argument("--binary", type=Path)
    parser.add_argument("--seed", action="store_true", help="preconfigure shortcuts in private dconf only")
    parser.add_argument("--seconds", type=int, default=300)
    args = parser.parse_args()
    if args.self_test:
        unittest.main(argv=[__file__])
    elif args.verify_observations:
        with args.verify_observations.open("rb") as source:
            data = source.read(65537)
        if len(data) > 65536 or not focus_observations_pass(json.loads(data)):
            parser.exit(1, "Focus observations incomplete or failed; no live success established.\n")
        print("Recorded focus observations satisfy the checklist; capture provenance remains required.")
    elif args.execute and args.binary and 1 <= args.seconds <= 3600:
        execute(args.binary, args.seed, args.seconds)
    else:
        parser.error("choose --self-test, or --execute --binary PATH [--seed]")
