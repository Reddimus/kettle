#!/usr/bin/env python3
"""Compile every ```mermaid block in tracked Markdown, so a broken diagram
fails here instead of rendering as an error box on GitHub.

A diagram that does not parse is worse than no diagram: GitHub replaces it with
a red "Unable to render rich display" panel, which reads as a broken document
rather than a broken snippet. One shipped that way — a node label containing
backslash-escaped quotes — and nothing noticed, because nothing looked.

Two mermaid syntax traps this catches, both of which bit while it was written:

* `;` separates statements in a sequence diagram, so a literal semicolon in
  message text (`OSC 133;A`) truncates the line. Write `#59;` instead.
* Quotes inside a node label must be `&quot;`, not `\\"`.

Requires the mermaid CLI and a Chrome/Chromium for it to render in. Without
either, this SKIPS rather than fails: it is a documentation gate, and a
contributor with no Node toolchain should still be able to run the suite.
Set `KETTLE_MERMAID_REQUIRED=1` to turn a skip into a failure, which is what
CI does so the gate cannot quietly stop running.
"""

from __future__ import annotations

import argparse
import json
import re
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

MERMAID_CLI = "@mermaid-js/mermaid-cli@11"

# Where a Chrome that mermaid-cli can drive usually lives. puppeteer's own
# download is preferred when present; these are the fallbacks so the gate works
# on a developer machine that never ran `puppeteer browsers install`.
CHROME_CANDIDATES = (
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/Applications/Chromium.app/Contents/MacOS/Chromium",
    "/usr/bin/google-chrome",
    "/usr/bin/chromium",
    "/usr/bin/chromium-browser",
    "/snap/bin/chromium",
    r"C:\Program Files\Google\Chrome\Application\chrome.exe",
    r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
)


# A real fence starts a line. Prose that *mentions* ```mermaid inside a longer
# backtick span is not a diagram, and counting it inflates the total and invites
# a hunt for a block that does not exist.
FENCE = re.compile(r"^```mermaid\s*$", re.M)


def diagram_count(text: str) -> int:
    return len(FENCE.findall(text))


def tracked_markdown() -> list[Path]:
    out = subprocess.run(
        ["git", "ls-files", "*.md"], capture_output=True, text=True, check=True
    ).stdout.split()
    return [Path(p) for p in out]


def find_chrome() -> str | None:
    env = os.environ.get("PUPPETEER_EXECUTABLE_PATH") or os.environ.get(
        "CHROME_PATH"
    )
    if env and Path(env).exists():
        return env
    for candidate in CHROME_CANDIDATES:
        if Path(candidate).exists():
            return candidate
    return shutil.which("google-chrome") or shutil.which("chromium")


def skip_or_fail(reason: str) -> int:
    required = os.environ.get("KETTLE_MERMAID_REQUIRED") == "1"
    stream = sys.stderr if required else sys.stdout
    print(f"mermaid check: {'FAILED' if required else 'skipped'} — {reason}", file=stream)
    return 1 if required else 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--list", action="store_true", help="list files with diagrams and exit"
    )
    args = parser.parse_args()

    files = [p for p in tracked_markdown() if diagram_count(p.read_text(encoding="utf-8"))]
    blocks = sum(diagram_count(p.read_text(encoding="utf-8")) for p in files)
    print(f"mermaid check: {blocks} block(s) across {len(files)} file(s)")
    if args.list:
        for p in files:
            print(f"  {p}  ({diagram_count(p.read_text(encoding='utf-8'))})")
        return 0
    if not files:
        return 0

    if shutil.which("npx") is None:
        return skip_or_fail("npx not found")
    chrome = find_chrome()
    if chrome is None:
        return skip_or_fail("no Chrome/Chromium found for mermaid-cli")

    failures: list[tuple[Path, str]] = []
    with tempfile.TemporaryDirectory() as tmp:
        tmpdir = Path(tmp)
        # mermaid-cli reads its browser settings from a config file; passing the
        # executable explicitly avoids depending on a puppeteer download.
        config = tmpdir / "puppeteer.json"
        config.write_text(
            json.dumps(
                {
                    "executablePath": chrome,
                    "headless": True,
                    "args": ["--no-sandbox", "--disable-gpu", "--disable-dev-shm-usage"],
                }
            ),
            encoding="utf-8",
        )
        for source in files:
            # One invocation per FILE, not per block: mermaid-cli's Markdown mode
            # compiles every block in a single browser launch. Launching once per
            # block was flaky enough to report different files broken on
            # different runs, which is a gate that cannot be believed.
            staged = tmpdir / source.name
            staged.write_text(source.read_text(encoding="utf-8"), encoding="utf-8")
            rendered = tmpdir / f"rendered-{source.name}"
            result = subprocess.run(
                [
                    "npx", "--yes", MERMAID_CLI,
                    "-i", str(staged),
                    "-o", str(rendered),
                    "-p", str(config),
                    "-q",
                ],
                capture_output=True,
                text=True,
                cwd=tmpdir,
            )
            if result.returncode == 0:
                print(f"  ok   {source}")
                continue
            output = (result.stderr or "") + (result.stdout or "")
            if "ChromeLauncher" in output or "Failed to launch" in output:
                return skip_or_fail(f"mermaid-cli could not launch {chrome}")
            failures.append((source, output.strip()))
            print(f"  FAIL {source}")

    for source, output in failures:
        print(f"\n--- {source} ---", file=sys.stderr)
        for line in output.splitlines():
            if line.strip():
                print(f"    {line}", file=sys.stderr)
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
