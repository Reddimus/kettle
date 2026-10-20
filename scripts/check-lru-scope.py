#!/usr/bin/env python3
"""Guard the reviewed RUSTSEC-2026-0253 dependency scope."""

from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parent.parent
EXPECTED_REVERSE_EDGES = {
    ("glyphon", "lru"),
    ("kettle-render", "glyphon"),
    ("kettle", "kettle-render"),
    ("kettle-ui", "kettle-render"),
    ("kettle", "kettle-ui"),
}
REVIEWED_VERSIONS = {
    "lru": {"0.16.4"},
    "glyphon": {"0.12.0"},
}
CRATES_IO_SOURCE = "registry+https://github.com/rust-lang/crates.io-index"
REVIEWED_SOURCES = {
    "lru": {CRATES_IO_SOURCE},
    "glyphon": {CRATES_IO_SOURCE},
    "kettle-render": {None},
    "kettle-ui": {None},
    "kettle": {None},
}
REVIEWED_WORKSPACE_PACKAGES = {"kettle-render", "kettle-ui", "kettle"}


from dependency_scope import metadata_command
from dependency_scope import scope_errors as check_scope


def scope_errors(metadata: dict[str, Any]) -> list[str]:
    return check_scope(
        metadata,
        {"lru"},
        EXPECTED_REVERSE_EDGES,
        REVIEWED_VERSIONS,
        REVIEWED_SOURCES,
        REVIEWED_WORKSPACE_PACKAGES,
    )


def main() -> int:
    environment = os.environ.copy()
    environment["CARGO_TERM_COLOR"] = "never"
    try:
        result = subprocess.run(
            metadata_command(),
            cwd=ROOT,
            check=False,
            capture_output=True,
            text=True,
            encoding="utf-8",
            errors="replace",
            env=environment,
        )
    except FileNotFoundError:
        print("error: cargo is required for the lru scope guard", file=sys.stderr)
        return 1
    if result.returncode != 0:
        print(result.stderr or result.stdout, file=sys.stderr)
        return result.returncode or 1
    try:
        metadata = json.loads(result.stdout)
    except json.JSONDecodeError as error:
        print(f"error: cargo metadata was not valid JSON: {error}", file=sys.stderr)
        return 1

    errors = scope_errors(metadata)
    if errors:
        print("::error::lru dependency scope changed.", file=sys.stderr)
        for error in errors:
            print(f"- {error}", file=sys.stderr)
        print(
            "\nRe-review RUSTSEC-2026-0253 before keeping its audit ignore.",
            file=sys.stderr,
        )
        return 1

    print(
        "lru scope OK: the locked all-target graph reaches reviewed lru 0.16.4 "
        "only through glyphon; the affected pop API is unreachable."
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
