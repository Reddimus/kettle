#!/usr/bin/env python3
"""Check the Linux clipboard URI backport without touching a desktop clipboard."""

import platform
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parent.parent


def run(*args, timeout=600):
    subprocess.run(["cargo", *args], cwd=ROOT, check=True, timeout=timeout)


def main():
    run("fmt", "--manifest-path", "vendor/Cargo.toml", "-p", "arboard", "--check", timeout=60)
    if platform.system() != "Linux":
        print("SKIP: clipboard URI unit tests require Linux; native Linux CI runs them")
        return
    common = [
        "--locked", "--manifest-path", "vendor/Cargo.toml",
        "--target-dir", "target/vendor-check", "-p", "arboard",
    ]
    run("test", *common, "--lib", "platform::linux::")
    run("clippy", *common, "--all-targets", "--", "-D", "warnings")


if __name__ == "__main__":
    main()
