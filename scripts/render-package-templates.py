#!/usr/bin/env python3
"""Render release package metadata from the exact platform archives."""

from __future__ import annotations

import argparse
import hashlib
import os
import re
import tempfile
from pathlib import Path


VERSION_RE = re.compile(r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)")
UNRESOLVED_RE = re.compile(r"@[A-Z][A-Z0-9_]*@")


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as archive:
        for chunk in iter(lambda: archive.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def render(template: Path, replacements: dict[str, str]) -> str:
    text = template.read_text(encoding="utf-8")
    for token, value in replacements.items():
        count = text.count(token)
        if count != 1:
            raise ValueError(f"{template}: expected one {token}, found {count}")
        text = text.replace(token, value)

    unresolved = sorted(set(UNRESOLVED_RE.findall(text)))
    if unresolved:
        raise ValueError(f"{template}: unresolved tokens: {', '.join(unresolved)}")
    return text


def atomic_write(path: Path, content: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, temporary = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    try:
        with os.fdopen(fd, "w", encoding="utf-8", newline="\n") as output:
            output.write(content)
            output.flush()
            os.fsync(output.fileno())
        os.chmod(temporary, 0o644)
        os.replace(temporary, path)
    except BaseException:
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass
        raise


def parse_args() -> argparse.Namespace:
    repo_root = Path(__file__).resolve().parent.parent
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--version", required=True)
    parser.add_argument("--macos-archive", required=True, type=Path)
    parser.add_argument("--linux-x86-64-archive", required=True, type=Path)
    parser.add_argument("--output-dir", required=True, type=Path)
    parser.add_argument("--template-root", type=Path, default=repo_root / "packaging")
    return parser.parse_args()


def main() -> None:
    args = parse_args()
    if VERSION_RE.fullmatch(args.version) is None:
        raise SystemExit(f"invalid stable version: {args.version!r}")

    for archive in (args.macos_archive, args.linux_x86_64_archive):
        if not archive.is_file():
            raise SystemExit(f"archive not found: {archive}")

    macos_hash = sha256_file(args.macos_archive)
    linux_hash = sha256_file(args.linux_x86_64_archive)
    common = {"@VERSION@": args.version, "@LINUX_X86_64_SHA256@": linux_hash}

    try:
        formula = render(
            args.template_root / "homebrew" / "kettle.rb.in",
            {**common, "@MACOS_SHA256@": macos_hash},
        )
        pkgbuild = render(args.template_root / "arch" / "PKGBUILD.in", common)
    except (OSError, UnicodeError, ValueError) as error:
        raise SystemExit(str(error)) from error

    atomic_write(args.output_dir / "kettle.rb", formula)
    atomic_write(args.output_dir / "PKGBUILD", pkgbuild)
    print(f"rendered package metadata for {args.version}")


if __name__ == "__main__":
    main()
