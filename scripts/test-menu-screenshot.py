#!/usr/bin/env python3
"""Exercise screenshot process ownership without a desktop."""

import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("menu-screenshot.sh")
TOOLS = r'''
import json
import os
from pathlib import Path
import signal
import sys
import time

root = Path(os.environ["MENU_FIXTURE"])
tool = Path(sys.argv[0]).name
args = sys.argv[1:]
if tool == "kettle":
    if os.environ["MENU_MODE"] == "delayed":
        time.sleep(0.6)
    (root / "launch.json").write_text(json.dumps({"pid": os.getpid(), "args": args}))
    if "--new-process" in args:
        signal.pause()
    sys.exit(0)
if tool == "xdotool" and args[0] == "search" and "--pid" in args:
    deadline = time.monotonic() + 5
    while True:
        try:
            launch = json.loads((root / "launch.json").read_text())
            break
        except (FileNotFoundError, json.JSONDecodeError):
            if time.monotonic() >= deadline:
                sys.exit(1)
            time.sleep(0.01)
with (root / "calls.jsonl").open("a") as log:
    log.write(json.dumps([tool] + args) + "\n")
if tool == "xdotool":
    if args[0] == "search":
        if "--pid" not in args:
            print("999")
        else:
            count_file = root / "search-count"
            count = int(count_file.read_text()) + 1 if count_file.exists() else 1
            count_file.write_text(str(count))
            if (os.environ["MENU_MODE"] == "missing" or count < 3
                    or args[args.index("--pid") + 1] != str(launch["pid"])):
                sys.exit(1)
            print("123")
    elif args[0] == "getwindowgeometry":
        print("X=10\nY=20\nWIDTH=1280\nHEIGHT=720")
elif tool == "scrot":
    Path(args[-1]).write_bytes(b"fixture")
'''


@unittest.skipIf(os.name == "nt", "the screenshot harness uses bash and X11")
class MenuScreenshotTests(unittest.TestCase):
    def run_fixture(self, mode="ready"):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            (root / "scripts").mkdir()
            script = root / "scripts" / SCRIPT.name
            shutil.copyfile(SCRIPT, script)
            fake_bin = root / "bin"
            fake_bin.mkdir()
            kettle = root / "target" / "release" / "kettle"
            kettle.parent.mkdir(parents=True)
            for path in (kettle, fake_bin / "xdotool", fake_bin / "scrot"):
                path.write_text("#!" + sys.executable + "\n" + TOOLS)
                path.chmod(0o755)
            if mode == "missing":
                sleeper = fake_bin / "sleep"
                sleeper.write_text("#!/bin/sh\nexit 0\n")
                sleeper.chmod(0o755)
            environment = dict(os.environ, DISPLAY=":fixture", MENU_FIXTURE=str(root),
                               MENU_MODE=mode, PATH=str(fake_bin) + os.pathsep + os.environ["PATH"])
            process = subprocess.Popen(
                ["bash", str(script)], env=environment, stdout=subprocess.PIPE,
                stderr=subprocess.PIPE, text=True, start_new_session=True)
            try:
                stdout, stderr = process.communicate(timeout=30)
                launch = json.loads((root / "launch.json").read_text())
                calls = [json.loads(line) for line in (root / "calls.jsonl").read_text().splitlines()]
                try:
                    os.kill(launch["pid"], 0)
                    alive = True
                except ProcessLookupError:
                    alive = False
                return process.returncode, stderr, launch, calls, alive
            finally:
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                process.communicate()

    def test_waits_for_owned_window_and_reaps_fixture(self):
        code, stderr, launch, calls, alive = self.run_fixture()
        self.assertEqual(code, 0, stderr)
        self.assertEqual(launch["args"], ["--new-process"])
        searches = [call for call in calls if call[:2] == ["xdotool", "search"]]
        self.assertEqual(len(searches), 3)
        for call in searches:
            self.assertEqual(call[call.index("--pid") + 1], str(launch["pid"]))
        self.assertIn(["xdotool", "windowactivate", "--sync", "123"], calls)
        capture = next(call for call in calls if call[0] == "scrot")
        self.assertEqual(capture[1:4], ["-o", "--window", "123"])
        self.assertFalse(alive)

    def test_delayed_startup_keeps_three_readiness_polls(self):
        code, stderr, launch, calls, alive = self.run_fixture("delayed")
        self.assertEqual(code, 0, stderr)
        searches = [call for call in calls if call[:2] == ["xdotool", "search"]]
        self.assertEqual(len(searches), 3)
        self.assertFalse(alive)

    def test_no_owned_window_fails_without_capture(self):
        code, stderr, launch, calls, alive = self.run_fixture("missing")
        self.assertNotEqual(code, 0, stderr)
        self.assertFalse(any(call[0] == "scrot" for call in calls))
        self.assertFalse(alive)


if __name__ == "__main__":
    unittest.main()
