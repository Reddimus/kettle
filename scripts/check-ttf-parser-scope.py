#!/usr/bin/env python3
"""Guard the temporary RUSTSEC-2026-0192 dependency exception."""

from __future__ import annotations

import json
import os
from pathlib import Path
import subprocess
import sys


ROOT = Path(__file__).resolve().parent.parent
EXPECTED = {
    ("fontdb", "ttf-parser"),
    ("cosmic-text", "fontdb"),
    ("glyphon", "cosmic-text"),
    ("kettle-render", "glyphon"),
    ("kettle-ui", "kettle-render"),
    ("kettle", "kettle-render"),
    ("kettle", "kettle-ui"),
}


def reverse_edges(metadata: dict) -> set[tuple[str, str]] | None:
    packages = {p["id"]: p["name"] for p in metadata["packages"]}
    reverse: dict[str, set[str]] = {}
    for node in metadata["resolve"]["nodes"]:
        for dependency in node["deps"]:
            reverse.setdefault(dependency["pkg"], set()).add(node["id"])
    reached = {pid for pid, name in packages.items() if name == "ttf-parser"}
    if not reached:
        return None
    pending = list(reached)
    edges = set()
    while pending:
        dependency = pending.pop()
        for consumer in reverse.get(dependency, set()):
            edges.add((packages[consumer], packages[dependency]))
            if consumer not in reached:
                reached.add(consumer)
                pending.append(consumer)
    if len({packages[pid] for pid in reached}) != len(reached):
        raise ValueError("multiple package identities on the reviewed path")
    return edges


def main() -> int:
    environment = os.environ.copy()
    environment["CARGO_TERM_COLOR"] = "never"
    try:
        result = subprocess.run(
            [
                "cargo",
                "metadata",
                "--locked",
                "--all-features",
                "--format-version",
                "1",
            ],
            cwd=ROOT,
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            encoding="utf-8",
            errors="replace",
            env=environment,
        )
    except FileNotFoundError:
        print("error: cargo is required for the ttf-parser scope guard", file=sys.stderr)
        return 1

    if result.returncode != 0:
        print(result.stderr or result.stdout, file=sys.stderr)
        return result.returncode or 1

    try:
        actual = reverse_edges(json.loads(result.stdout))
    except (ValueError, KeyError, TypeError) as error:
        print(f"error: invalid cargo metadata: {error}", file=sys.stderr)
        return 1
    if actual is None:
        print("ttf-parser is no longer in the dependency graph.")
        print(
            "Remove the RUSTSEC-2026-0192 ignores from deny.toml and "
            "audit.yml, then close #36."
        )
        return 0

    if actual != EXPECTED:
        print("::error::ttf-parser dependency scope changed.", file=sys.stderr)
        for consumer, dependency in sorted(EXPECTED - actual):
            print(f"missing reviewed edge: {consumer} -> {dependency}", file=sys.stderr)
        for consumer, dependency in sorted(actual - EXPECTED):
            print(f"new reverse edge: {consumer} -> {dependency}", file=sys.stderr)
        print(
            "\nRe-review RUSTSEC-2026-0192 before keeping the advisory ignore.",
            file=sys.stderr,
        )
        return 1

    print("ttf-parser scope OK: only glyphon -> cosmic-text -> fontdb remains.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
