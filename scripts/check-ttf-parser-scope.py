#!/usr/bin/env python3
"""Guard the temporary RUSTSEC-2026-0192 dependency exception."""

from __future__ import annotations

import os
import re
import subprocess
import sys


EXPECTED = [
    "ttf-parser",
    "fontdb",
    "cosmic-text",
    "glyphon",
    "kettle-render",
    "kettle",
    "kettle-ui",
    "kettle",
]
FORBIDDEN = ("owned_ttf_parser", "sctk-adwaita", "ab_glyph")


def main() -> int:
    environment = os.environ.copy()
    environment["CARGO_TERM_COLOR"] = "never"
    try:
        result = subprocess.run(
            [
                "cargo",
                "tree",
                "-q",
                "-i",
                "ttf-parser",
                "--prefix",
                "none",
                "--format",
                "{p}",
            ],
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            encoding="utf-8",
            errors="replace",
            env=environment,
        )
    except FileNotFoundError:
        print("error: cargo is required for the ttf-parser scope guard", file=sys.stderr)
        return 1

    tree = result.stdout.rstrip()
    if result.returncode != 0:
        if "did not match any packages" in tree:
            print("ttf-parser is no longer in the dependency graph.")
            print(
                "Remove the RUSTSEC-2026-0192 ignores from deny.toml and "
                "audit.yml, then close #36."
            )
            return 0
        print(tree, file=sys.stderr)
        return result.returncode or 1

    actual = []
    for line in tree.splitlines():
        match = re.match(r"^([A-Za-z0-9_.-]+) v[0-9]", line)
        if match:
            actual.append(match.group(1))

    if actual != EXPECTED:
        print("::error::ttf-parser dependency scope changed.", file=sys.stderr)
        print("\nExpected the only unresolved path to remain:", file=sys.stderr)
        print("\n".join(EXPECTED), file=sys.stderr)
        print("\nActual cargo tree path:", file=sys.stderr)
        print(tree, file=sys.stderr)
        print(
            "\nRe-review RUSTSEC-2026-0192 before keeping the advisory ignore.",
            file=sys.stderr,
        )
        return 1

    for package in FORBIDDEN:
        if package in tree:
            print(
                f"::error::{package} re-entered the ttf-parser dependency path",
                file=sys.stderr,
            )
            return 1

    print("ttf-parser scope OK: only glyphon -> cosmic-text -> fontdb remains.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
