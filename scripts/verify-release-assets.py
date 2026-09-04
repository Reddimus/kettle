#!/usr/bin/env python3
"""Verify a GitHub draft release against an exact local asset set."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import stat


MAX_ASSET_BYTES = 256 * 1024 * 1024
SHA256_DIGEST = re.compile(r"sha256:[0-9a-f]{64}\Z")


def identity(metadata: os.stat_result) -> tuple[int, int, int, int]:
    return (
        metadata.st_dev,
        metadata.st_ino,
        metadata.st_size,
        metadata.st_mtime_ns,
    )


def measure(path: Path) -> dict[str, object]:
    before = path.lstat()
    is_junction = getattr(path, "is_junction", None)
    if (
        not stat.S_ISREG(before.st_mode)
        or path.is_symlink()
        or bool(is_junction and is_junction())
        or before.st_size <= 0
        or before.st_size > MAX_ASSET_BYTES
    ):
        raise ValueError(f"local release asset is unsafe or unbounded: {path}")
    flags = os.O_RDONLY
    for name in ("O_BINARY", "O_CLOEXEC", "O_NOINHERIT", "O_NOFOLLOW"):
        flags |= getattr(os, name, 0)
    descriptor = os.open(path, flags)
    try:
        opened = os.fstat(descriptor)
        if (
            not stat.S_ISREG(opened.st_mode)
            or identity(opened) != identity(before)
        ):
            raise ValueError(f"local release asset changed while opening: {path}")
        with os.fdopen(descriptor, "rb") as stream:
            descriptor = -1
            sha256 = hashlib.sha256()
            total = 0
            for chunk in iter(lambda: stream.read(1024 * 1024), b""):
                total += len(chunk)
                if total > MAX_ASSET_BYTES:
                    raise ValueError(f"local release asset grew too large: {path}")
                sha256.update(chunk)
            if (
                total != opened.st_size
                or identity(os.fstat(stream.fileno())) != identity(opened)
            ):
                raise ValueError(
                    f"local release asset changed while hashing: {path}"
                )
        return {
            "size": total,
            "digest": f"sha256:{sha256.hexdigest()}",
        }
    finally:
        if descriptor >= 0:
            os.close(descriptor)


def local_assets(paths: list[Path]) -> dict[str, dict[str, object]]:
    expected: dict[str, dict[str, object]] = {}
    for path in paths:
        if path.name in expected:
            raise ValueError(f"duplicate local release asset name: {path.name}")
        expected[path.name] = measure(path)
    if not expected:
        raise ValueError("the expected release asset set is empty")
    return expected


def verify(
    payload: object,
    *,
    tag: str,
    paths: list[Path],
) -> None:
    if not isinstance(payload, dict):
        raise ValueError("GitHub release response is not an object")
    if payload.get("tag_name") != tag:
        raise ValueError("GitHub release tag does not match the requested tag")
    if payload.get("draft") is not True:
        raise ValueError("GitHub release must still be a draft during verification")
    if payload.get("prerelease") is not False:
        raise ValueError("GitHub release unexpectedly has prerelease state")

    expected = local_assets(paths)
    assets = payload.get("assets")
    if not isinstance(assets, list) or len(assets) != len(expected):
        raise ValueError("GitHub release asset count does not match the exact set")

    actual: dict[str, dict[str, object]] = {}
    for asset in assets:
        if not isinstance(asset, dict):
            raise ValueError("GitHub release contains a malformed asset")
        name = asset.get("name")
        size = asset.get("size")
        remote_digest = asset.get("digest")
        if not isinstance(name, str) or not name or name in actual:
            raise ValueError("GitHub release contains a duplicate or invalid asset name")
        if asset.get("state") != "uploaded":
            raise ValueError(f"GitHub release asset is not uploaded: {name}")
        if type(size) is not int or size <= 0 or size > MAX_ASSET_BYTES:
            raise ValueError(f"GitHub release asset has an invalid size: {name}")
        if (
            not isinstance(remote_digest, str)
            or SHA256_DIGEST.fullmatch(remote_digest) is None
        ):
            raise ValueError(f"GitHub release asset has an invalid digest: {name}")
        actual[name] = {
            "size": size,
            "digest": remote_digest,
        }

    if actual != expected:
        raise ValueError(
            f"GitHub release assets differ from local files:"
            f"\nexpected={expected}\nactual={actual}"
        )


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--api-json", required=True, type=Path)
    parser.add_argument("--tag", required=True)
    parser.add_argument("--asset", action="append", required=True, type=Path)
    args = parser.parse_args(argv)
    try:
        metadata = args.api_json.lstat()
        if (
            not stat.S_ISREG(metadata.st_mode)
            or args.api_json.is_symlink()
            or metadata.st_size <= 0
            or metadata.st_size > 1024 * 1024
        ):
            raise ValueError("GitHub release response is unsafe or unbounded")
        with args.api_json.open("r", encoding="utf-8") as stream:
            payload = json.load(stream)
        verify(payload, tag=args.tag, paths=args.asset)
    except (OSError, UnicodeError, json.JSONDecodeError, ValueError) as error:
        parser.error(str(error))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
