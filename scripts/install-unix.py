#!/usr/bin/env python3
"""Descriptor-relative filesystem operations for Kettle's Linux installer."""

from __future__ import annotations

import argparse
from contextlib import contextmanager
import errno
import hashlib
import json
import os
import secrets
import stat
import sys
from typing import Any, Dict, Iterator, List, NoReturn, Optional, Sequence, Tuple


MANIFEST_RELATIVE = "share/kettle/install-files.json"
# What an install is about to write, kept until its provenance is committed.
# A retry trusts a file that differs from its record only when this receipt
# shows the interrupted install wrote it; equal bytes alone prove nothing.
PENDING_RELATIVE = "share/kettle/install-pending.json"
UNINSTALL_RELATIVE = "share/kettle/uninstall-pending.json"
RECOVERY_PATHS = ("share/kettle/install.sh", "share/kettle/install-unix.py")
MANIFEST_SCHEMA = 1
MAX_ENTRIES = 128
# Every provenance record names these. A partial first install has no record
# until it has them all; its retry recognizes the files it already wrote.
REQUIRED_PATHS = frozenset(
    {
        "bin/kettle",
        "share/applications/kettle.desktop",
        "share/kettle/install.sh",
        "share/kettle/install-unix.py",
        "share/kettle/install.json",
    }
)
MAX_FILE_BYTES = 512 * 1024 * 1024
MAX_MANIFEST_BYTES = 1024 * 1024
UPDATE_LOCK = ".kettle-update.lock"
DIRECTORY_FLAGS = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC
FILE_READ_FLAGS = os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC | os.O_NONBLOCK


class InstallerError(RuntimeError):
    pass


def _fail(message: str) -> NoReturn:
    raise InstallerError(message)


def _is_int(value: Any) -> bool:
    return isinstance(value, int) and not isinstance(value, bool)


def _absolute_parts(path: str, label: str) -> List[str]:
    if not path.startswith("/") or path == "/":
        _fail(f"{label} must be an absolute non-root path")
    if any(ord(character) < 32 or ord(character) == 127 for character in path):
        _fail(f"{label} contains a control character")
    parts = path.split("/")[1:]
    if not parts or any(part in ("", ".", "..") for part in parts):
        _fail(f"{label} is not a canonical absolute path")
    return parts


def _relative_parts(path: str) -> List[str]:
    if (
        not path
        or path.startswith("/")
        or "\\" in path
        or any(ord(character) < 32 or ord(character) == 127 for character in path)
    ):
        _fail(f"unsafe managed relative path: {path!r}")
    parts = path.split("/")
    if any(part in ("", ".", "..") or len(part.encode("utf-8")) > 255 for part in parts):
        _fail(f"unsafe managed relative path: {path!r}")
    return parts


def _validate_directory(metadata: os.stat_result, path: str, *, final: bool) -> None:
    if not stat.S_ISDIR(metadata.st_mode):
        _fail(f"install path component is not a directory: {path}")
    effective_uid = os.geteuid()
    if metadata.st_uid not in ({0} if effective_uid == 0 else {0, effective_uid}):
        _fail(f"install path component has an untrusted owner: {path}")
    writable_by_others = stat.S_IMODE(metadata.st_mode) & 0o022
    trusted_sticky_ancestor = (
        not final
        and metadata.st_uid == 0
        and bool(metadata.st_mode & stat.S_ISVTX)
    )
    if writable_by_others and not trusted_sticky_ancestor:
        _fail(f"install path component is group/other writable: {path}")


def _open_absolute_directory(
    path: str,
    *,
    create: bool,
    final_mode: int = 0o755,
) -> int:
    parts = _absolute_parts(path, "directory")
    current_fd = os.open("/", DIRECTORY_FLAGS)
    current_text = ""
    try:
        for index, component in enumerate(parts):
            current_text += "/" + component
            final = index == len(parts) - 1
            try:
                next_fd = os.open(component, DIRECTORY_FLAGS, dir_fd=current_fd)
            except FileNotFoundError:
                if not create:
                    raise
                mode = final_mode if final else 0o755
                os.mkdir(component, mode, dir_fd=current_fd)
                next_fd = os.open(component, DIRECTORY_FLAGS, dir_fd=current_fd)
                os.fchmod(next_fd, mode)
                os.fsync(current_fd)
            except OSError as error:
                if error.errno in (errno.ELOOP, errno.ENOTDIR):
                    _fail(f"install path component is a symlink or non-directory: {current_text}")
                raise
            metadata = os.fstat(next_fd)
            _validate_directory(metadata, current_text, final=final)
            os.close(current_fd)
            current_fd = next_fd
        return current_fd
    except Exception:
        os.close(current_fd)
        raise


def _open_relative_directory(
    root_fd: int,
    parts: Sequence[str],
    *,
    create: bool,
    created: Optional[Dict[str, int]] = None,
) -> int:
    current_fd = os.dup(root_fd)
    walked: List[str] = []
    try:
        for component in parts:
            walked.append(component)
            relative = "/".join(walked)
            try:
                next_fd = os.open(component, DIRECTORY_FLAGS, dir_fd=current_fd)
            except FileNotFoundError:
                if not create:
                    raise
                os.mkdir(component, 0o755, dir_fd=current_fd)
                next_fd = os.open(component, DIRECTORY_FLAGS, dir_fd=current_fd)
                os.fchmod(next_fd, 0o755)
                os.fsync(current_fd)
                if created is not None:
                    created[relative] = 0o755
            except OSError as error:
                if error.errno in (errno.ELOOP, errno.ENOTDIR):
                    _fail(f"managed path component is a symlink or non-directory: {relative}")
                raise
            metadata = os.fstat(next_fd)
            if metadata.st_uid != os.geteuid():
                _fail(f"managed directory has an unexpected owner: {relative}")
            if not stat.S_ISDIR(metadata.st_mode) or stat.S_IMODE(metadata.st_mode) & 0o022:
                _fail(f"managed directory has an unsafe type or mode: {relative}")
            os.close(current_fd)
            current_fd = next_fd
        return current_fd
    except Exception:
        os.close(current_fd)
        raise


def _open_parent(
    root_fd: int,
    relative: str,
    *,
    create: bool = False,
    created: Optional[Dict[str, int]] = None,
) -> Tuple[int, str]:
    parts = _relative_parts(relative)
    parent = _open_relative_directory(
        root_fd,
        parts[:-1],
        create=create,
        created=created,
    )
    return parent, parts[-1]


def _read_fd_bounded(handle: int, maximum: int) -> bytes:
    chunks: List[bytes] = []
    total = 0
    while True:
        chunk = os.read(handle, min(1024 * 1024, maximum + 1 - total))
        if not chunk:
            return b"".join(chunks)
        chunks.append(chunk)
        total += len(chunk)
        if total > maximum:
            _fail("file exceeds its installer safety limit")


def _open_source(path: str) -> int:
    parts = _absolute_parts(path, "installer source")
    parent_path = "/" + "/".join(parts[:-1]) if len(parts) > 1 else "/"
    if parent_path == "/":
        parent_fd = os.open("/", DIRECTORY_FLAGS)
    else:
        # Source directories are not required to be private, but every component
        # is still opened without following a symlink.
        parent_fd = os.open("/", DIRECTORY_FLAGS)
        try:
            for component in parts[:-1]:
                next_fd = os.open(component, DIRECTORY_FLAGS, dir_fd=parent_fd)
                os.close(parent_fd)
                parent_fd = next_fd
        except Exception:
            os.close(parent_fd)
            raise
    try:
        handle = os.open(parts[-1], FILE_READ_FLAGS, dir_fd=parent_fd)
    finally:
        os.close(parent_fd)
    metadata = os.fstat(handle)
    # Cargo may hardlink a finished artifact into target/release. That is safe
    # for this read-only, descriptor-anchored source: the copied bytes are
    # checked against their expected size and digest before publication. The
    # single-link invariant belongs to managed destinations that may be
    # replaced or removed, not to a source that is only read.
    if not stat.S_ISREG(metadata.st_mode):
        os.close(handle)
        _fail(f"installer source is not a regular file: {path}")
    if metadata.st_size < 0 or metadata.st_size > MAX_FILE_BYTES:
        os.close(handle)
        _fail(f"installer source exceeds its safety limit: {path}")
    return handle


def _source_identity(path: str) -> Tuple[int, str]:
    handle = _open_source(path)
    try:
        digest = hashlib.sha256()
        size = 0
        while True:
            chunk = os.read(handle, 1024 * 1024)
            if not chunk:
                break
            digest.update(chunk)
            size += len(chunk)
            if size > MAX_FILE_BYTES:
                _fail(f"installer source exceeds its safety limit: {path}")
        return size, digest.hexdigest()
    finally:
        os.close(handle)


def _validate_file_record(record: Any) -> Dict[str, Any]:
    if not isinstance(record, dict) or set(record) != {"path", "size", "sha256", "mode"}:
        _fail("install provenance contains an invalid file record")
    path = record.get("path")
    size = record.get("size")
    digest = record.get("sha256")
    mode = record.get("mode")
    if not isinstance(path, str) or path in (MANIFEST_RELATIVE, PENDING_RELATIVE, UNINSTALL_RELATIVE):
        _fail("install provenance contains an invalid file path")
    _relative_parts(path)
    if not _is_int(size) or size < 0 or size > MAX_FILE_BYTES:
        _fail("install provenance contains an invalid file size")
    if (
        not isinstance(digest, str)
        or len(digest) != 64
        or any(character not in "0123456789abcdef" for character in digest)
    ):
        _fail("install provenance contains an invalid file digest")
    if not _is_int(mode) or mode not in (0o644, 0o755):
        _fail("install provenance contains an invalid file mode")
    return {"path": path, "size": size, "sha256": digest, "mode": mode}


def _validate_directory_record(record: Any) -> Dict[str, Any]:
    if not isinstance(record, dict) or set(record) != {"path", "mode"}:
        _fail("install provenance contains an invalid directory record")
    path = record.get("path")
    mode = record.get("mode")
    if not isinstance(path, str) or not _is_int(mode) or mode != 0o755:
        _fail("install provenance contains an invalid directory identity")
    _relative_parts(path)
    return {"path": path, "mode": mode}


def _read_owned_json(root_fd: int, relative: str, mode: int, label: str) -> Optional[Any]:
    """The JSON in the owned, bounded regular file at `relative`, or `None`
    when there is none."""
    try:
        parent_fd, leaf = _open_parent(root_fd, relative)
    except FileNotFoundError:
        return None
    try:
        try:
            handle = os.open(leaf, FILE_READ_FLAGS, dir_fd=parent_fd)
        except FileNotFoundError:
            return None
    finally:
        os.close(parent_fd)
    try:
        metadata = os.fstat(handle)
        if (
            not stat.S_ISREG(metadata.st_mode)
            or metadata.st_nlink != 1
            or metadata.st_uid != os.geteuid()
            or stat.S_IMODE(metadata.st_mode) != mode
            or metadata.st_size <= 0
            or metadata.st_size > MAX_MANIFEST_BYTES
        ):
            _fail(f"{label} is not an owned bounded regular file")
        raw = _read_fd_bounded(handle, MAX_MANIFEST_BYTES)
    finally:
        os.close(handle)
    try:
        return json.loads(raw.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        _fail(f"{label} is not strict UTF-8 JSON: {error}")


def _read_pending(root_fd: int, prefix: str) -> Dict[str, List[Dict[str, Any]]]:
    """The files an interrupted install was writing, by path; empty when no
    install was interrupted."""
    document = _read_owned_json(root_fd, PENDING_RELATIVE, 0o600, "install receipt")
    if document is None:
        return {}
    expected = {"schema", "product", "managed_by", "prefix", "owner_uid", "files"}
    if (
        not isinstance(document, dict)
        or set(document) != expected
        or not _is_int(document["schema"])
        or document["schema"] != MANIFEST_SCHEMA
        or document["product"] != "kettle"
        or document["managed_by"] != "kettle-installer"
        or document["prefix"] != prefix
        or not _is_int(document["owner_uid"])
        or document["owner_uid"] != os.geteuid()
        or not isinstance(document["files"], list)
        or not 1 <= len(document["files"]) <= 2 * MAX_ENTRIES
    ):
        _fail("install receipt does not identify this installation")
    pending: Dict[str, List[Dict[str, Any]]] = {}
    for record in document["files"]:
        record = _validate_file_record(record)
        pending.setdefault(record["path"], []).append(record)
    return pending


def _publish_pending(
    root_fd: int,
    prefix: str,
    pending: Dict[str, List[Dict[str, Any]]],
    created: Dict[str, int],
) -> None:
    records = [record for path in sorted(pending) for record in pending[path]]
    encoded = (
        json.dumps(
            {
                "schema": MANIFEST_SCHEMA,
                "product": "kettle",
                "managed_by": "kettle-installer",
                "prefix": prefix,
                "owner_uid": os.geteuid(),
                "files": records,
            },
            indent=2,
        )
        + "\n"
    ).encode("utf-8")
    if len(encoded) > MAX_MANIFEST_BYTES:
        _fail("install receipt exceeds its bounded size")
    _publish_bytes(root_fd, PENDING_RELATIVE, encoded, 0o600, created)


def _read_manifest(
    root_fd: int, prefix: str, *, uninstall: bool = False
) -> Optional[Dict[str, Any]]:
    relative = UNINSTALL_RELATIVE if uninstall else MANIFEST_RELATIVE
    document = _read_owned_json(root_fd, relative, 0o600 if uninstall else 0o644, "install provenance")
    if document is None:
        return None
    expected = {
        "schema",
        "product",
        "managed_by",
        "prefix",
        "owner_uid",
        "files",
        "directories",
    }
    if not isinstance(document, dict) or set(document) != expected:
        _fail("install provenance has an unexpected schema")
    if (
        not _is_int(document["schema"])
        or document["schema"] != MANIFEST_SCHEMA
        or document["product"] != "kettle"
        or document["managed_by"] != "kettle-installer"
        or document["prefix"] != prefix
        or not _is_int(document["owner_uid"])
        or document["owner_uid"] != os.geteuid()
        or not isinstance(document["files"], list)
        or not isinstance(document["directories"], list)
        or not 1 <= len(document["files"]) <= (3 * MAX_ENTRIES if uninstall else MAX_ENTRIES)
        or len(document["directories"]) > MAX_ENTRIES
    ):
        _fail("install provenance does not identify this installation")
    files = [_validate_file_record(record) for record in document["files"]]
    directories = [
        _validate_directory_record(record) for record in document["directories"]
    ]
    file_paths = [record["path"] for record in files]
    directory_paths = [record["path"] for record in directories]
    if file_paths != sorted(file_paths) or len(set(file_paths)) != len(file_paths):
        _fail("install provenance file paths are not unique and sorted")
    if directory_paths != sorted(directory_paths) or len(set(directory_paths)) != len(directory_paths):
        _fail("install provenance directory paths are not unique and sorted")
    if not REQUIRED_PATHS.issubset(file_paths):
        _fail("install provenance is missing a required Kettle path")
    document["files"] = files
    document["directories"] = directories
    return document


def _verify_file(root_fd: int, record: Dict[str, Any]) -> None:
    parent_fd, leaf = _open_parent(root_fd, record["path"])
    try:
        handle = os.open(leaf, FILE_READ_FLAGS, dir_fd=parent_fd)
    except FileNotFoundError:
        os.close(parent_fd)
        _fail(f"recorded install file is missing: {record['path']}")
    except OSError as error:
        os.close(parent_fd)
        if error.errno in (errno.ELOOP, errno.ENOTDIR):
            _fail(f"recorded install file is a symlink: {record['path']}")
        raise
    os.close(parent_fd)
    try:
        metadata = os.fstat(handle)
        if (
            not stat.S_ISREG(metadata.st_mode)
            or metadata.st_nlink != 1
            or metadata.st_uid != os.geteuid()
            or stat.S_IMODE(metadata.st_mode) != record["mode"]
            or metadata.st_size != record["size"]
        ):
            _fail(f"recorded install file changed identity: {record['path']}")
        digest = hashlib.sha256()
        while True:
            chunk = os.read(handle, 1024 * 1024)
            if not chunk:
                break
            digest.update(chunk)
        if digest.hexdigest() != record["sha256"]:
            _fail(f"recorded install file changed content: {record['path']}")
    finally:
        os.close(handle)


def _verify_directory(root_fd: int, record: Dict[str, Any]) -> None:
    parts = _relative_parts(record["path"])
    handle = _open_relative_directory(root_fd, parts, create=False)
    try:
        metadata = os.fstat(handle)
        if metadata.st_uid != os.geteuid() or stat.S_IMODE(metadata.st_mode) != record["mode"]:
            _fail(f"recorded install directory changed identity: {record['path']}")
    finally:
        os.close(handle)


def _verify_manifest_tree(root_fd: int, manifest: Dict[str, Any]) -> None:
    for directory in manifest["directories"]:
        _verify_directory(root_fd, directory)
    for record in manifest["files"]:
        _verify_file(root_fd, record)


def _write_all(handle: int, data: bytes) -> None:
    offset = 0
    while offset < len(data):
        written = os.write(handle, data[offset:])
        if written <= 0:
            _fail("short write while staging installer content")
        offset += written


def _temporary_name() -> str:
    return ".kettle-install-tmp-" + secrets.token_hex(16)


def _publish_bytes(
    root_fd: int,
    relative: str,
    data: bytes,
    mode: int,
    created: Dict[str, int],
) -> None:
    parent_fd, leaf = _open_parent(root_fd, relative, create=True, created=created)
    temporary = _temporary_name()
    handle: Optional[int] = None
    try:
        handle = os.open(
            temporary,
            os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC,
            0o600,
            dir_fd=parent_fd,
        )
        os.fchmod(handle, mode)
        _write_all(handle, data)
        os.fsync(handle)
        os.close(handle)
        handle = None
        os.replace(temporary, leaf, src_dir_fd=parent_fd, dst_dir_fd=parent_fd)
        os.fsync(parent_fd)
    finally:
        if handle is not None:
            os.close(handle)
        try:
            os.unlink(temporary, dir_fd=parent_fd)
        except FileNotFoundError:
            pass
        os.close(parent_fd)


def _publish_source(
    root_fd: int,
    relative: str,
    source: str,
    expected_size: int,
    expected_digest: str,
    mode: int,
    created: Dict[str, int],
) -> None:
    parent_fd, leaf = _open_parent(root_fd, relative, create=True, created=created)
    temporary = _temporary_name()
    source_fd = _open_source(source)
    output_fd: Optional[int] = None
    try:
        output_fd = os.open(
            temporary,
            os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC,
            0o600,
            dir_fd=parent_fd,
        )
        os.fchmod(output_fd, mode)
        digest = hashlib.sha256()
        size = 0
        limit = min(expected_size, MAX_FILE_BYTES)
        while True:
            chunk = os.read(source_fd, min(1024 * 1024, limit + 1 - size))
            if not chunk:
                break
            size += len(chunk)
            if size > limit:
                _fail(f"installer source changed while copying: {source}")
            digest.update(chunk)
            _write_all(output_fd, chunk)
        if size != expected_size or digest.hexdigest() != expected_digest:
            _fail(f"installer source changed while copying: {source}")
        os.fsync(output_fd)
        os.close(output_fd)
        output_fd = None
        os.replace(temporary, leaf, src_dir_fd=parent_fd, dst_dir_fd=parent_fd)
        os.fsync(parent_fd)
    finally:
        os.close(source_fd)
        if output_fd is not None:
            os.close(output_fd)
        try:
            os.unlink(temporary, dir_fd=parent_fd)
        except FileNotFoundError:
            pass
        os.close(parent_fd)


def _desktop_string_escape(value: str) -> str:
    return value.replace("\\", "\\\\")


def _desktop_exec_quote(value: str) -> str:
    escaped = (
        value.replace("\\", "\\\\")
        .replace('"', '\\"')
        .replace("`", "\\`")
        .replace("$", "\\$")
        .replace("%", "%%")
    )
    return '"' + _desktop_string_escape(escaped) + '"'


def _render_desktop(template_path: str, binary: str, icon: str, record_dir: str) -> bytes:
    handle = _open_source(template_path)
    try:
        raw = _read_fd_bounded(handle, 1024 * 1024)
    finally:
        os.close(handle)
    try:
        text = raw.decode("utf-8")
    except UnicodeDecodeError as error:
        _fail(f"desktop template is not UTF-8: {error}")
    for label, value in (("binary", binary), ("icon", icon), ("record directory", record_dir)):
        if any(ord(character) < 32 or ord(character) == 127 for character in value):
            _fail(f"desktop {label} contains a control character")
    if "=" in binary:
        _fail("Desktop Entry executable paths cannot contain '='")
    lines = text.splitlines()
    expected = ("Exec=kettle", "TryExec=kettle", "Icon=kettle")
    for line in expected:
        if lines.count(line) != 1:
            _fail(f"desktop template must contain exactly one {line} entry")
    execution = _desktop_exec_quote(binary)
    if record_dir:
        execution = (
            "/usr/bin/env "
            + _desktop_exec_quote("KETTLE_RECORD_DIR=" + record_dir)
            + " "
            + execution
        )
    rendered = []
    for line in lines:
        if line == "Exec=kettle":
            rendered.append("Exec=" + execution)
        elif line == "TryExec=kettle":
            rendered.append("TryExec=" + _desktop_string_escape(binary))
        elif line == "Icon=kettle":
            rendered.append("Icon=" + _desktop_string_escape(icon))
        else:
            rendered.append(line)
    return ("\n".join(rendered) + "\n").encode("utf-8")


def _secure_record_directory(path: str) -> None:
    handle = _open_absolute_directory(path, create=True, final_mode=0o700)
    try:
        metadata = os.fstat(handle)
        if metadata.st_uid != os.geteuid():
            _fail("recording directory is not owned by the installing user")
        os.fchmod(handle, 0o700)
        os.fsync(handle)
    finally:
        os.close(handle)


def _parse_mode(value: str) -> int:
    try:
        mode = int(value, 8)
    except ValueError:
        _fail(f"invalid managed file mode: {value}")
    if mode not in (0o644, 0o755):
        _fail(f"unsupported managed file mode: {value}")
    return mode


def _marker_bytes(channel: str, target: str, version: str) -> bytes:
    if channel not in ("stable", "local-dev"):
        _fail("invalid install channel")
    if not target or any(character not in "abcdefghijklmnopqrstuvwxyz0123456789-_" for character in target):
        _fail("invalid install target")
    components = version.split(".")
    if version != "unknown" and (
        len(components) != 3 or any(not component.isdigit() for component in components)
    ):
        _fail("invalid installed version")
    marker = {
        "schema": 1,
        "product": "kettle",
        "managed_by": "kettle-installer",
        "channel": channel,
        "target": target,
        "version": version,
    }
    return (json.dumps(marker, indent=2) + "\n").encode("utf-8")


def _destination_exists(root_fd: int, relative: str) -> bool:
    try:
        parent_fd, leaf = _open_parent(root_fd, relative)
    except FileNotFoundError:
        return False
    try:
        try:
            os.stat(leaf, dir_fd=parent_fd, follow_symlinks=False)
            return True
        except FileNotFoundError:
            return False
    finally:
        os.close(parent_fd)


def _lock_identity(root_fd: int, handle: int) -> os.stat_result:
    metadata = os.fstat(handle)
    current = os.stat(UPDATE_LOCK, dir_fd=root_fd, follow_symlinks=False)
    if (
        not stat.S_ISREG(metadata.st_mode)
        or metadata.st_nlink != 1
        or metadata.st_uid != os.geteuid()
        or stat.S_IMODE(metadata.st_mode) != 0o600
        or metadata.st_size != 0
        or (metadata.st_dev, metadata.st_ino) != (current.st_dev, current.st_ino)
    ):
        _fail("update lock is not an unchanged owned empty private file")
    return metadata


@contextmanager
def _update_lock(root_fd: int) -> Iterator[int]:
    import fcntl

    flags = os.O_RDWR | os.O_NOFOLLOW | os.O_CLOEXEC | os.O_NONBLOCK
    created = False
    completed = False
    held = False
    handle = None
    try:
        try:
            handle = os.open(UPDATE_LOCK, flags | os.O_CREAT | os.O_EXCL, 0o600, dir_fd=root_fd)
            created = True
            os.fchmod(handle, 0o600)
        except FileExistsError:
            handle = os.open(UPDATE_LOCK, flags, dir_fd=root_fd)
        _lock_identity(root_fd, handle)
        try:
            fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            _fail("another Kettle operation holds the update lock")
        held = True
        _lock_identity(root_fd, handle)
        yield handle
        completed = True
    finally:
        if handle is not None:
            if created and held and not completed:
                try:
                    _lock_identity(root_fd, handle)
                    os.unlink(UPDATE_LOCK, dir_fd=root_fd)
                    os.fsync(root_fd)
                except (InstallerError, OSError):
                    pass
            os.close(handle)


def _install(arguments: argparse.Namespace) -> None:
    prefix = arguments.prefix
    _absolute_parts(prefix, "install prefix")
    if arguments.record_dir:
        _absolute_parts(arguments.record_dir, "recording directory")
        _secure_record_directory(arguments.record_dir)

    specs: List[Dict[str, Any]] = []
    seen = set()
    total_size = 0
    for relative, mode_text, source in arguments.file:
        _relative_parts(relative)
        if relative in (MANIFEST_RELATIVE, PENDING_RELATIVE, UNINSTALL_RELATIVE) or relative in seen:
            _fail(f"duplicate or reserved managed path: {relative}")
        seen.add(relative)
        mode = _parse_mode(mode_text)
        size, digest = _source_identity(source)
        total_size += size
        specs.append(
            {
                "path": relative,
                "mode": mode,
                "source": source,
                "size": size,
                "sha256": digest,
            }
        )

    desktop = _render_desktop(
        arguments.desktop_template,
        arguments.desktop_binary,
        arguments.desktop_icon,
        arguments.record_dir,
    )
    marker = _marker_bytes(arguments.channel, arguments.target, arguments.version)
    generated = [
        ("share/applications/kettle.desktop", desktop, 0o644),
        ("share/kettle/install.json", marker, 0o644),
    ]
    for relative, data, mode in generated:
        if relative in seen:
            _fail(f"duplicate generated managed path: {relative}")
        seen.add(relative)
        total_size += len(data)
        specs.append(
            {
                "path": relative,
                "mode": mode,
                "bytes": data,
                "size": len(data),
                "sha256": hashlib.sha256(data).hexdigest(),
            }
        )
    if not 1 <= len(specs) <= MAX_ENTRIES or total_size > MAX_FILE_BYTES:
        _fail("installer file plan exceeds its bounded limits")
    planned_parents = set()
    for spec in specs:
        parts = _relative_parts(spec["path"])
        for index in range(1, len(parts)):
            planned_parents.add("/".join(parts[:index]))
    manifest_parts = _relative_parts(MANIFEST_RELATIVE)
    for index in range(1, len(manifest_parts)):
        planned_parents.add("/".join(manifest_parts[:index]))
    if len(planned_parents) > MAX_ENTRIES:
        _fail("installer directory plan exceeds its bounded limit")

    root_fd = _open_absolute_directory(prefix, create=True)
    try:
        with _update_lock(root_fd):
            if _read_manifest(root_fd, prefix, uninstall=True) is not None:
                _fail("an interrupted uninstall must be completed before installing")
            existing = _read_manifest(root_fd, prefix)
            pending = _read_pending(root_fd, prefix)
            planned = {spec["path"]: _planned_record(spec) for spec in specs}

            def written_by_interrupted_install(path: str) -> bool:
                return any(_file_matches(root_fd, record) for record in pending.get(path, []))

            if existing is None:
                old_files: Dict[str, Dict[str, Any]] = {}
                owned_directories: Dict[str, int] = {}
            else:
                for directory in existing["directories"]:
                    _verify_directory(root_fd, directory)
                for record in existing["files"]:
                    if not written_by_interrupted_install(record["path"]):
                        _verify_file(root_fd, record)
                old_files = {record["path"]: record for record in existing["files"]}
                owned_directories = {
                    record["path"]: record["mode"] for record in existing["directories"]
                }
            for path, records in pending.items():
                for record in records:
                    if _file_matches(root_fd, record):
                        old_files[path] = record
                        break
            merged = {**old_files, **planned}
            if sum(record["size"] for record in merged.values()) > MAX_FILE_BYTES:
                _fail("install provenance file set exceeds its bounded size")
            if len(set(owned_directories).union(planned_parents)) > MAX_ENTRIES:
                _fail("install provenance directory set exceeds its bounded limit")
            if len(set(old_files).union(planned)) > MAX_ENTRIES:
                _fail("install provenance file set exceeds its bounded limit")

            for spec in specs:
                exists = _destination_exists(root_fd, spec["path"])
                if (
                    exists
                    and spec["path"] not in old_files
                    and not written_by_interrupted_install(spec["path"])
                ):
                    _fail(f"refusing to overwrite an unrecorded path: {spec['path']}")
            created: Dict[str, int] = {}
            # Name what this install will write before writing it, keeping
            # what an interrupted one wrote, so a retry can tell its own files
            # from anyone else's.
            for path, record in planned.items():
                if record not in pending.setdefault(path, []):
                    pending[path].append(record)
            _publish_pending(root_fd, prefix, pending, created)
            new_records: Dict[str, Dict[str, Any]] = dict(old_files)
            try:
                for spec in specs:
                    _publish_spec(root_fd, spec, created)
                    new_records[spec["path"]] = planned[spec["path"]]
            except BaseException:
                # Record what did change before giving up, so a retry or an
                # uninstall can still prove every file this install owns.
                for path, record in planned.items():
                    if path not in new_records or new_records[path] != record:
                        try:
                            if _file_matches(root_fd, record):
                                new_records[path] = record
                        except OSError:
                            pass
                owned_directories.update(created)
                if REQUIRED_PATHS.issubset(new_records):
                    try:
                        _publish_provenance(
                            root_fd, prefix, new_records, owned_directories, created
                        )
                    except (InstallerError, OSError):
                        pass
                raise
            owned_directories.update(created)
            _publish_provenance(root_fd, prefix, new_records, owned_directories, created)
            # Re-open and verify the complete committed set before reporting success.
            committed = _read_manifest(root_fd, prefix)
            if committed is None:
                _fail("install provenance publication failed")
            _verify_manifest_tree(root_fd, committed)
            _unlink_recorded(root_fd, PENDING_RELATIVE)
    finally:
        os.close(root_fd)


def _planned_record(spec: Dict[str, Any]) -> Dict[str, Any]:
    return {
        "path": spec["path"],
        "size": spec["size"],
        "sha256": spec["sha256"],
        "mode": spec["mode"],
    }


def _file_matches(root_fd: int, record: Optional[Dict[str, Any]]) -> bool:
    """Whether the file `record` describes is in place exactly as described."""
    if record is None:
        return False
    try:
        _verify_file(root_fd, record)
    except (InstallerError, FileNotFoundError):
        return False
    return True


def _publish_spec(root_fd: int, spec: Dict[str, Any], created: Dict[str, int]) -> None:
    if "bytes" in spec:
        _publish_bytes(root_fd, spec["path"], spec["bytes"], spec["mode"], created)
    else:
        _publish_source(
            root_fd,
            spec["path"],
            spec["source"],
            spec["size"],
            spec["sha256"],
            spec["mode"],
            created,
        )


def _publish_provenance(
    root_fd: int,
    prefix: str,
    files: Dict[str, Dict[str, Any]],
    directories: Dict[str, int],
    created: Dict[str, int],
) -> None:
    provenance = {
        "schema": MANIFEST_SCHEMA,
        "product": "kettle",
        "managed_by": "kettle-installer",
        "prefix": prefix,
        "owner_uid": os.geteuid(),
        "files": [files[path] for path in sorted(files)],
        "directories": [
            {"path": path, "mode": directories[path]} for path in sorted(directories)
        ],
    }
    encoded = (json.dumps(provenance, indent=2) + "\n").encode("utf-8")
    if len(encoded) > MAX_MANIFEST_BYTES:
        _fail("install provenance exceeds its bounded size")
    _publish_bytes(root_fd, MANIFEST_RELATIVE, encoded, 0o644, created)


def _unlink_recorded(root_fd: int, relative: str) -> None:
    parent_fd, leaf = _open_parent(root_fd, relative)
    try:
        os.unlink(leaf, dir_fd=parent_fd)
        os.fsync(parent_fd)
    finally:
        os.close(parent_fd)


def _remove_recorded_directory(root_fd: int, relative: str) -> None:
    parts = _relative_parts(relative)
    parent = _open_relative_directory(root_fd, parts[:-1], create=False)
    try:
        try:
            os.rmdir(parts[-1], dir_fd=parent)
            os.fsync(parent)
        except OSError as error:
            if error.errno not in (errno.ENOENT, errno.ENOTEMPTY):
                raise
    finally:
        os.close(parent)


def _uninstall(arguments: argparse.Namespace) -> None:
    prefix = arguments.prefix
    root_fd = _open_absolute_directory(prefix, create=False)
    try:
        with _update_lock(root_fd) as lock:
            journal = _read_manifest(root_fd, prefix, uninstall=True)
            if journal is None:
                manifest = _read_manifest(root_fd, prefix)
                if manifest is None:
                    if _read_pending(root_fd, prefix):
                        _fail(
                            "an interrupted install left files here; run the installer "
                            "again to finish it, then uninstall"
                        )
                    _fail(
                        "no Kettle install provenance was found; refusing to guess which "
                        "paths belong to this installation"
                    )
                pending = _read_pending(root_fd, prefix)
                for directory in manifest["directories"]:
                    _verify_directory(root_fd, directory)
                records = {record["path"]: record for record in manifest["files"]}
                for path, candidates in pending.items():
                    for record in candidates:
                        if _file_matches(root_fd, record):
                            records[path] = record
                            break
                for record in records.values():
                    _verify_file(root_fd, record)
                journal = dict(manifest, files=[records[path] for path in sorted(records)])
                encoded = (json.dumps(journal, indent=2) + "\n").encode("utf-8")
                if len(encoded) > MAX_MANIFEST_BYTES:
                    _fail("uninstall journal exceeds its bounded size")
                _publish_bytes(root_fd, UNINSTALL_RELATIVE, encoded, 0o600, {})
            # Missing journaled objects have already been removed. Every remaining
            # object must still prove its identity before any deletion resumes.
            for directory in journal["directories"]:
                if _destination_exists(root_fd, directory["path"]):
                    _verify_directory(root_fd, directory)
            remaining = []
            for record in journal["files"]:
                if _destination_exists(root_fd, record["path"]):
                    _verify_file(root_fd, record)
                    remaining.append(record["path"])
            for path in sorted(
                set(remaining).difference(RECOVERY_PATHS),
                key=lambda path: (path.count("/"), path), reverse=True,
            ):
                _unlink_recorded(root_fd, path)
            for path in (PENDING_RELATIVE, MANIFEST_RELATIVE, *RECOVERY_PATHS):
                if _destination_exists(root_fd, path):
                    _unlink_recorded(root_fd, path)
            directories = sorted(
                journal["directories"],
                key=lambda item: (item["path"].count("/"), item["path"]),
                reverse=True,
            )
            for record in directories:
                try:
                    _remove_recorded_directory(root_fd, record["path"])
                except FileNotFoundError:
                    pass
            _unlink_recorded(root_fd, UNINSTALL_RELATIVE)
            # The journal kept its ancestors nonempty until its final unlink.
            for record in directories:
                try:
                    _remove_recorded_directory(root_fd, record["path"])
                except FileNotFoundError:
                    pass
            _lock_identity(root_fd, lock)
            os.unlink(UPDATE_LOCK, dir_fd=root_fd)
            os.fsync(root_fd)
    finally:
        os.close(root_fd)


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="operation", required=True)
    install = subparsers.add_parser("install")
    install.add_argument("--prefix", required=True)
    install.add_argument("--channel", required=True)
    install.add_argument("--target", required=True)
    install.add_argument("--version", required=True)
    install.add_argument("--desktop-template", required=True)
    install.add_argument("--desktop-binary", required=True)
    install.add_argument("--desktop-icon", required=True)
    install.add_argument("--record-dir", default="")
    install.add_argument("--file", nargs=3, action="append", default=[])
    uninstall = subparsers.add_parser("uninstall")
    uninstall.add_argument("--prefix", required=True)
    return parser


def main() -> int:
    try:
        arguments = _parser().parse_args()
        if arguments.operation == "install":
            _install(arguments)
        else:
            _uninstall(arguments)
        return 0
    except (InstallerError, FileNotFoundError, PermissionError, OSError) as error:
        print(f"kettle installer: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
