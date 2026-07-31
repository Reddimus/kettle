#!/usr/bin/env python3
"""Generate, verify, or safely extract Kettle release packages."""

from __future__ import annotations

import argparse
from dataclasses import dataclass
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import stat
import tarfile
import tempfile
from typing import BinaryIO
import zipfile


MANIFEST_NAME = "kettle-package-manifest.json"
SCHEMA = 1
MAX_FILES = 127
MAX_ARCHIVE_ENTRIES = 128
MAX_ARCHIVE_BYTES = 256 * 1024 * 1024
MAX_TOTAL_BYTES = 512 * 1024 * 1024
MAX_MANIFEST_BYTES = 256 * 1024
MAX_PACKAGE_PATH_BYTES = 240
MAX_ZIP_CENTRAL_BYTES = 1024 * 1024
COPY_CHUNK_BYTES = 1024 * 1024
TARGETS = {
    "x86_64-pc-windows-msvc",
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
}
SEMVER = re.compile(r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\Z")
SAFE_COMPONENT = re.compile(r"[A-Za-z0-9._-]+\Z")
SHA256 = re.compile(r"[0-9a-f]{64}\Z")


@dataclass
class ArchiveMember:
    source: object
    relative: str
    is_dir: bool
    size: int
    mode: int | None
    digest: str | None = None


class ArchivePaths:
    """Reject aliases and file/directory prefix conflicts portably."""

    def __init__(self) -> None:
        self.entries: dict[str, bool] = {}
        self.spellings: dict[str, str] = {}

    def insert(self, path: str, is_dir: bool) -> None:
        key = path.lower()
        original_parts = path.split("/") if path else []
        folded_parts = key.split("/") if key else []
        for index in range(1, len(folded_parts) + 1):
            folded_prefix = "/".join(folded_parts[:index])
            original_prefix = "/".join(original_parts[:index])
            previous = self.spellings.get(folded_prefix)
            if previous is not None and previous != original_prefix:
                raise ValueError(f"case-aliased archive path: {path!r}")
            self.spellings[folded_prefix] = original_prefix
        if key in self.entries:
            raise ValueError(f"duplicate or case-aliased archive path: {path!r}")
        for index in range(1, len(folded_parts)):
            ancestor = "/".join(folded_parts[:index])
            if self.entries.get(ancestor) is False:
                raise ValueError(f"file/directory prefix conflict at {path!r}")
        if not is_dir:
            prefix = f"{key}/"
            if any(existing.startswith(prefix) for existing in self.entries):
                raise ValueError(f"file/directory prefix conflict at {path!r}")
        self.entries[key] = is_dir


def is_windows_device_name(name: str) -> bool:
    stem = name.split(".", 1)[0].upper()
    if stem in {"CON", "PRN", "AUX", "NUL", "CLOCK$", "CONIN$", "CONOUT$"}:
        return True
    for prefix in ("COM", "LPT"):
        if stem.startswith(prefix) and stem[len(prefix) :] in tuple("123456789"):
            return True
    return False


def portable_components(path: str, *, directory: bool = False) -> list[str]:
    if not isinstance(path, str) or not path:
        raise ValueError(f"unsafe package path: {path!r}")
    if (
        not path.isascii()
        or len(path) > MAX_PACKAGE_PATH_BYTES
        or any(
            ord(character) < 32 or ord(character) == 127 for character in path
        )
    ):
        raise ValueError(f"non-ASCII or control character in package path: {path!r}")
    if "\\" in path or ":" in path or path.startswith("/"):
        raise ValueError(f"unsafe package path: {path!r}")
    if directory and path.endswith("/"):
        path = path[:-1]
    if not path or path.endswith("/") or "//" in path:
        raise ValueError(f"unsafe package path: {path!r}")
    parts = path.split("/")
    for component in parts:
        if (
            component in {"", ".", ".."}
            or len(component) > 255
            or SAFE_COMPONENT.fullmatch(component) is None
            or component.endswith((".", " "))
            or is_windows_device_name(component)
        ):
            raise ValueError(f"unsafe package path component: {component!r}")
    return parts


def archive_relative_path(path: str, *, directory: bool, target: str) -> str:
    parts = portable_components(path, directory=directory)
    if target.endswith("linux-gnu"):
        if parts[0] != "kettle":
            raise ValueError(
                f"Linux archive entry is outside the kettle root: {path!r}"
            )
        parts = parts[1:]
        if not parts:
            if not directory:
                raise ValueError("the Linux kettle archive root must be a directory")
            return ""
    return "/".join(parts)


def validate_linux_mode(mode: object, path: str) -> int:
    if type(mode) is not int or mode < 0 or mode & ~0o777:
        raise ValueError(f"special permission bits are forbidden for {path!r}")
    if mode & 0o022:
        raise ValueError(f"group/world-writable mode is forbidden for {path!r}")
    return mode


def validate_manifest_document(
    data: bytes, target: str, version: str
) -> tuple[dict[str, object], dict[str, dict[str, object]]]:
    validate_identity(target, version)
    if not data or len(data) > MAX_MANIFEST_BYTES:
        raise ValueError("package manifest size is outside the accepted range")

    def reject_duplicates(pairs: list[tuple[str, object]]) -> dict[str, object]:
        result: dict[str, object] = {}
        for key, value in pairs:
            if key in result:
                raise ValueError(f"duplicate JSON key in package manifest: {key!r}")
            result[key] = value
        return result

    try:
        manifest = json.loads(data, object_pairs_hook=reject_duplicates)
    except (UnicodeDecodeError, json.JSONDecodeError, RecursionError) as error:
        raise ValueError(f"package manifest is invalid JSON: {error}") from error
    if not isinstance(manifest, dict) or set(manifest) != {
        "schema",
        "product",
        "target",
        "version",
        "files",
    }:
        raise ValueError("package manifest has an invalid top-level schema")
    if (
        type(manifest["schema"]) is not int
        or manifest["schema"] != SCHEMA
        or manifest["product"] != "kettle"
        or manifest["target"] != target
        or manifest["version"] != version
        or not isinstance(manifest["files"], list)
        or not 1 <= len(manifest["files"]) <= MAX_FILES
    ):
        raise ValueError("package manifest identity failed validation")

    records: dict[str, dict[str, object]] = {}
    paths = ArchivePaths()
    total = 0
    ordered_paths: list[str] = []
    for value in manifest["files"]:
        if not isinstance(value, dict) or set(value) != {
            "path",
            "size",
            "sha256",
            "mode",
        }:
            raise ValueError("package manifest contains an invalid file record")
        path = value["path"]
        if not isinstance(path, str):
            raise ValueError("package manifest file path must be a string")
        normalized = "/".join(portable_components(path))
        if normalized != path or path.lower() == MANIFEST_NAME.lower():
            raise ValueError(f"unsafe or self-referential manifest path: {path!r}")
        paths.insert(path, False)
        size = value["size"]
        digest_value = value["sha256"]
        mode = value["mode"]
        if type(size) is not int or size < 0 or size > MAX_TOTAL_BYTES:
            raise ValueError(f"invalid manifest size for {path!r}")
        if not isinstance(digest_value, str) or SHA256.fullmatch(digest_value) is None:
            raise ValueError(f"invalid manifest SHA-256 for {path!r}")
        if target.endswith("linux-gnu"):
            validate_linux_mode(mode, path)
        elif mode is not None:
            raise ValueError(f"Windows manifest mode must be null for {path!r}")
        total += size
        if total > MAX_TOTAL_BYTES:
            raise ValueError("package contents exceed the 512 MiB safety limit")
        records[path] = value
        ordered_paths.append(path)
    if ordered_paths != sorted(ordered_paths):
        raise ValueError("package manifest file records are not sorted")
    if data != encode(manifest):
        raise ValueError("package manifest is not in deterministic canonical form")
    return manifest, records


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


def collect_files(root: Path, target: str | None = None) -> list[Path]:
    if not root.is_dir() or _is_link_or_junction(root):
        raise ValueError(f"package root must be a real directory: {root}")
    if target is not None and target.endswith("linux-gnu"):
        validate_linux_mode(stat.S_IMODE(root.lstat().st_mode), ".")
    files: list[Path] = []

    def raise_walk_error(error: OSError) -> None:
        raise error

    paths = ArchivePaths()
    for directory, dirs, names in os.walk(
        root, followlinks=False, onerror=raise_walk_error
    ):
        base = Path(directory)
        for name in dirs:
            path = base / name
            if _is_link_or_junction(path):
                raise ValueError(f"package contains a symlinked directory: {path}")
            relative = path.relative_to(root).as_posix()
            normalized = "/".join(portable_components(relative))
            if normalized != relative:
                raise ValueError(f"unsafe package directory path: {relative!r}")
            if target is not None and target.endswith("linux-gnu"):
                validate_linux_mode(
                    stat.S_IMODE(path.lstat().st_mode), relative
                )
            paths.insert(relative, True)
        for name in names:
            path = base / name
            relative = path.relative_to(root).as_posix()
            if relative == MANIFEST_NAME:
                continue
            metadata = path.lstat()
            if not stat.S_ISREG(metadata.st_mode):
                raise ValueError(f"package contains a non-regular file: {path}")
            normalized = "/".join(portable_components(relative))
            if normalized != relative or relative.lower() == MANIFEST_NAME.lower():
                raise ValueError(f"unsafe package file path: {relative!r}")
            paths.insert(relative, False)
            files.append(path)
    files.sort(key=lambda path: path.relative_to(root).as_posix())
    return files


def build_manifest(root: Path, target: str, version: str) -> dict[str, object]:
    validate_identity(target, version)
    files = collect_files(root, target)
    if not files or len(files) > MAX_FILES:
        raise ValueError(f"package must contain between 1 and {MAX_FILES} files")

    records: list[dict[str, object]] = []
    total = 0
    for path in files:
        relative = path.relative_to(root).as_posix()
        metadata = path.stat()
        total += metadata.st_size
        if total > MAX_TOTAL_BYTES:
            raise ValueError("package contents exceed the 512 MiB safety limit")
        mode = None
        if target.endswith("linux-gnu"):
            mode = validate_linux_mode(stat.S_IMODE(metadata.st_mode), relative)
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
    try:
        metadata = path.lstat()
    except FileNotFoundError as error:
        raise ValueError(f"package manifest is missing or unsafe: {path}") from error
    if path.is_symlink() or not stat.S_ISREG(metadata.st_mode):
        raise ValueError(f"package manifest is missing or unsafe: {path}")
    if metadata.st_size <= 0 or metadata.st_size > MAX_MANIFEST_BYTES:
        raise ValueError("package manifest size is outside the accepted range")
    data = path.read_bytes()
    actual, _ = validate_manifest_document(data, target, version)
    expected = build_manifest(root, target, version)
    if actual != expected:
        raise ValueError(
            "package manifest does not match the package contents or identity"
        )


def _archive_identity(metadata: os.stat_result) -> tuple[int, int, int, int]:
    return (
        metadata.st_dev,
        metadata.st_ino,
        metadata.st_size,
        metadata.st_mtime_ns,
    )


def _open_regular_archive(path: Path) -> tuple[BinaryIO, tuple[int, int, int, int]]:
    before = path.lstat()
    if not stat.S_ISREG(before.st_mode):
        raise ValueError(f"archive must be a regular file, not a link: {path}")
    if before.st_size <= 0 or before.st_size > MAX_ARCHIVE_BYTES:
        raise ValueError("archive size is outside the 256 MiB safety limit")

    flags = os.O_RDONLY
    for name in ("O_BINARY", "O_CLOEXEC", "O_NOINHERIT", "O_NOFOLLOW"):
        flags |= getattr(os, name, 0)
    descriptor = os.open(path, flags)
    try:
        after = os.fstat(descriptor)
        if (
            not stat.S_ISREG(after.st_mode)
            or _archive_identity(after) != _archive_identity(before)
        ):
            raise ValueError("archive changed while it was being opened")
        return os.fdopen(descriptor, "rb"), _archive_identity(after)
    except BaseException:
        os.close(descriptor)
        raise


def _digest_open_archive(stream: BinaryIO) -> str:
    stream.seek(0)
    sha256 = hashlib.sha256()
    total = 0
    for chunk in iter(lambda: stream.read(COPY_CHUNK_BYTES), b""):
        total += len(chunk)
        if total > MAX_ARCHIVE_BYTES:
            raise ValueError("archive grew beyond the 256 MiB safety limit")
        sha256.update(chunk)
    stream.seek(0)
    return sha256.hexdigest()


def _catalog_tar(
    archive: tarfile.TarFile, target: str
) -> list[ArchiveMember]:
    if archive.pax_headers:
        raise ValueError("PAX archive headers are forbidden")
    paths = ArchivePaths()
    members: list[ArchiveMember] = []
    total = 0
    saw_root = False
    for entry in archive:
        if len(members) >= MAX_ARCHIVE_ENTRIES:
            raise ValueError(
                f"archive contains more than {MAX_ARCHIVE_ENTRIES} entries"
            )
        if entry.pax_headers:
            raise ValueError(f"PAX archive headers are forbidden for {entry.name!r}")
        if entry.type == tarfile.GNUTYPE_SPARSE or getattr(entry, "sparse", None):
            raise ValueError(
                f"GNU sparse archive entries are forbidden: {entry.name!r}"
            )
        if entry.isdir():
            is_dir = True
        elif entry.isreg():
            is_dir = False
        else:
            raise ValueError(f"special tar archive entry is forbidden: {entry.name!r}")

        relative = archive_relative_path(
            entry.name, directory=is_dir, target=target
        )
        if not relative:
            if saw_root:
                raise ValueError("Linux archive contains duplicate kettle roots")
            saw_root = True
        else:
            paths.insert(relative, is_dir)

        if type(entry.size) is not int or entry.size < 0:
            raise ValueError(f"invalid archive size for {entry.name!r}")
        if is_dir and entry.size:
            raise ValueError(f"archive directory has nonzero size: {entry.name!r}")
        mode = validate_linux_mode(entry.mode, entry.name)
        total += entry.size
        if total > MAX_TOTAL_BYTES:
            raise ValueError("archive contents exceed the 512 MiB safety limit")
        members.append(
            ArchiveMember(
                source=entry,
                relative=relative,
                is_dir=is_dir,
                size=entry.size,
                mode=mode,
            )
        )

    if not members:
        raise ValueError(
            f"archive must contain between 1 and {MAX_ARCHIVE_ENTRIES} entries"
        )
    if not saw_root:
        raise ValueError("Linux archive is missing its single kettle/ root directory")
    return members


def _validate_zip_container(stream: BinaryIO, archive_size: int) -> None:
    tail_size = min(archive_size, 22 + 65_535)
    stream.seek(archive_size - tail_size)
    tail = stream.read(tail_size)
    signature = b"PK\x05\x06"
    position = tail.rfind(signature)
    while position >= 0:
        if position + 22 <= len(tail):
            comment_size = int.from_bytes(tail[position + 20 : position + 22], "little")
            if position + 22 + comment_size == len(tail):
                break
        position = tail.rfind(signature, 0, position)
    if position < 0:
        raise ValueError("ZIP end-of-central-directory record is missing")

    disk = int.from_bytes(tail[position + 4 : position + 6], "little")
    central_disk = int.from_bytes(tail[position + 6 : position + 8], "little")
    entries_on_disk = int.from_bytes(tail[position + 8 : position + 10], "little")
    entries = int.from_bytes(tail[position + 10 : position + 12], "little")
    central_size = int.from_bytes(tail[position + 12 : position + 16], "little")
    central_offset = int.from_bytes(tail[position + 16 : position + 20], "little")
    if (
        disk != 0
        or central_disk != 0
        or entries_on_disk != entries
        or entries in {0xFFFF, 0}
        or central_size == 0xFFFFFFFF
        or central_offset == 0xFFFFFFFF
    ):
        raise ValueError("multi-disk, empty, or ZIP64 archives are forbidden")
    if entries > MAX_ARCHIVE_ENTRIES:
        raise ValueError(
            f"archive contains more than {MAX_ARCHIVE_ENTRIES} entries"
        )
    if central_size > MAX_ZIP_CENTRAL_BYTES:
        raise ValueError("ZIP central directory exceeds the 1 MiB safety limit")
    eocd_offset = archive_size - tail_size + position
    if central_offset + central_size != eocd_offset:
        raise ValueError("ZIP central-directory bounds are inconsistent")
    stream.seek(0)


def _catalog_zip(archive: zipfile.ZipFile, target: str) -> list[ArchiveMember]:
    entries = archive.infolist()
    if not 1 <= len(entries) <= MAX_ARCHIVE_ENTRIES:
        raise ValueError(
            f"archive must contain between 1 and {MAX_ARCHIVE_ENTRIES} entries"
        )

    paths = ArchivePaths()
    members: list[ArchiveMember] = []
    total = 0
    for entry in entries:
        original = entry.orig_filename
        if entry.filename != original:
            raise ValueError(f"NUL-truncated ZIP path is forbidden: {original!r}")
        if entry.flag_bits & 0x1:
            raise ValueError(f"encrypted ZIP entry is forbidden: {original!r}")
        if entry.compress_type not in {zipfile.ZIP_STORED, zipfile.ZIP_DEFLATED}:
            raise ValueError(f"unsupported ZIP compression for {original!r}")
        if entry.compress_size < 0 or entry.compress_size > MAX_ARCHIVE_BYTES:
            raise ValueError(f"invalid compressed ZIP size for {original!r}")

        is_dir = original.endswith("/")
        unix_mode = (entry.external_attr >> 16) & 0xFFFF
        file_type = stat.S_IFMT(unix_mode)
        expected_type = stat.S_IFDIR if is_dir else stat.S_IFREG
        if file_type not in {0, expected_type}:
            raise ValueError(f"special ZIP archive entry is forbidden: {original!r}")
        if unix_mode:
            validate_linux_mode(unix_mode & 0o7777, original)

        relative = archive_relative_path(
            original, directory=is_dir, target=target
        )
        paths.insert(relative, is_dir)
        if type(entry.file_size) is not int or entry.file_size < 0:
            raise ValueError(f"invalid ZIP size for {original!r}")
        if is_dir and entry.file_size:
            raise ValueError(f"ZIP directory has nonzero size: {original!r}")
        total += entry.file_size
        if total > MAX_TOTAL_BYTES:
            raise ValueError("archive contents exceed the 512 MiB safety limit")
        members.append(
            ArchiveMember(
                source=entry,
                relative=relative,
                is_dir=is_dir,
                size=entry.file_size,
                mode=None,
            )
        )
    return members


def _open_archive_member(
    archive: tarfile.TarFile | zipfile.ZipFile, member: ArchiveMember
) -> BinaryIO:
    if isinstance(archive, tarfile.TarFile):
        assert isinstance(member.source, tarfile.TarInfo)
        stream = archive.extractfile(member.source)
        if stream is None:
            raise ValueError(f"tar member is not readable: {member.relative!r}")
        return stream
    assert isinstance(member.source, zipfile.ZipInfo)
    return archive.open(member.source, "r")


def _measure_member(
    archive: tarfile.TarFile | zipfile.ZipFile,
    member: ArchiveMember,
    *,
    capture: bool = False,
) -> tuple[int, str, bytes | None]:
    sha256 = hashlib.sha256()
    chunks: list[bytes] | None = [] if capture else None
    total = 0
    with _open_archive_member(archive, member) as stream:
        while True:
            chunk = stream.read(COPY_CHUNK_BYTES)
            if not chunk:
                break
            total += len(chunk)
            if total > member.size or total > MAX_TOTAL_BYTES:
                raise ValueError(
                    f"archive member exceeds its declared size: {member.relative!r}"
                )
            sha256.update(chunk)
            if chunks is not None:
                chunks.append(chunk)
    if total != member.size:
        raise ValueError(f"archive member size mismatch: {member.relative!r}")
    return total, sha256.hexdigest(), b"".join(chunks) if chunks is not None else None


def _required_directories(paths: set[str]) -> set[str]:
    directories: set[str] = set()
    for path in paths:
        parts = path.split("/")
        for index in range(1, len(parts)):
            directories.add("/".join(parts[:index]))
    return directories


def _preflight_archive(
    archive: tarfile.TarFile | zipfile.ZipFile,
    members: list[ArchiveMember],
    target: str,
    version: str,
) -> None:
    files = [member for member in members if not member.is_dir]
    manifests = [member for member in files if member.relative == MANIFEST_NAME]
    if len(manifests) != 1:
        raise ValueError("archive must contain exactly one canonical package manifest")
    manifest_member = manifests[0]
    if manifest_member.size <= 0 or manifest_member.size > MAX_MANIFEST_BYTES:
        raise ValueError("package manifest size is outside the accepted range")

    _, manifest_digest, manifest_data = _measure_member(
        archive, manifest_member, capture=True
    )
    assert manifest_data is not None
    manifest_member.digest = manifest_digest
    _, records = validate_manifest_document(manifest_data, target, version)

    payload = {
        member.relative: member
        for member in files
        if member.relative != MANIFEST_NAME
    }
    if set(payload) != set(records):
        raise ValueError("archive files do not exactly match the package manifest")

    file_paths = {member.relative for member in files}
    required_directories = _required_directories(file_paths)
    archive_directories = {
        member.relative
        for member in members
        if member.is_dir and member.relative
    }
    unexpected_directories = archive_directories - required_directories
    if unexpected_directories:
        unexpected = sorted(unexpected_directories)[0]
        raise ValueError(
            f"archive contains an undeclared empty directory: {unexpected!r}"
        )

    actual_total = manifest_member.size
    for path, member in payload.items():
        record = records[path]
        if member.size != record["size"]:
            raise ValueError(f"archive size does not match manifest for {path!r}")
        if target.endswith("linux-gnu") and member.mode != record["mode"]:
            raise ValueError(f"archive mode does not match manifest for {path!r}")
        actual, member_digest, _ = _measure_member(archive, member)
        actual_total += actual
        if actual_total > MAX_TOTAL_BYTES:
            raise ValueError("archive output exceeds the 512 MiB safety limit")
        if member_digest != record["sha256"]:
            raise ValueError(f"archive SHA-256 does not match manifest for {path!r}")
        member.digest = member_digest


def _is_link_or_junction(path: Path) -> bool:
    if path.is_symlink():
        return True
    is_junction = getattr(path, "is_junction", None)
    return bool(is_junction and is_junction())


def _require_real_directory(path: Path) -> None:
    metadata = path.lstat()
    if not stat.S_ISDIR(metadata.st_mode) or _is_link_or_junction(path):
        raise ValueError(f"output ancestor must be a real directory: {path}")


def _prepare_output_root(root: Path) -> tuple[Path, tuple[int, int]]:
    output = Path(os.path.abspath(os.fspath(root)))
    if os.path.lexists(output):
        raise ValueError(f"output root must not already exist: {output}")

    ancestors = list(output.parent.parents)
    ancestors.reverse()
    for ancestor in [*ancestors, output.parent]:
        _require_real_directory(ancestor)
    os.mkdir(output, 0o700)
    metadata = output.lstat()
    if not stat.S_ISDIR(metadata.st_mode) or _is_link_or_junction(output):
        raise ValueError(f"failed to create a real output directory: {output}")
    return output, (metadata.st_dev, metadata.st_ino)


def _same_created_root(root: Path, identity: tuple[int, int]) -> bool:
    try:
        metadata = root.lstat()
    except FileNotFoundError:
        return False
    return (
        stat.S_ISDIR(metadata.st_mode)
        and not _is_link_or_junction(root)
        and (metadata.st_dev, metadata.st_ino) == identity
    )


def _cleanup_created_root(root: Path, identity: tuple[int, int]) -> None:
    if not os.path.lexists(root):
        return
    if not _same_created_root(root, identity):
        raise RuntimeError("refusing to clean an output root whose identity changed")

    def make_removable(function, path, _error) -> None:
        os.chmod(path, 0o700)
        function(path)

    shutil.rmtree(root, onerror=make_removable)


def _destination(root: Path, relative: str) -> Path:
    return root.joinpath(*relative.split("/"))


def _create_output_directories(root: Path, members: list[ArchiveMember]) -> None:
    file_paths = {
        member.relative for member in members if not member.is_dir
    }
    for relative in sorted(
        _required_directories(file_paths), key=lambda path: (path.count("/"), path)
    ):
        destination = _destination(root, relative)
        os.mkdir(destination, 0o700)
        _require_real_directory(destination)


def _write_member(
    archive: tarfile.TarFile | zipfile.ZipFile,
    member: ArchiveMember,
    root: Path,
    target: str,
) -> None:
    if member.digest is None:
        raise ValueError(f"archive member was not preflighted: {member.relative!r}")
    destination = _destination(root, member.relative)
    _require_real_directory(destination.parent)
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
    for name in ("O_BINARY", "O_CLOEXEC", "O_NOINHERIT", "O_NOFOLLOW"):
        flags |= getattr(os, name, 0)
    descriptor = os.open(destination, flags, 0o600)
    mode_applied_by_descriptor = False
    try:
        metadata = os.fstat(descriptor)
        if not stat.S_ISREG(metadata.st_mode):
            raise ValueError(f"output is not a regular file: {destination}")
        sha256 = hashlib.sha256()
        total = 0
        with _open_archive_member(archive, member) as source:
            with os.fdopen(descriptor, "wb") as output:
                descriptor = -1
                while True:
                    chunk = source.read(COPY_CHUNK_BYTES)
                    if not chunk:
                        break
                    total += len(chunk)
                    if total > member.size or total > MAX_TOTAL_BYTES:
                        raise ValueError(
                            f"archive member changed size: {member.relative!r}"
                        )
                    sha256.update(chunk)
                    output.write(chunk)
                output.flush()
                os.fsync(output.fileno())
                if target.endswith("linux-gnu") and hasattr(os, "fchmod"):
                    assert member.mode is not None
                    os.fchmod(output.fileno(), member.mode)
                    mode_applied_by_descriptor = True
        if total != member.size or sha256.hexdigest() != member.digest:
            raise ValueError(
                f"archive member changed after preflight: {member.relative!r}"
            )
        if target.endswith("linux-gnu"):
            assert member.mode is not None
            if not mode_applied_by_descriptor:
                os.chmod(destination, member.mode)
            metadata = destination.lstat()
            if (
                not stat.S_ISREG(metadata.st_mode)
                or stat.S_IMODE(metadata.st_mode) != member.mode
            ):
                raise ValueError(
                    f"failed to apply archive mode for {member.relative!r}"
                )
    finally:
        if descriptor >= 0:
            os.close(descriptor)


def _write_archive(
    archive: tarfile.TarFile | zipfile.ZipFile,
    members: list[ArchiveMember],
    root: Path,
    target: str,
) -> None:
    _create_output_directories(root, members)
    for member in members:
        if not member.is_dir:
            _write_member(archive, member, root, target)


def _apply_directory_modes(
    root: Path,
    root_identity: tuple[int, int],
    members: list[ArchiveMember],
    target: str,
) -> None:
    if not target.endswith("linux-gnu"):
        return
    explicit_modes = {
        member.relative: member.mode
        for member in members
        if member.is_dir
    }
    directories = _required_directories(
        {member.relative for member in members if not member.is_dir}
    )
    for relative in sorted(
        directories, key=lambda path: (path.count("/"), path), reverse=True
    ):
        mode = explicit_modes.get(relative, 0o755)
        assert mode is not None
        destination = _destination(root, relative)
        _require_real_directory(destination)
        os.chmod(destination, mode)
        if stat.S_IMODE(destination.lstat().st_mode) != mode:
            raise ValueError(f"failed to apply directory mode for {relative!r}")
    root_mode = explicit_modes.get("", 0o755)
    assert root_mode is not None
    if not _same_created_root(root, root_identity):
        raise ValueError("output root identity changed before mode finalization")
    os.chmod(root, root_mode)
    if stat.S_IMODE(root.lstat().st_mode) != root_mode:
        raise ValueError("failed to apply the Linux package root mode")


def extract_archive(archive_path: Path, root: Path, target: str, version: str) -> None:
    validate_identity(target, version)
    created: tuple[Path, tuple[int, int]] | None = None
    try:
        stream, initial_identity = _open_regular_archive(archive_path)
        with stream:
            initial_digest = _digest_open_archive(stream)
            stream.seek(0)
            if target.endswith("linux-gnu"):
                with tarfile.open(fileobj=stream, mode="r:gz") as archive:
                    members = _catalog_tar(archive, target)
                    _preflight_archive(archive, members, target, version)
                    output, output_identity = _prepare_output_root(root)
                    created = (output, output_identity)
                    _write_archive(archive, members, output, target)
            else:
                _validate_zip_container(stream, initial_identity[2])
                with zipfile.ZipFile(stream, mode="r") as archive:
                    members = _catalog_zip(archive, target)
                    _preflight_archive(archive, members, target, version)
                    output, output_identity = _prepare_output_root(root)
                    created = (output, output_identity)
                    _write_archive(archive, members, output, target)

            if _archive_identity(os.fstat(stream.fileno())) != initial_identity:
                raise ValueError("archive metadata changed during extraction")
            if _digest_open_archive(stream) != initial_digest:
                raise ValueError("archive contents changed during extraction")
            if not _same_created_root(output, output_identity):
                raise ValueError("output root identity changed during extraction")
            verify(output, target, version)
            _apply_directory_modes(output, output_identity, members, target)
    except BaseException as error:
        if created is not None:
            try:
                _cleanup_created_root(*created)
            except BaseException as cleanup_error:
                raise RuntimeError(
                    f"extraction failed and partial output could not be cleaned: "
                    f"{cleanup_error}"
                ) from error
        if isinstance(error, (tarfile.TarError, zipfile.BadZipFile, EOFError)):
            raise ValueError(f"invalid release archive: {error}") from error
        raise


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("operation", choices=("generate", "verify", "extract"))
    parser.add_argument("--archive", type=Path)
    parser.add_argument("--root", required=True, type=Path)
    parser.add_argument("--target", required=True, choices=sorted(TARGETS))
    parser.add_argument("--version", required=True)
    args = parser.parse_args()
    try:
        if args.operation == "generate":
            if args.archive is not None:
                raise ValueError("--archive is only valid with extract")
            generate(args.root, args.target, args.version)
        elif args.operation == "verify":
            if args.archive is not None:
                raise ValueError("--archive is only valid with extract")
            verify(args.root, args.target, args.version)
        else:
            if args.archive is None:
                raise ValueError("--archive is required with extract")
            extract_archive(args.archive, args.root, args.target, args.version)
    except (OSError, RuntimeError, ValueError) as error:
        parser.error(str(error))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
