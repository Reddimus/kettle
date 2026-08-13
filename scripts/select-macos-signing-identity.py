#!/usr/bin/env python3
"""Select one valid Developer ID Application identity from `security` output."""

from __future__ import annotations

import re
import sys


IDENTITY = re.compile(
    r'^\s*\d+\)\s+([0-9A-Fa-f]{40})\s+"Developer ID Application: .+"\s*$'
)


def main() -> int:
    matches = [
        match.group(1)
        for line in sys.stdin
        if (match := IDENTITY.match(line.rstrip("\n"))) is not None
    ]
    if len(matches) != 1:
        print(
            "expected exactly one valid Developer ID Application identity "
            f"in the imported keychain, found {len(matches)}",
            file=sys.stderr,
        )
        return 1
    print(matches[0])
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
