#!/usr/bin/env python3
"""Check the vendored clipboard patches.

The URI-list tests need no display. The X11 INCR test needs one and xclip, so
it runs under `xvfb-run` when both are installed and is skipped otherwise.
"""

import platform
from pathlib import Path
import shutil
import subprocess

ROOT = Path(__file__).resolve().parent.parent


def run(*args, timeout=600):
    subprocess.run(["cargo", *args], cwd=ROOT, check=True, timeout=timeout)


def main():
    run("fmt", "--manifest-path", "vendor/Cargo.toml", "-p", "arboard", "--check", timeout=60)
    common = [
        "--locked", "--manifest-path", "vendor/Cargo.toml",
        "--target-dir", "target/vendor-check", "-p", "arboard",
    ]
    if platform.system() != "Linux":
        print("SKIP: clipboard URI and INCR tests require Linux")
        if platform.system() == "Darwin":
            run("test", *common, "--lib", "platform::osx::tests::")
        run("clippy", *common, "--all-targets", "--", "-D", "warnings")
        return
    run("test", *common, "--lib", "platform::linux::")
    # The INCR test takes the clipboard, so it runs only on a private X
    # server; on a desktop the run above skips it.
    if shutil.which("xvfb-run") and shutil.which("xclip"):
        subprocess.run(
            ["xvfb-run", "-a", "env", "KETTLE_X11_CLIPBOARD_TEST=1", "cargo", "test", *common,
             "--lib", "platform::linux::x11::incr_tests", "--", "--nocapture"],
            cwd=ROOT, check=True, timeout=600,
        )
    else:
        print("SKIP: the X11 INCR test needs xvfb-run and xclip")
    run("clippy", *common, "--all-targets", "--", "-D", "warnings")


if __name__ == "__main__":
    main()
