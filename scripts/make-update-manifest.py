#!/usr/bin/env python3
"""Build kettle's deterministic stable-channel update manifest."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import tempfile


SCHEMA = 1
EXPECTED_NAMES = {
    "x86_64-pc-windows-msvc": "kettle-windows-x86_64.zip",
    "x86_64-unknown-linux-gnu": "kettle-linux-x86_64.tar.gz",
    "aarch64-unknown-linux-gnu": "kettle-linux-aarch64.tar.gz",
}


def parse_asset(value: str) -> tuple[str, Path]:
    target, separator, raw_path = value.partition("=")
    if not separator or target not in EXPECTED_NAMES or not raw_path:
        raise argparse.ArgumentTypeError(
            "asset must be one of TARGET=PATH for " + ", ".join(EXPECTED_NAMES)
        )
    return target, Path(raw_path)


def digest(path: Path) -> str:
    sha256 = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            sha256.update(chunk)
    return sha256.hexdigest()


def build_manifest(
    tag: str, published_at: str, assets: list[tuple[str, Path]]
) -> dict[str, object]:
    match = re.fullmatch(r"v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)", tag)
    if not match:
        raise ValueError(f"stable tag must be vMAJOR.MINOR.PATCH, got {tag!r}")
    if not published_at or any(char in published_at for char in "\r\n"):
        raise ValueError("published-at must be a non-empty single-line timestamp")
    by_target = dict(assets)
    if len(by_target) != len(assets) or set(by_target) != set(EXPECTED_NAMES):
        raise ValueError("exactly one artifact for every supported target is required")

    output_assets = []
    for target in sorted(by_target):
        path = by_target[target]
        expected_name = EXPECTED_NAMES[target]
        if path.name != expected_name:
            raise ValueError(
                f"{target} requires {expected_name}, got {path.name or str(path)!r}"
            )
        if not path.is_file():
            raise ValueError(f"artifact does not exist: {path}")
        size = path.stat().st_size
        if size <= 0 or size > 512 * 1024 * 1024:
            raise ValueError(f"artifact size is outside the accepted range: {path}")
        output_assets.append(
            {
                "target": target,
                "name": expected_name,
                "size": size,
                "sha256": digest(path),
            }
        )

    return {
        "schema": SCHEMA,
        "product": "kettle",
        "channel": "stable",
        "version": tag[1:],
        "tag": tag,
        "published_at": published_at,
        "assets": output_assets,
    }


def write_atomic(path: Path, data: bytes) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    descriptor, temporary = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    try:
        with os.fdopen(descriptor, "wb") as stream:
            stream.write(data)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
    except BaseException:
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass
        raise


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tag", required=True)
    parser.add_argument("--published-at", required=True)
    parser.add_argument("--asset", action="append", type=parse_asset, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    try:
        manifest = build_manifest(args.tag, args.published_at, args.asset)
    except ValueError as error:
        parser.error(str(error))
    encoded = (
        json.dumps(manifest, sort_keys=True, separators=(",", ":"), ensure_ascii=True)
        + "\n"
    ).encode("ascii")
    write_atomic(args.output, encoded)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
