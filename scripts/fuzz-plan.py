#!/usr/bin/env python3
"""Plan a fuzz workflow run: which targets run, for how long, seeded from
which crates.

`fuzz/targets.json` lists every cargo-fuzz target with the crates whose
in-process sweeps write its seeds and how long it runs each night (and on
Sundays, when that differs). This prints the GitHub Actions matrix for one
run as `matrix=<json>`:

- a pull request runs every target for a minute;
- a night runs each for its nightly time, or its Sunday time on a Sunday,
  and Sunday's run also minimizes each corpus;
- a dispatch runs the targets it names (all when it names none) for the
  seconds it asks, held to five hours.

It refuses a table whose names differ from `fuzz/Cargo.toml`'s `[[bin]]`
targets, so a target cannot be added without a schedule or dropped without
leaving the table.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PULL_REQUEST_SECONDS = 60
DISPATCH_DEFAULT_SECONDS = 3600
DISPATCH_MAX_SECONDS = 18000
# Building, seeding and minimizing, on top of the run itself.
OVERHEAD_MINUTES = 30
SUNDAY = 6  # Python's weekday() numbering: Monday is 0.
NAME = re.compile(r"^[a-z][a-z0-9_]*$")


class PlanError(Exception):
    pass


def load_targets(table: str) -> list[dict]:
    try:
        targets = json.loads(table)["targets"]
    except (ValueError, KeyError, TypeError) as error:
        raise PlanError(f"fuzz/targets.json is malformed: {error}") from error
    names = set()
    for target in targets:
        name = target.get("name")
        if not isinstance(name, str) or not NAME.match(name) or name in names:
            raise PlanError(f"bad or repeated target name: {name!r}")
        names.add(name)
        crates = target.get("crates")
        if not crates or not all(
            isinstance(crate, str) and re.match(r"^kettle-[a-z-]+$", crate) for crate in crates
        ):
            raise PlanError(f"{name}: crates must name kettle crates")
        for key in ("nightly", "sunday"):
            value = target.get(key, target.get("nightly"))
            if not isinstance(value, int) or not 60 <= value <= DISPATCH_MAX_SECONDS:
                raise PlanError(f"{name}: {key} must be 60 to {DISPATCH_MAX_SECONDS} seconds")
    return targets


def bin_names(manifest: str) -> set[str]:
    """The `[[bin]]` names in a cargo-fuzz manifest."""
    names = set()
    for block in manifest.split("[[bin]]")[1:]:
        match = re.search(r'^name\s*=\s*"([^"]+)"', block, re.MULTILINE)
        if match:
            names.add(match.group(1))
    return names


def plan(
    targets: list[dict],
    event: str,
    weekday: int,
    requested: str = "",
    seconds: str = "",
) -> list[dict]:
    chosen = targets
    minimize = False
    if event == "pull_request":
        durations = {target["name"]: PULL_REQUEST_SECONDS for target in targets}
    elif event == "schedule":
        key = "sunday" if weekday == SUNDAY else "nightly"
        minimize = weekday == SUNDAY
        durations = {target["name"]: target.get(key, target["nightly"]) for target in targets}
    elif event == "workflow_dispatch":
        names = requested.split()
        known = {target["name"] for target in targets}
        unknown = sorted(set(names) - known)
        if unknown:
            raise PlanError(f"unknown targets: {' '.join(unknown)}")
        if names:
            chosen = [target for target in targets if target["name"] in names]
        asked = DISPATCH_DEFAULT_SECONDS
        if seconds.strip():
            if not re.fullmatch(r"[0-9]{1,6}", seconds.strip()) or int(seconds) == 0:
                raise PlanError(f"seconds must be a positive whole number, not {seconds!r}")
            asked = min(int(seconds), DISPATCH_MAX_SECONDS)
        durations = {target["name"]: asked for target in chosen}
    else:
        raise PlanError(f"no plan for event {event!r}")
    return [
        {
            "target": target["name"],
            "crates": " ".join(f"-p {crate}" for crate in target["crates"]),
            "seconds": durations[target["name"]],
            "timeout": -(-durations[target["name"]] // 60) + OVERHEAD_MINUTES,
            "minimize": minimize,
        }
        for target in chosen
    ]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--event", required=True)
    parser.add_argument("--weekday", type=int, required=True, help="0 is Monday")
    parser.add_argument("--targets", default="")
    parser.add_argument("--seconds", default="")
    arguments = parser.parse_args()
    try:
        targets = load_targets((ROOT / "fuzz/targets.json").read_text())
        listed = {target["name"] for target in targets}
        built = bin_names((ROOT / "fuzz/Cargo.toml").read_text())
        if listed != built:
            raise PlanError(
                "fuzz/targets.json and fuzz/Cargo.toml list different targets: "
                f"only in the table {sorted(listed - built)}, "
                f"only in the manifest {sorted(built - listed)}"
            )
        include = plan(
            targets, arguments.event, arguments.weekday, arguments.targets, arguments.seconds
        )
    except PlanError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    print("matrix=" + json.dumps({"include": include}, separators=(",", ":")))
    return 0


if __name__ == "__main__":
    sys.exit(main())
