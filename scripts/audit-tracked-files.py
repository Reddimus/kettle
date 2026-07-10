#!/usr/bin/env python3
"""Audit every Git-tracked path and emit a reproducible integrity ledger."""

from __future__ import annotations

import argparse
import collections
import datetime as dt
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import struct
import subprocess
import sys
import tomllib
from typing import Any, Iterable
import unicodedata
import urllib.parse
import zlib


BINARY_EXTENSIONS = {
    ".gz",
    ".gif",
    ".icns",
    ".ico",
    ".jpeg",
    ".jpg",
    ".otf",
    ".png",
    ".ttf",
    ".woff",
    ".woff2",
    ".zip",
}
MARKDOWN_LINK = re.compile(r"(?<!!)\[[^\]]+\]\((<[^>]+>|[^)\s]+)")
MARKDOWN_FENCE = re.compile(r"(?ms)^\s*(```|~~~).*?^\s*\1\s*$")
MARKDOWN_CODE_SPAN = re.compile(r"`+[^`\n]*`+")


def git(root: Path, *args: str, text: bool = True) -> str | bytes:
    result = subprocess.run(
        ["git", *args],
        cwd=root,
        check=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=text,
    )
    return result.stdout


def tracked_entries(root: Path) -> list[tuple[str, str, str]]:
    raw = git(root, "ls-files", "--stage", "-z", text=False)
    assert isinstance(raw, bytes)
    entries: list[tuple[str, str, str]] = []
    for record in raw.split(b"\0"):
        if not record:
            continue
        metadata, separator, path_bytes = record.partition(b"\t")
        if not separator:
            raise ValueError("malformed git ls-files record")
        mode, object_id, stage = metadata.decode("ascii").split()
        if stage != "0":
            raise ValueError(
                f"unmerged index entry at {path_bytes.decode('utf-8', 'replace')}"
            )
        entries.append((mode, object_id, path_bytes.decode("utf-8")))
    return entries


def git_blob_id(data: bytes, object_format: str) -> str:
    try:
        digest = hashlib.new(object_format, usedforsecurity=False)
    except TypeError:
        digest = hashlib.new(object_format)
    digest.update(f"blob {len(data)}\0".encode("ascii"))
    digest.update(data)
    return digest.hexdigest()


def audit_sfnt(path: str, data: bytes) -> list[str]:
    errors: list[str] = []
    if len(data) < 12:
        return [f"{path}: truncated SFNT header"]
    signature = data[:4]
    if signature not in (b"\x00\x01\x00\x00", b"OTTO", b"true", b"typ1"):
        errors.append(f"{path}: unsupported SFNT signature {signature!r}")
    table_count = struct.unpack_from(">H", data, 4)[0]
    directory_end = 12 + 16 * table_count
    if directory_end > len(data):
        return [f"{path}: table directory exceeds file bounds"]
    tags: set[bytes] = set()
    for offset in range(12, directory_end, 16):
        tag, _checksum, table_offset, table_length = struct.unpack_from(
            ">4sIII", data, offset
        )
        if tag in tags:
            errors.append(f"{path}: duplicate SFNT table {tag!r}")
        tags.add(tag)
        if table_offset > len(data) or table_length > len(data) - table_offset:
            errors.append(f"{path}: SFNT table {tag!r} exceeds file bounds")
    required = {b"cmap", b"head", b"maxp", b"name"}
    missing = sorted(tag.decode("ascii") for tag in required - tags)
    if missing:
        errors.append(f"{path}: missing required SFNT tables: {', '.join(missing)}")
    return errors


def audit_png(path: str, data: bytes) -> list[str]:
    if not data.startswith(b"\x89PNG\r\n\x1a\n"):
        return [f"{path}: invalid PNG signature"]
    errors: list[str] = []
    offset = 8
    chunks: list[bytes] = []
    while offset + 12 <= len(data):
        length = struct.unpack_from(">I", data, offset)[0]
        chunk_type = data[offset + 4 : offset + 8]
        end = offset + 12 + length
        if end > len(data):
            errors.append(f"{path}: PNG chunk {chunk_type!r} exceeds file bounds")
            break
        payload = data[offset + 8 : offset + 8 + length]
        expected_crc = struct.unpack_from(">I", data, offset + 8 + length)[0]
        actual_crc = zlib.crc32(chunk_type + payload) & 0xFFFFFFFF
        if actual_crc != expected_crc:
            errors.append(f"{path}: PNG chunk {chunk_type!r} has an invalid CRC")
        chunks.append(chunk_type)
        offset = end
        if chunk_type == b"IEND":
            break
    if not chunks or chunks[0] != b"IHDR" or chunks[-1] != b"IEND":
        errors.append(f"{path}: PNG must start with IHDR and end with IEND")
    if offset != len(data):
        errors.append(f"{path}: trailing or truncated PNG data")
    return errors


def audit_binary(path: str, data: bytes) -> list[str]:
    suffix = PurePosixPath(path).suffix.lower()
    if suffix in {".ttf", ".otf"}:
        return audit_sfnt(path, data)
    if suffix == ".png":
        return audit_png(path, data)
    if suffix in {".jpg", ".jpeg"} and not (
        data.startswith(b"\xff\xd8") and data.endswith(b"\xff\xd9")
    ):
        return [f"{path}: invalid JPEG boundary markers"]
    if suffix == ".gif" and not (
        data.startswith((b"GIF87a", b"GIF89a")) and data.endswith(b";")
    ):
        return [f"{path}: invalid GIF boundary markers"]
    if suffix == ".ico" and not data.startswith(b"\x00\x00\x01\x00"):
        return [f"{path}: invalid ICO header"]
    return []


def audit_text(path: str, data: bytes) -> tuple[str | None, list[str]]:
    errors: list[str] = []
    try:
        text = data.decode("utf-8")
    except UnicodeDecodeError as error:
        return None, [f"{path}: invalid UTF-8 at byte {error.start}"]
    if text.startswith("\ufeff"):
        errors.append(f"{path}: unexpected UTF-8 BOM")
    if "\r" in text:
        errors.append(f"{path}: CR/CRLF found; tracked text must use LF")
    if text and not text.endswith("\n"):
        errors.append(f"{path}: missing final newline")
    if not path.lower().endswith(".md"):
        for number, line in enumerate(text.splitlines(), 1):
            if line.endswith((" ", "\t")):
                errors.append(f"{path}:{number}: trailing whitespace")
    return text, errors


def audit_structure(path: str, text: str) -> list[str]:
    errors: list[str] = []
    suffix = PurePosixPath(path).suffix.lower()
    try:
        if suffix == ".toml" or path == "Cargo.lock":
            tomllib.loads(text)
        elif suffix == ".json":
            json.loads(text)
    except (tomllib.TOMLDecodeError, json.JSONDecodeError) as error:
        errors.append(f"{path}: structured-data parse failed: {error}")
    return errors


def local_markdown_links(root: Path, path: str, text: str) -> Iterable[tuple[str, bool]]:
    parent = PurePosixPath(path).parent
    prose = MARKDOWN_FENCE.sub("", text)
    prose = MARKDOWN_CODE_SPAN.sub("", prose)
    for match in MARKDOWN_LINK.finditer(prose):
        raw = match.group(1).strip("<>")
        if raw.startswith(("#", "http://", "https://", "mailto:")):
            continue
        target = urllib.parse.unquote(raw.split("#", 1)[0])
        if not target:
            continue
        relative = PurePosixPath(target.lstrip("/")) if target.startswith("/") else parent / target
        normalized = Path(os.path.normpath(str(relative).replace("/", os.sep)))
        yield raw, (root / normalized).exists()


def audit(root: Path) -> dict[str, Any]:
    entries = tracked_entries(root)
    object_format = str(git(root, "rev-parse", "--show-object-format")).strip()
    head = str(git(root, "rev-parse", "HEAD")).strip()
    errors: list[str] = []
    warnings: list[str] = []
    files: list[dict[str, Any]] = []
    categories: collections.Counter[str] = collections.Counter()
    extensions: collections.Counter[str] = collections.Counter()
    casefolded: dict[str, list[str]] = collections.defaultdict(list)

    for mode, index_id, relative in entries:
        pure = PurePosixPath(relative)
        categories[pure.parts[0]] += 1
        extensions[pure.suffix.lower() or "<none>"] += 1
        casefolded[unicodedata.normalize("NFC", relative).casefold()].append(relative)
        if (
            pure.is_absolute()
            or ".." in pure.parts
            or "\\" in relative
            or pure.as_posix() != relative
        ):
            errors.append(f"{relative}: unsafe or non-canonical tracked path")
            continue
        path = root.joinpath(*pure.parts)
        if not path.exists() and not path.is_symlink():
            errors.append(f"{relative}: tracked path is missing from the worktree")
            continue
        is_symlink = mode == "120000"
        if is_symlink:
            if not path.is_symlink():
                errors.append(f"{relative}: index says symlink but worktree does not")
                continue
            target = os.readlink(path)
            data = os.fsencode(target)
            resolved = (path.parent / target).resolve()
            try:
                resolved.relative_to(root)
            except ValueError:
                errors.append(f"{relative}: symlink escapes the repository: {target}")
        elif not path.is_file():
            errors.append(f"{relative}: tracked path is not a regular file")
            continue
        else:
            data = path.read_bytes()

        suffix = pure.suffix.lower()
        is_binary = suffix in BINARY_EXTENSIONS or b"\0" in data
        text: str | None = None
        if is_symlink:
            pass
        elif is_binary:
            errors.extend(audit_binary(relative, data))
        else:
            text, text_errors = audit_text(relative, data)
            errors.extend(text_errors)
            if text is not None:
                errors.extend(audit_structure(relative, text))
                if suffix in {".yml", ".yaml"} and "\t" in text:
                    errors.append(f"{relative}: tab found in YAML")
                if suffix == ".md":
                    for link, exists in local_markdown_links(root, relative, text):
                        if not exists:
                            warnings.append(f"{relative}: unresolved local link {link}")

        worktree_id = git_blob_id(data, object_format)
        files.append(
            {
                "path": relative,
                "mode": mode,
                "bytes": len(data),
                "kind": "symlink" if is_symlink else "binary" if is_binary else "text",
                "index_object": index_id,
                "worktree_object": worktree_id,
                "matches_index": worktree_id == index_id,
                "sha256": hashlib.sha256(data).hexdigest(),
            }
        )

    for paths in casefolded.values():
        if len(paths) > 1:
            errors.append(f"case-folding path collision: {', '.join(sorted(paths))}")

    dirty = [item["path"] for item in files if not item["matches_index"]]
    return {
        "schema": 1,
        "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "repository": str(root),
        "head": head,
        "object_format": object_format,
        "summary": {
            "tracked_files": len(entries),
            "audited_files": len(files),
            "tracked_bytes": sum(item["bytes"] for item in files),
            "dirty_files": len(dirty),
            "errors": len(errors),
            "warnings": len(warnings),
        },
        "categories": dict(sorted(categories.items())),
        "extensions": dict(sorted(extensions.items())),
        "dirty_paths": dirty,
        "errors": errors,
        "warnings": warnings,
        "files": files,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument("--output", type=Path)
    parser.add_argument(
        "--require-clean-index",
        action="store_true",
        help="fail when a worktree file differs from its indexed Git object",
    )
    args = parser.parse_args()
    root = args.root.resolve()
    report = audit(root)
    if args.output:
        output = args.output if args.output.is_absolute() else root / args.output
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    summary = report["summary"]
    print(
        "tracked-file audit: "
        f"{summary['audited_files']}/{summary['tracked_files']} files, "
        f"{summary['tracked_bytes']} bytes, {summary['dirty_files']} dirty, "
        f"{summary['errors']} errors, {summary['warnings']} warnings"
    )
    for error in report["errors"]:
        print(f"error: {error}", file=sys.stderr)
    for warning in report["warnings"]:
        print(f"warning: {warning}", file=sys.stderr)
    if report["errors"]:
        return 1
    if args.require_clean_index and report["dirty_paths"]:
        print("error: worktree differs from the Git index", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
