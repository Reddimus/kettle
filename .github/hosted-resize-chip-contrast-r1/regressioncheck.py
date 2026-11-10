"""Qualify exact Rust assertion failures and focused GREEN from retained logs."""

import argparse
import hashlib
import json
from pathlib import Path
import re


ASSERTION_MESSAGES = {
    "chrome::tests::resize_chip_light_theme_text_and_outline_are_readable": "assertion failed: color::contrast_ratio(ink, fill) >= TEXT_CONTRAST",
    "chrome::tests::resize_chip_is_readable_across_themes_and_color_overrides": "assertion `left == right` failed: 0x96f: opaque resize fill",
}


def qualify(text, status, expect, names, exact_one=False):
    if expect == "red":
        if status != 101 or len(names) != 1:
            return False
        name = re.escape(names[0])
        message = ASSERTION_MESSAGES.get(names[0])
        if message is None:
            return False
        return (
            len(re.findall(r"^running 1 test$", text, re.M)) == 1
            and len(re.findall(rf"^test {name} \.\.\. FAILED$", text, re.M)) == 1
            and len(
                re.findall(
                    r"^test result: FAILED\. 0 passed; 1 failed; 0 ignored;", text, re.M
                )
            )
            == 1
            and re.search(
                rf"thread '{name}'(?: \([0-9]+\))? panicked at [^\n]+:\d+:\d+:\n{re.escape(message)}\n",
                text,
            )
            is not None
            and all(
                line.startswith("error: test failed,")
                for line in text.splitlines()
                if re.match(r"^error(?:\[[^]]+\])?:", line)
            )
        )
    counts = [
        int(n)
        for n in re.findall(r"^test result: ok\. (\d+) passed; 0 failed;", text, re.M)
    ]
    return (
        status == 0
        and any(n > 0 for n in counts)
        and (not exact_one or counts == [1])
        and all(
            re.search(rf"^test {re.escape(name)} \.\.\. ok$", text, re.M)
            for name in names
        )
        and not re.search(r"^error(?:\[[^]]+\])?:|^test result: FAILED", text, re.M)
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", required=True, type=Path)
    parser.add_argument("--exit", required=True, type=int)
    parser.add_argument("--drain-exit", required=True, type=int)
    parser.add_argument("--expect", required=True, choices=("red", "green"))
    parser.add_argument("--name", action="append", required=True)
    parser.add_argument("--exact-one", action="store_true")
    args = parser.parse_args()
    raw = (args.root / "strict.log").read_bytes()
    capture = json.loads((args.root / "strict-log.json").read_text(encoding="utf-8"))
    complete = (
        args.drain_exit == 0
        and capture["pipe_eof"] is True
        and capture["over_limit"] is False
        and capture["bytes"] == capture["retained_bytes"] == len(raw)
        and hashlib.sha256(raw).hexdigest() == capture["sha256_all_drained_bytes"]
    )
    passed = complete and qualify(
        raw.decode("utf-8", "replace"),
        args.exit,
        args.expect,
        args.name,
        args.exact_one,
    )
    record = {
        "scope": "HOSTED_RUST_TEST_LOG_ONLY_NOT_NATIVE_RELEASE",
        "expected": args.expect,
        "names": args.name,
        "command_exit": args.exit,
        "drain_exit": args.drain_exit,
        "full_log_drained": complete,
        "qualified": passed,
        "raw_log_sha256": hashlib.sha256(raw).hexdigest(),
    }
    with (args.root / "regression-qualification.json").open(
        "x", encoding="utf-8"
    ) as output:
        output.write(json.dumps(record, indent=2) + "\n")
    print(json.dumps(record, sort_keys=True))
    return 0 if passed else 1


if __name__ == "__main__":
    raise SystemExit(main())
