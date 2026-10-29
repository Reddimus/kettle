#!/usr/bin/env python3
"""Hermetic tests for package-manifest.py."""

from __future__ import annotations

import gzip
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import shutil
import stat
import struct
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest import mock
import warnings
import zipfile


sys.dont_write_bytecode = True
SCRIPT = Path(__file__).with_name("package-manifest.py")
SPEC = importlib.util.spec_from_file_location("package_manifest", SCRIPT)
assert SPEC and SPEC.loader
MODULE = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = MODULE
SPEC.loader.exec_module(MODULE)

VERSION = "2.36.0"
WINDOWS_TARGET = "x86_64-pc-windows-msvc"
LINUX_TARGET = "x86_64-unknown-linux-gnu"


def manifest_bytes(
    target: str,
    files: dict[str, tuple[bytes, int]],
    *,
    version: str = VERSION,
    mutate=None,
) -> bytes:
    records = []
    for path, (data, mode) in sorted(files.items()):
        records.append(
            {
                "path": path,
                "size": len(data),
                "sha256": hashlib.sha256(data).hexdigest(),
                "mode": mode if target.endswith("linux-gnu") else None,
            }
        )
    manifest = {
        "schema": MODULE.SCHEMA,
        "product": "kettle",
        "target": target,
        "version": version,
        "files": records,
    }
    if mutate is not None:
        mutate(manifest)
    return MODULE.encode(manifest)


def zip_info(
    name: str,
    *,
    mode: int = 0o644,
    file_type: int = stat.S_IFREG,
) -> zipfile.ZipInfo:
    info = zipfile.ZipInfo(name)
    info.create_system = 3
    info.external_attr = (file_type | mode) << 16
    info.compress_type = zipfile.ZIP_DEFLATED
    return info


def write_windows_zip(
    path: Path,
    files: dict[str, tuple[bytes, int]],
    *,
    manifest: bytes | None = None,
    extras: list[tuple[zipfile.ZipInfo, bytes]] | None = None,
) -> None:
    if manifest is None:
        manifest = manifest_bytes(WINDOWS_TARGET, files)
    directories = {
        "/".join(file_path.split("/")[:index])
        for file_path in files
        for index in range(1, len(file_path.split("/")))
    }
    with zipfile.ZipFile(path, "w") as archive:
        for directory in sorted(directories):
            archive.writestr(
                zip_info(
                    f"{directory}/", mode=0o755, file_type=stat.S_IFDIR
                ),
                b"",
            )
        for file_path, (data, _mode) in sorted(files.items()):
            archive.writestr(zip_info(file_path), data)
        for info, data in extras or []:
            with warnings.catch_warnings():
                warnings.simplefilter("ignore", UserWarning)
                archive.writestr(info, data)
        archive.writestr(
            zip_info(MODULE.MANIFEST_NAME, mode=0o600), manifest
        )


def tar_entry(
    name: str,
    data: bytes = b"",
    *,
    mode: int = 0o644,
    entry_type: bytes = tarfile.REGTYPE,
    linkname: str = "",
    pax_headers: dict[str, str] | None = None,
) -> tuple[tarfile.TarInfo, bytes]:
    info = tarfile.TarInfo(name)
    info.type = entry_type
    info.mode = mode
    info.linkname = linkname
    info.size = len(data) if entry_type in {tarfile.REGTYPE, tarfile.AREGTYPE} else 0
    if pax_headers:
        info.pax_headers = pax_headers
    return info, data


def write_linux_tar(
    path: Path,
    files: dict[str, tuple[bytes, int]],
    *,
    manifest: bytes | None = None,
    extras: list[tuple[tarfile.TarInfo, bytes]] | None = None,
    archive_format: int = tarfile.GNU_FORMAT,
) -> None:
    if manifest is None:
        manifest = manifest_bytes(LINUX_TARGET, files)
    directories = {
        "/".join(file_path.split("/")[:index])
        for file_path in files
        for index in range(1, len(file_path.split("/")))
    }
    with tarfile.open(path, "w:gz", format=archive_format) as archive:
        root, _ = tar_entry("kettle", mode=0o755, entry_type=tarfile.DIRTYPE)
        archive.addfile(root)
        for directory in sorted(directories):
            info, _ = tar_entry(
                f"kettle/{directory}", mode=0o755, entry_type=tarfile.DIRTYPE
            )
            archive.addfile(info)
        for file_path, (data, mode) in sorted(files.items()):
            info, payload = tar_entry(f"kettle/{file_path}", data, mode=mode)
            archive.addfile(info, io.BytesIO(payload))
        for info, data in extras or []:
            source = (
                io.BytesIO(data)
                if info.type in {tarfile.REGTYPE, tarfile.AREGTYPE}
                else None
            )
            archive.addfile(info, source)
        info, payload = tar_entry(
            f"kettle/{MODULE.MANIFEST_NAME}", manifest, mode=0o600
        )
        archive.addfile(info, io.BytesIO(payload))


def mark_first_zip_entry_encrypted(path: Path) -> None:
    data = bytearray(path.read_bytes())
    local = data.find(b"PK\x03\x04")
    central = data.find(b"PK\x01\x02")
    assert local >= 0 and central >= 0
    local_flags = struct.unpack_from("<H", data, local + 6)[0] | 0x1
    central_flags = struct.unpack_from("<H", data, central + 8)[0] | 0x1
    struct.pack_into("<H", data, local + 6, local_flags)
    struct.pack_into("<H", data, central + 8, central_flags)
    path.write_bytes(data)


class PackageManifestTests(unittest.TestCase):
    def package(self, root: Path) -> None:
        shell_integration = root / "shell-integration"
        shell_integration.mkdir()
        # This fixture models the installed package, whose directory modes are
        # explicit. Ubuntu commonly runs tests with umask 0002; leaving this to
        # the ambient umask creates 0775 and makes the security validator
        # correctly reject the fixture itself.
        shell_integration.chmod(0o755)
        binary = root / "kettle"
        binary.write_bytes(b"binary\n")
        binary.chmod(0o755)
        script = shell_integration / "kettle.sh"
        script.write_bytes(b"prompt\n")
        script.chmod(0o644)

    @unittest.skipUnless(os.name == "posix", "shared POSIX output ancestor")
    def test_shared_parent_cannot_redirect_descendant_writes(self):
        with tempfile.TemporaryDirectory() as raw:
            temporary = Path(raw).resolve()
            shared = temporary / "shared"
            shared.mkdir(mode=0o777)
            shared.chmod(0o777)
            external = temporary / "external"
            external.mkdir()
            output = shared / "output"
            archive = temporary / "input.tar.gz"
            write_linux_tar(archive, {"nested/file": (b"data", 0o644)})
            create_directories = MODULE._create_output_directories
            def replace_root(root, members):
                root.rename(shared / "displaced")
                root.symlink_to(external, target_is_directory=True)
                create_directories(root, members)
            with mock.patch.object(MODULE, "_create_output_directories", side_effect=replace_root):
                with self.assertRaises((ValueError, RuntimeError)):
                    MODULE.extract_archive(archive, output, LINUX_TARGET, VERSION)
            self.assertEqual(list(external.iterdir()), [], "extraction wrote through replaced root")

    @unittest.skipIf(os.name == "nt", "Linux package modes require POSIX")
    def test_pack_rejects_compressed_output_over_limit_without_publishing(self):
        with tempfile.TemporaryDirectory() as raw:
            temporary = Path(raw)
            root = temporary / "package"
            root.mkdir(mode=0o755)
            self.package(root)
            MODULE.generate(root, LINUX_TARGET, VERSION)
            archive = temporary / "archive.tar.gz"
            with mock.patch.object(MODULE, "MAX_ARCHIVE_BYTES", 1):
                with self.assertRaisesRegex(ValueError, "archive.*limit"):
                    MODULE.pack_archive(archive, root, LINUX_TARGET, VERSION)
            self.assertFalse(archive.exists())
            self.assertEqual(sorted(p.name for p in temporary.iterdir()), ["package"])

    @unittest.skipIf(os.name == "nt", "Linux package modes require POSIX")
    def test_pack_extract_relative_path_limit_includes_longname(self):
        for length in (234, MODULE.MAX_PACKAGE_PATH_BYTES):
            with self.subTest(length=length), tempfile.TemporaryDirectory() as raw:
                temporary = Path(raw)
                root = temporary / "package"
                root.mkdir(mode=0o755)
                self.package(root)
                name = "a" * length
                (root / name).write_bytes(b"boundary")
                (root / name).chmod(0o644)
                MODULE.generate(root, LINUX_TARGET, VERSION)
                archive = temporary / "archive.tar.gz"
                MODULE.pack_archive(archive, root, LINUX_TARGET, VERSION)
                output = temporary / "output"
                try:
                    MODULE.extract_archive(archive, output, LINUX_TARGET, VERSION)
                except ValueError as error:
                    self.fail(f"valid relative path failed round trip: {error}")
                self.assertEqual((output / name).read_bytes(), b"boundary")

    def test_generation_is_deterministic_and_verifiable(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            self.package(root)
            target = WINDOWS_TARGET if os.name == "nt" else LINUX_TARGET
            first = MODULE.build_manifest(root, target, VERSION)
            second = MODULE.build_manifest(root, target, VERSION)
            self.assertEqual(first, second)
            self.assertEqual(
                [item["path"] for item in first["files"]],
                ["kettle", "shell-integration/kettle.sh"],
            )
            self.assertEqual(
                first["files"][0]["mode"], None if os.name == "nt" else 0o755
            )
            MODULE.generate(root, target, VERSION)
            MODULE.verify(root, target, VERSION)

            (root / "kettle").write_bytes(b"mutated\n")
            with self.assertRaisesRegex(ValueError, "does not match"):
                MODULE.verify(root, target, VERSION)

    @unittest.skipIf(os.name == "nt", "Linux package modes require POSIX")
    def test_pack_preserves_nested_licenses_at_the_archive_entry_limit(self):
        with tempfile.TemporaryDirectory() as raw:
            temporary = Path(raw)
            root = temporary / "package"
            root.mkdir(mode=0o755)
            self.package(root)
            for index in range(124):
                license_path = root / f"docs/licenses/dependency-{index}/LICENSE"
                license_path.parent.mkdir(parents=True, exist_ok=True)
                license_path.write_bytes(f"License {index}\n".encode("ascii"))
                license_path.chmod(0o644)
            for directory, _dirs, _files in os.walk(root):
                Path(directory).chmod(0o755)
            MODULE.generate(root, LINUX_TARGET, VERSION)

            archive = temporary / "kettle.tar.gz"

            def cli(operation: str, directory: Path) -> None:
                subprocess.run(
                    [
                        sys.executable, str(SCRIPT), operation,
                        "--archive", str(archive), "--root", str(directory),
                        "--target", LINUX_TARGET, "--version", VERSION,
                    ],
                    check=True,
                    capture_output=True,
                )

            cli("pack", root)
            with tarfile.open(archive) as packed:
                members = packed.getmembers()
                self.assertEqual(len(members), 128)
                self.assertEqual(
                    [member.name for member in members if member.isdir()], ["kettle"]
                )
                self.assertEqual(sum(member.isfile() for member in members), 127)
            output = temporary / "output"
            cli("extract", output)
            MODULE.verify(output, LINUX_TARGET, VERSION)
            for index in range(124):
                self.assertEqual(
                    (
                        output / f"docs/licenses/dependency-{index}/LICENSE"
                    ).read_bytes(),
                    f"License {index}\n".encode("ascii"),
                )
            self.assertEqual((output / "kettle").stat().st_mode & 0o777, 0o755)
            (root / "one-more").write_bytes(b"limit\n")
            (root / "one-more").chmod(0o644)
            MODULE.generate(root, LINUX_TARGET, VERSION)
            overflow = temporary / "overflow.tar.gz"
            with self.assertRaisesRegex(ValueError, "128-entry limit"):
                MODULE.pack_archive(overflow, root, LINUX_TARGET, VERSION)
            self.assertFalse(overflow.exists())

    @unittest.skipIf(os.name == "nt", "Linux package modes require POSIX")
    def test_pack_requires_verified_contents_and_an_external_output(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw) / "package"
            root.mkdir(mode=0o755)
            self.package(root)
            MODULE.generate(root, LINUX_TARGET, VERSION)
            with self.assertRaisesRegex(ValueError, "outside"):
                MODULE.pack_archive(
                    root / "archive.tar.gz", root, LINUX_TARGET, VERSION
                )
            archive = Path(raw) / "archive.tar.gz"
            (root / "kettle").write_bytes(b"changed\n")
            with self.assertRaisesRegex(ValueError, "does not match"):
                MODULE.pack_archive(archive, root, LINUX_TARGET, VERSION)
            self.assertFalse(archive.exists())

    def test_windows_modes_are_null_and_identity_is_exact(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            self.package(root)
            manifest = MODULE.build_manifest(root, WINDOWS_TARGET, VERSION)
            self.assertTrue(all(item["mode"] is None for item in manifest["files"]))
            MODULE.generate(root, WINDOWS_TARGET, VERSION)
            with self.assertRaisesRegex(ValueError, "identity|does not match"):
                MODULE.verify(root, WINDOWS_TARGET, "2.36.1")
            with self.assertRaisesRegex(ValueError, "stable"):
                MODULE.build_manifest(root, WINDOWS_TARGET, "2.36.0-rc.1")

    def test_rejects_symlinks_and_case_folded_duplicates(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            self.package(root)
            link = root / "link"
            try:
                link.symlink_to("kettle")
            except OSError as error:
                if os.name != "nt" or getattr(error, "winerror", None) != 1314:
                    raise
            else:
                with self.assertRaisesRegex(ValueError, "non-regular"):
                    MODULE.build_manifest(root, WINDOWS_TARGET, VERSION)
                link.unlink()

            paths = MODULE.ArchivePaths()
            paths.insert("README", False)
            with self.assertRaisesRegex(ValueError, "duplicate|alias"):
                paths.insert("readme", False)

            case_probe = root / "case-probe"
            case_probe.mkdir()
            upper = case_probe / "README"
            lower = case_probe / "readme"
            upper.write_bytes(b"upper\n")
            lower.write_bytes(b"lower\n")
            try:
                if not os.path.samefile(upper, lower):
                    with self.assertRaisesRegex(ValueError, "duplicate|alias"):
                        MODULE.build_manifest(root, WINDOWS_TARGET, VERSION)
            finally:
                lower.unlink(missing_ok=True)
                upper.unlink(missing_ok=True)
                case_probe.rmdir()

    def test_verifier_requires_canonical_manifest(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            self.package(root)
            target = WINDOWS_TARGET if os.name == "nt" else LINUX_TARGET
            manifest = MODULE.build_manifest(root, target, VERSION)
            (root / MODULE.MANIFEST_NAME).write_text(
                json.dumps(manifest, indent=2), encoding="ascii"
            )
            with self.assertRaisesRegex(ValueError, "canonical"):
                MODULE.verify(root, target, VERSION)


class ArchiveExtractionTests(unittest.TestCase):
    windows_files = {
        "kettle.exe": (b"windows-binary\n", 0o755),
        "shell-integration/kettle.ps1": (b"Write-Output kettle\n", 0o644),
    }
    linux_files = {
        "kettle": (b"linux-binary\n", 0o755),
        "shell-integration/kettle.sh": (b"printf kettle\n", 0o644),
    }

    def test_output_parent_is_canonicalized_once_before_creation(self):
        with tempfile.TemporaryDirectory() as raw:
            temporary = Path(raw)
            real_parent = temporary / "real"
            real_parent.mkdir()
            alias_parent = temporary / "alias"
            try:
                alias_parent.symlink_to(real_parent, target_is_directory=True)
            except OSError as error:
                if os.name == "nt" and getattr(error, "winerror", None) == 1314:
                    self.skipTest("creating a directory symlink requires Windows privilege")
                raise

            output, identity = MODULE._prepare_output_root(alias_parent / "output")
            self.assertEqual(output, real_parent.resolve() / "output")
            self.assertEqual((output.lstat().st_dev, output.lstat().st_ino), identity)
            self.assertFalse(output.is_symlink())
            output.rmdir()

    def test_rejects_preexisting_output_root_symlink(self):
        with tempfile.TemporaryDirectory() as raw:
            temporary = Path(raw)
            target = temporary / "target"
            target.mkdir()
            output = temporary / "output"
            try:
                output.symlink_to(target, target_is_directory=True)
            except OSError as error:
                if os.name == "nt" and getattr(error, "winerror", None) == 1314:
                    self.skipTest("creating a directory symlink requires Windows privilege")
                raise

            with self.assertRaisesRegex(ValueError, "must not already exist"):
                MODULE._prepare_output_root(output)

    def test_extracts_flat_windows_zip(self):
        with tempfile.TemporaryDirectory() as raw:
            temporary = Path(raw)
            archive = temporary / "kettle.zip"
            output = temporary / "package"
            write_windows_zip(archive, self.windows_files)

            subprocess.run(
                [
                    sys.executable,
                    os.fspath(SCRIPT),
                    "extract",
                    "--archive",
                    os.fspath(archive),
                    "--root",
                    os.fspath(output),
                    "--target",
                    WINDOWS_TARGET,
                    "--version",
                    VERSION,
                ],
                check=True,
            )

            self.assertEqual(
                (output / "kettle.exe").read_bytes(), b"windows-binary\n"
            )
            self.assertEqual(
                (output / "shell-integration" / "kettle.ps1").read_bytes(),
                b"Write-Output kettle\n",
            )
            MODULE.verify(output, WINDOWS_TARGET, VERSION)

    @unittest.skipUnless(os.name == "nt", "PowerShell packaging requires Windows")
    def test_extracts_powershell_compress_archive_output(self):
        powershell = shutil.which("pwsh")
        if powershell is None:
            self.skipTest("pwsh is unavailable")
        with tempfile.TemporaryDirectory() as raw:
            temporary = Path(raw)
            stage = temporary / "stage"
            output = temporary / "package"
            archive = temporary / "kettle.zip"
            (stage / "shell-integration").mkdir(parents=True)
            (stage / "kettle.exe").write_bytes(b"windows-binary\n")
            (stage / "shell-integration" / "kettle.ps1").write_bytes(
                b"Write-Output kettle\n"
            )
            MODULE.generate(stage, WINDOWS_TARGET, VERSION)
            environment = os.environ.copy()
            environment["KETTLE_TEST_STAGE"] = os.fspath(stage)
            environment["KETTLE_TEST_ARCHIVE"] = os.fspath(archive)
            subprocess.run(
                [
                    powershell,
                    "-NoProfile",
                    "-Command",
                    "Compress-Archive "
                    "-Path (Join-Path $env:KETTLE_TEST_STAGE '*') "
                    "-DestinationPath $env:KETTLE_TEST_ARCHIVE -Force",
                ],
                check=True,
                env=environment,
            )

            MODULE.extract_archive(archive, output, WINDOWS_TARGET, VERSION)
            MODULE.verify(output, WINDOWS_TARGET, VERSION)

    @unittest.skipIf(os.name == "nt", "POSIX mode verification requires Linux")
    def test_extracts_single_root_linux_tar(self):
        with tempfile.TemporaryDirectory() as raw:
            temporary = Path(raw)
            archive = temporary / "kettle.tar.gz"
            output = temporary / "package"
            write_linux_tar(archive, self.linux_files)

            MODULE.extract_archive(archive, output, LINUX_TARGET, VERSION)

            self.assertEqual((output / "kettle").read_bytes(), b"linux-binary\n")
            self.assertEqual(
                stat.S_IMODE((output / "kettle").stat().st_mode), 0o755
            )
            self.assertFalse((output / "kettle").is_dir())
            MODULE.verify(output, LINUX_TARGET, VERSION)

    def test_rejects_unsafe_zip_paths_before_writing(self):
        unsafe_paths = [
            "../escape",
            "./dot",
            "/absolute",
            "C:drive",
            "back\\slash",
            "two//slashes",
            "NUL.txt",
            "non-ascii-\N{SNOWMAN}",
            "control-\x01",
        ]
        with tempfile.TemporaryDirectory() as raw:
            temporary = Path(raw)
            for index, unsafe in enumerate(unsafe_paths):
                with self.subTest(path=unsafe):
                    archive = temporary / f"unsafe-{index}.zip"
                    output = temporary / f"output-{index}"
                    write_windows_zip(
                        archive,
                        {"ok.exe": (b"ok", 0o755)},
                        extras=[(zip_info(unsafe), b"bad")],
                    )
                    with self.assertRaises(ValueError):
                        MODULE.extract_archive(
                            archive, output, WINDOWS_TARGET, VERSION
                        )
                    self.assertFalse(output.exists())

    def test_rejects_zip_aliases_conflicts_special_and_encryption(self):
        cases = {
            "duplicate": [(zip_info("kettle.exe"), b"duplicate")],
            "case-alias": [(zip_info("KETTLE.EXE"), b"alias")],
            "prefix": [(zip_info("kettle.exe/child"), b"conflict")],
            "symlink": [
                (
                    zip_info(
                        "link",
                        mode=0o755,
                        file_type=stat.S_IFLNK,
                    ),
                    b"kettle.exe",
                )
            ],
            "empty-directory": [
                (
                    zip_info(
                        "unused/", mode=0o755, file_type=stat.S_IFDIR
                    ),
                    b"",
                )
            ],
        }
        with tempfile.TemporaryDirectory() as raw:
            temporary = Path(raw)
            for name, extras in cases.items():
                with self.subTest(case=name):
                    archive = temporary / f"{name}.zip"
                    output = temporary / f"{name}-output"
                    write_windows_zip(
                        archive, self.windows_files, extras=extras
                    )
                    with self.assertRaises(ValueError):
                        MODULE.extract_archive(
                            archive, output, WINDOWS_TARGET, VERSION
                        )
                    self.assertFalse(output.exists())

            archive = temporary / "encrypted.zip"
            output = temporary / "encrypted-output"
            write_windows_zip(archive, self.windows_files)
            mark_first_zip_entry_encrypted(archive)
            with self.assertRaisesRegex(ValueError, "encrypted"):
                MODULE.extract_archive(archive, output, WINDOWS_TARGET, VERSION)
            self.assertFalse(output.exists())

    def test_rejects_more_than_128_archive_entries(self):
        with tempfile.TemporaryDirectory() as raw:
            temporary = Path(raw)
            archive = temporary / "too-many.zip"
            output = temporary / "output"
            extras = [
                (zip_info(f"extra-{index}"), b"x")
                for index in range(MODULE.MAX_ARCHIVE_ENTRIES)
            ]
            write_windows_zip(
                archive, {"kettle.exe": (b"ok", 0o755)}, extras=extras
            )
            with self.assertRaisesRegex(ValueError, "more than 128"):
                MODULE.extract_archive(
                    archive, output, WINDOWS_TARGET, VERSION
                )
            self.assertFalse(output.exists())

    def test_rejects_noncanonical_and_unbound_manifests(self):
        mutations = {
            "size": lambda document: document["files"][0].__setitem__("size", 99),
            "hash": lambda document: document["files"][0].__setitem__(
                "sha256", "0" * 64
            ),
            "path": lambda document: document["files"][0].__setitem__(
                "path", "renamed.exe"
            ),
        }
        with tempfile.TemporaryDirectory() as raw:
            temporary = Path(raw)
            for name, mutate in mutations.items():
                with self.subTest(case=name):
                    archive = temporary / f"manifest-{name}.zip"
                    output = temporary / f"manifest-{name}-output"
                    manifest = manifest_bytes(
                        WINDOWS_TARGET, self.windows_files, mutate=mutate
                    )
                    write_windows_zip(
                        archive, self.windows_files, manifest=manifest
                    )
                    with self.assertRaises(ValueError):
                        MODULE.extract_archive(
                            archive, output, WINDOWS_TARGET, VERSION
                        )
                    self.assertFalse(output.exists())

            archive = temporary / "noncanonical.zip"
            output = temporary / "noncanonical-output"
            canonical = manifest_bytes(WINDOWS_TARGET, self.windows_files)
            noncanonical = json.dumps(json.loads(canonical), indent=2).encode("ascii")
            write_windows_zip(
                archive, self.windows_files, manifest=noncanonical
            )
            with self.assertRaisesRegex(ValueError, "canonical"):
                MODULE.extract_archive(archive, output, WINDOWS_TARGET, VERSION)
            self.assertFalse(output.exists())

            archive = temporary / "duplicate-key.zip"
            output = temporary / "duplicate-key-output"
            duplicate_key = canonical.replace(
                b'"schema":1', b'"schema":1,"schema":1', 1
            )
            write_windows_zip(
                archive, self.windows_files, manifest=duplicate_key
            )
            with self.assertRaisesRegex(ValueError, "duplicate JSON key"):
                MODULE.extract_archive(archive, output, WINDOWS_TARGET, VERSION)
            self.assertFalse(output.exists())

    def test_rejects_malicious_tar_entries_and_modes(self):
        malicious_entries = {
            "outside-root": tar_entry("outside", b"bad"),
            "traversal": tar_entry("kettle/../escape", b"bad"),
            "backslash": tar_entry("kettle/bad\\name", b"bad"),
            "symlink": tar_entry(
                "kettle/link",
                entry_type=tarfile.SYMTYPE,
                linkname="kettle",
            ),
            "hardlink": tar_entry(
                "kettle/hardlink",
                entry_type=tarfile.LNKTYPE,
                linkname="kettle/kettle",
            ),
            "fifo": tar_entry("kettle/fifo", entry_type=tarfile.FIFOTYPE),
            "gnu-sparse": tar_entry(
                "kettle/sparse", entry_type=tarfile.GNUTYPE_SPARSE
            ),
            "group-writable": tar_entry("kettle/writable", b"bad", mode=0o664),
            "setuid": tar_entry("kettle/setuid", b"bad", mode=0o4755),
        }
        with tempfile.TemporaryDirectory() as raw:
            temporary = Path(raw)
            for name, extra in malicious_entries.items():
                with self.subTest(case=name):
                    archive = temporary / f"{name}.tar.gz"
                    output = temporary / f"{name}-output"
                    write_linux_tar(
                        archive, self.linux_files, extras=[extra]
                    )
                    with self.assertRaises(ValueError):
                        MODULE.extract_archive(
                            archive, output, LINUX_TARGET, VERSION
                        )
                    self.assertFalse(output.exists())

            archive = temporary / "pax.tar.gz"
            output = temporary / "pax-output"
            pax = tar_entry(
                "kettle/pax",
                b"bad",
                pax_headers={"comment": "forbidden"},
            )
            write_linux_tar(
                archive,
                self.linux_files,
                extras=[pax],
                archive_format=tarfile.PAX_FORMAT,
            )
            with self.assertRaisesRegex(ValueError, "PAX"):
                MODULE.extract_archive(archive, output, LINUX_TARGET, VERSION)
            self.assertFalse(output.exists())

    def test_bounds_tar_metadata_before_extension_processing(self):
        cases = [
            (tarfile.GNUTYPE_LONGNAME, MODULE.MAX_PACKAGE_PATH_BYTES + 2),
            (tarfile.GNUTYPE_LONGNAME, 4 * 1024 ** 3),
            (tarfile.GNUTYPE_LONGLINK, 4 * 1024 ** 3),
            (tarfile.XHDTYPE, 4 * 1024 ** 3),
            (tarfile.XGLTYPE, 4 * 1024 ** 3),
            (tarfile.SOLARIS_XHDTYPE, 4 * 1024 ** 3),
        ]
        with tempfile.TemporaryDirectory() as raw:
            temporary = Path(raw)
            for kind, size in cases:
                for first in (True, False):
                    with self.subTest(kind=kind, size=size, first=first):
                        archive = temporary / "metadata.tar.gz"
                        output = temporary / "output"
                        header = tarfile.TarInfo("././@LongLink")
                        header.type = kind
                        header.size = size
                        with gzip.open(archive, "wb") as stream:
                            if not first:
                                root, _ = tar_entry("kettle", mode=0o755, entry_type=tarfile.DIRTYPE)
                                stream.write(root.tobuf(format=tarfile.GNU_FORMAT))
                            stream.write(header.tobuf(format=tarfile.GNU_FORMAT))
                            stream.write(b"\0" * 1024)
                        with mock.patch.object(
                            tarfile.TarInfo, "_proc_gnulong",
                            side_effect=AssertionError("GNU metadata was processed"),
                        ), mock.patch.object(
                            tarfile.TarInfo, "_proc_pax",
                            side_effect=AssertionError("PAX metadata was processed"),
                        ):
                            with self.assertRaisesRegex(ValueError, "metadata|PAX|special tar"):
                                MODULE.extract_archive(archive, output, LINUX_TARGET, VERSION)
                        self.assertFalse(output.exists())

    @unittest.skipIf(os.name == "nt", "POSIX mode verification requires Linux")
    def test_accepts_bounded_gnu_longname(self):
        with tempfile.TemporaryDirectory() as raw:
            temporary = Path(raw)
            archive = temporary / "longname.tar.gz"
            output = temporary / "output"
            files = dict(self.linux_files)
            name = "a" * 120
            files[name] = (b"long path payload", 0o644)
            write_linux_tar(archive, files)
            MODULE.extract_archive(archive, output, LINUX_TARGET, VERSION)
            self.assertEqual((output / name).read_bytes(), files[name][0])

    def test_rejects_linux_manifest_mode_mismatch(self):
        with tempfile.TemporaryDirectory() as raw:
            temporary = Path(raw)
            archive = temporary / "mode-mismatch.tar.gz"
            output = temporary / "output"
            manifest = manifest_bytes(
                LINUX_TARGET,
                self.linux_files,
                mutate=lambda document: document["files"][0].__setitem__(
                    "mode", 0o644
                ),
            )
            write_linux_tar(
                archive, self.linux_files, manifest=manifest
            )
            with self.assertRaisesRegex(ValueError, "mode"):
                MODULE.extract_archive(archive, output, LINUX_TARGET, VERSION)
            self.assertFalse(output.exists())

    def test_preserves_preexisting_root_and_cleans_partial_root(self):
        with tempfile.TemporaryDirectory() as raw:
            temporary = Path(raw)
            archive = temporary / "kettle.zip"
            write_windows_zip(archive, self.windows_files)

            existing = temporary / "existing"
            existing.mkdir()
            marker = existing / "keep.txt"
            marker.write_text("keep", encoding="ascii")
            with self.assertRaisesRegex(ValueError, "must not already exist"):
                MODULE.extract_archive(
                    archive, existing, WINDOWS_TARGET, VERSION
                )
            self.assertEqual(marker.read_text(encoding="ascii"), "keep")

            partial = temporary / "partial"
            with mock.patch.object(
                MODULE, "_write_member", side_effect=OSError("injected failure")
            ):
                with self.assertRaisesRegex(OSError, "injected failure"):
                    MODULE.extract_archive(
                        archive, partial, WINDOWS_TARGET, VERSION
                    )
            self.assertFalse(partial.exists())

    def test_rejects_oversized_archive_before_writing(self):
        with tempfile.TemporaryDirectory() as raw:
            temporary = Path(raw)
            archive = temporary / "oversized.zip"
            output = temporary / "output"
            with archive.open("wb") as stream:
                stream.seek(MODULE.MAX_ARCHIVE_BYTES)
                stream.write(b"x")
            with self.assertRaisesRegex(ValueError, "256 MiB"):
                MODULE.extract_archive(
                    archive, output, WINDOWS_TARGET, VERSION
                )
            self.assertFalse(output.exists())


if __name__ == "__main__":
    unittest.main()
