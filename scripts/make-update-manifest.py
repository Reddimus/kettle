#!/usr/bin/env python3
"""Build kettle's deterministic stable-channel update manifest."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import tempfile


SCHEMA = 1
MAX_ARTIFACT_BYTES = 256 * 1024 * 1024
MAX_MANIFEST_BYTES = 128 * 1024
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


def _identity(metadata: os.stat_result) -> tuple[int, int, int, int]:
    return (
        metadata.st_dev,
        metadata.st_ino,
        metadata.st_size,
        metadata.st_mtime_ns,
    )


def _open_regular(path: Path, limit: int):
    before = path.lstat()
    is_junction = getattr(path, "is_junction", None)
    if (
        not stat.S_ISREG(before.st_mode)
        or path.is_symlink()
        or bool(is_junction and is_junction())
        or before.st_size <= 0
        or before.st_size > limit
    ):
        raise ValueError(f"file is not a bounded regular file: {path}")
    flags = os.O_RDONLY
    for name in ("O_BINARY", "O_CLOEXEC", "O_NOINHERIT", "O_NOFOLLOW"):
        flags |= getattr(os, name, 0)
    descriptor = os.open(path, flags)
    try:
        after = os.fstat(descriptor)
        if (
            not stat.S_ISREG(after.st_mode)
            or _identity(after) != _identity(before)
        ):
            raise ValueError(f"file changed while it was being opened: {path}")
        return os.fdopen(descriptor, "rb"), _identity(after)
    except BaseException:
        os.close(descriptor)
        raise


def digest_and_size(path: Path, limit: int) -> tuple[int, str]:
    stream, identity = _open_regular(path, limit)
    with stream:
        sha256 = hashlib.sha256()
        total = 0
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            total += len(chunk)
            if total > limit:
                raise ValueError(f"file grew beyond its safety limit: {path}")
            sha256.update(chunk)
        if total != identity[2] or _identity(os.fstat(stream.fileno())) != identity:
            raise ValueError(f"file changed while it was being hashed: {path}")
    return total, sha256.hexdigest()


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
        try:
            size, sha256 = digest_and_size(path, MAX_ARTIFACT_BYTES)
        except ValueError as error:
            raise ValueError(
                f"artifact size is outside the accepted range: {path}"
            ) from error
        output_assets.append(
            {
                "target": target,
                "name": expected_name,
                "size": size,
                "sha256": sha256,
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


def encode_manifest(manifest: dict[str, object]) -> bytes:
    return (
        json.dumps(manifest, sort_keys=True, separators=(",", ":"), ensure_ascii=True)
        + "\n"
    ).encode("ascii")


def verify_manifest(
    path: Path,
    tag: str,
    assets: list[tuple[str, Path]],
) -> None:
    stream, identity = _open_regular(path, MAX_MANIFEST_BYTES)
    with stream:
        data = stream.read(MAX_MANIFEST_BYTES + 1)
        if (
            len(data) != identity[2]
            or _identity(os.fstat(stream.fileno())) != identity
        ):
            raise ValueError("signed manifest changed while it was being read")
    try:
        text = data.decode("ascii")

        def reject_duplicates(
            pairs: list[tuple[str, object]],
        ) -> dict[str, object]:
            result: dict[str, object] = {}
            for key, value in pairs:
                if key in result:
                    raise ValueError(f"duplicate signed manifest key: {key!r}")
                result[key] = value
            return result

        manifest = json.loads(text, object_pairs_hook=reject_duplicates)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ValueError(f"signed manifest is invalid JSON: {error}") from error
    if not isinstance(manifest, dict) or set(manifest) != {
        "schema",
        "product",
        "channel",
        "version",
        "tag",
        "published_at",
        "assets",
    }:
        raise ValueError("signed manifest has an invalid top-level schema")
    published_at = manifest["published_at"]
    if not isinstance(published_at, str):
        raise ValueError("signed manifest published_at must be a string")
    expected = build_manifest(tag, published_at, assets)
    if manifest != expected or data != encode_manifest(expected):
        raise ValueError(
            "signed manifest does not exactly bind the local release artifacts"
        )


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
    parser.add_argument("--published-at")
    parser.add_argument("--asset", action="append", type=parse_asset, required=True)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--verify-existing", type=Path)
    args = parser.parse_args()
    try:
        if args.verify_existing is not None:
            if args.published_at is not None or args.output is not None:
                raise ValueError(
                    "--verify-existing cannot be combined with "
                    "--published-at or --output"
                )
            verify_manifest(args.verify_existing, args.tag, args.asset)
            return 0
        if args.published_at is None or args.output is None:
            raise ValueError(
                "generation requires both --published-at and --output"
            )
        manifest = build_manifest(args.tag, args.published_at, args.asset)
    except (OSError, UnicodeError, ValueError) as error:
        parser.error(str(error))
    write_atomic(args.output, encode_manifest(manifest))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
