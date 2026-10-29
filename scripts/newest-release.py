#!/usr/bin/env python3
"""Print the id of the newest published stable release.

Usage: newest-release.py [--require TAG] [--above CURRENT] RELEASES_JSON

RELEASES_JSON is `gh api --paginate --slurp .../releases` output: a list of
pages, each a list of releases. Drafts, prereleases and tags that are not
plain `vMAJOR.MINOR.PATCH` are ignored. Exits non-zero, printing nothing, when
the input cannot be read or no release qualifies, so a caller that moves
"latest" stops rather than guessing. With --require, exits 3 while TAG is not
yet listed as a published stable release, which the list can briefly lag
behind after publication; the caller retries. With --above, prints nothing
(exit 0) unless the newest release is strictly newer than CURRENT, so latest
is never moved backward; an empty CURRENT means there is no latest yet.
"""

import json
import re
import sys

TAG = re.compile(r"^v(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$")


def published(pages):
    for page in pages:
        for release in page:
            if not release.get("draft") and not release.get("prerelease"):
                yield release


def newest(pages):
    best = None
    for page in pages:
        for release in page:
            if release.get("draft") or release.get("prerelease"):
                continue
            match = TAG.match(str(release.get("tag_name", "")))
            if not match:
                continue
            key = tuple(int(part) for part in match.groups())
            if best is None or key > best[0]:
                best = (key, release["id"])
    return None if best is None else best[1]


def version(tag):
    match = TAG.match(tag)
    return None if match is None else tuple(int(part) for part in match.groups())


def newest_release(pages):
    best = None
    for release in published(pages):
        key = version(str(release.get("tag_name", "")))
        if key is not None and (best is None or key > best[0]):
            best = (key, release["id"])
    return best


def main():
    args = sys.argv[1:]
    options = {}
    while len(args) > 1 and args[0] in ("--require", "--above"):
        options[args[0]] = args[1]
        args = args[2:]
    if len(args) != 1:
        print(__doc__.strip(), file=sys.stderr)
        return 2
    with open(args[0], encoding="utf-8") as handle:
        pages = json.load(handle)
    if not isinstance(pages, list) or not all(isinstance(page, list) for page in pages):
        print("newest-release: expected a list of release pages", file=sys.stderr)
        return 1
    required = options.get("--require")
    if required is not None and not any(
        release.get("tag_name") == required for release in published(pages)
    ):
        print(f"newest-release: {required} is not listed as published yet", file=sys.stderr)
        return 3
    found = newest_release(pages)
    if found is None:
        print("newest-release: no published stable release", file=sys.stderr)
        return 1
    current = options.get("--above")
    if current:
        floor = version(current)
        if floor is None:
            print(f"newest-release: cannot order the current latest {current!r}", file=sys.stderr)
            return 1
        if found[0] <= floor:
            return 0
    print(found[1])
    return 0

if __name__ == "__main__":
    sys.exit(main())
