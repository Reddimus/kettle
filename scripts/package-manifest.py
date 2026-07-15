#!/usr/bin/env python3
"""Generate or verify Kettle's inner release-package manifest."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import tempfile


MANIFEST_NAME = "kettle-package-manifest.json"
SCHEMA = 1
MAX_FILES = 127
MAX_TOTAL_BYTES = 512 * 1024 * 1024
MAX_MANIFEST_BYTES = 256 * 1024
TARGETS = {
    "x86_64-pc-windows-msvc",
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
}
SEMVER = re.compile(r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\Z")


def digest(path: Path) -> str:
    sha256 = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            sha256.update(chunk)
    return sha256.hexdigest()


def validate_identity(target: str, version: str) -> None:
    if target not in TARGETS:
        raise ValueError(f"unsupported package target: {target!r}")
    if not SEMVER.fullmatch(version):
        raise ValueError(f"version must be stable MAJOR.MINOR.PATCH, got {version!r}")


def collect_files(root: Path) -> list[Path]:
    if not root.is_dir() or root.is_symlink():
        raise ValueError(f"package root must be a real directory: {root}")
    files: list[Path] = []
    for directory, dirs, names in os.walk(root, followlinks=False):
        base = Path(directory)
        for name in dirs:
            path = base / name
            if path.is_symlink():
                raise ValueError(f"package contains a symlinked directory: {path}")
        for name in names:
            path = base / name
            relative = path.relative_to(root).as_posix()
            if relative == MANIFEST_NAME:
                continue
            metadata = path.lstat()
            if not stat.S_ISREG(metadata.st_mode):
                raise ValueError(f"package contains a non-regular file: {path}")
            files.append(path)
    files.sort(key=lambda path: path.relative_to(root).as_posix())
    return files


def build_manifest(root: Path, target: str, version: str) -> dict[str, object]:
    validate_identity(target, version)
    files = collect_files(root)
    if not files or len(files) > MAX_FILES:
        raise ValueError(f"package must contain between 1 and {MAX_FILES} files")

    records: list[dict[str, object]] = []
    folded_paths: set[str] = set()
    total = 0
    for path in files:
        relative = path.relative_to(root).as_posix()
        if relative.startswith("/") or relative in {"", "."} or ".." in Path(relative).parts:
            raise ValueError(f"unsafe package path: {relative!r}")
        folded = relative.casefold()
        if folded in folded_paths or folded == MANIFEST_NAME.casefold():
            raise ValueError(f"case-insensitive duplicate package path: {relative}")
        folded_paths.add(folded)
        metadata = path.stat()
        total += metadata.st_size
        if total > MAX_TOTAL_BYTES:
            raise ValueError("package contents exceed the 512 MiB safety limit")
        mode = None
        if target.endswith("linux-gnu"):
            mode = stat.S_IMODE(metadata.st_mode)
        records.append(
            {
                "path": relative,
                "size": metadata.st_size,
                "sha256": digest(path),
                "mode": mode,
            }
        )

    return {
        "schema": SCHEMA,
        "product": "kettle",
        "target": target,
        "version": version,
        "files": records,
    }


def encode(manifest: dict[str, object]) -> bytes:
    data = (
        json.dumps(manifest, sort_keys=True, separators=(",", ":"), ensure_ascii=True)
        + "\n"
    ).encode("ascii")
    if len(data) > MAX_MANIFEST_BYTES:
        raise ValueError("package manifest exceeds the 256 KiB safety limit")
    return data


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


def generate(root: Path, target: str, version: str) -> None:
    output = root / MANIFEST_NAME
    write_atomic(output, encode(build_manifest(root, target, version)))


def verify(root: Path, target: str, version: str) -> None:
    validate_identity(target, version)
    path = root / MANIFEST_NAME
    if path.is_symlink() or not path.is_file():
        raise ValueError(f"package manifest is missing or unsafe: {path}")
    data = path.read_bytes()
    if not data or len(data) > MAX_MANIFEST_BYTES:
        raise ValueError("package manifest size is outside the accepted range")
    try:
        actual = json.loads(data)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ValueError(f"package manifest is invalid JSON: {error}") from error
    expected = build_manifest(root, target, version)
    if actual != expected:
        raise ValueError("package manifest does not match the package contents or identity")
    if data != encode(expected):
        raise ValueError("package manifest is not in deterministic canonical form")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("operation", choices=("generate", "verify"))
    parser.add_argument("--root", required=True, type=Path)
    parser.add_argument("--target", required=True, choices=sorted(TARGETS))
    parser.add_argument("--version", required=True)
    args = parser.parse_args()
    try:
        if args.operation == "generate":
            generate(args.root, args.target, args.version)
        else:
            verify(args.root, args.target, args.version)
    except (OSError, ValueError) as error:
        parser.error(str(error))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
