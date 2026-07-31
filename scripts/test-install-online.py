#!/usr/bin/env python3
"""Hermetic security tests for the POSIX online installer."""

from __future__ import annotations

from io import BytesIO
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import tarfile
import tempfile
import unittest


ROOT = Path(__file__).resolve().parent.parent
INSTALLER = ROOT / "scripts" / "install-online.sh"


def deterministic_filler(size: int = 4096) -> bytes:
    output = bytearray()
    counter = 0
    while len(output) < size:
        output.extend(hashlib.sha256(f"kettle-fixture-{counter}".encode()).digest())
        counter += 1
    return bytes(output[:size])


class OnlineInstallerTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        if os.name != "posix":
            raise unittest.SkipTest("the online installer is Linux/POSIX-only")
        tar_version = subprocess.run(
            ["tar", "--version"],
            check=False,
            capture_output=True,
            text=True,
        )
        if tar_version.returncode != 0 or "GNU tar" not in tar_version.stdout:
            raise unittest.SkipTest("the hardened installer requires GNU tar")
        machine = platform.machine().lower()
        if machine in {"x86_64", "amd64"}:
            cls.asset = "kettle-linux-x86_64.tar.gz"
            cls.target = "x86_64-unknown-linux-gnu"
        elif machine in {"aarch64", "arm64"}:
            cls.asset = "kettle-linux-aarch64.tar.gz"
            cls.target = "aarch64-unknown-linux-gnu"
        else:
            raise unittest.SkipTest(f"no online installer asset for {machine}")

    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.fake_bin = self.root / "bin"
        self.fake_bin.mkdir()
        self.curl_log = self.root / "curl.log"
        self.sidecar = self.root / f"{self.asset}.sha256"
        self._write_fake_curl()

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def _write_fake_curl(self) -> None:
        script = self.fake_bin / "curl"
        script.write_text(
            """#!/bin/sh
set -eu
if [ "${1-}" = "--help" ] && [ "${2-}" = "all" ]; then
  echo "     --max-filesize <bytes>"
  exit 0
fi
printf '%s\\n' "$*" >> "${FIXTURE_CURL_LOG:?}"
output=
url=
while [ "$#" -gt 0 ]; do
  case "$1" in
    -o)
      output=$2
      shift 2
      ;;
    --max-filesize)
      shift 2
      ;;
    *)
      url=$1
      shift
      ;;
  esac
done
[ -n "$output" ] || exit 2
case "$url" in
  *.tar.gz.sha256)
    cp "${FIXTURE_SIDECAR:?}" "$output"
    ;;
  *kettle-update-manifest.json.sig)
    cp "${FIXTURE_MANIFEST_SIGNATURE:?}" "$output"
    ;;
  *kettle-update-manifest.json)
    cp "${FIXTURE_MANIFEST:?}" "$output"
    ;;
  *.tar.gz)
    if [ "${FIXTURE_OVERSIZE:-0}" = 1 ]; then
      truncate -s 268435457 "$output"
    else
      cp "${FIXTURE_ARCHIVE:?}" "$output"
    fi
    ;;
  *)
    exit 22
    ;;
esac
""",
            encoding="ascii",
            newline="\n",
        )
        script.chmod(0o755)

    def _write_fake_openssl(self) -> None:
        script = self.fake_bin / "openssl"
        script.write_text(
            """#!/bin/sh
set -eu
if [ "$*" = "pkeyutl -verify -help" ]; then
  echo "Usage: pkeyutl -verify -rawin"
  exit 0
fi
if [ "${1-}" = "base64" ]; then
  output=
  while [ "$#" -gt 0 ]; do
    if [ "$1" = "-out" ]; then
      output=$2
      break
    fi
    shift
  done
  [ -n "$output" ]
  dd if=/dev/zero of="$output" bs=64 count=1 2>/dev/null
  exit 0
fi
if [ "${1-}" = "pkeyutl" ] && [ "${2-}" = "-verify" ]; then
  exit 0
fi
exit 2
""",
            encoding="ascii",
            newline="\n",
        )
        script.chmod(0o755)

    @staticmethod
    def _add(
        archive: tarfile.TarFile,
        name: str,
        *,
        data: bytes | None = None,
        mode: int = 0o644,
        kind: bytes = tarfile.REGTYPE,
        linkname: str = "",
    ) -> None:
        entry = tarfile.TarInfo(name)
        entry.uid = 0
        entry.gid = 0
        entry.uname = "root"
        entry.gname = "root"
        entry.mtime = 0
        entry.mode = mode
        entry.type = kind
        entry.linkname = linkname
        if kind == tarfile.DIRTYPE:
            entry.size = 0
            archive.addfile(entry)
            return
        payload = data or b""
        entry.size = len(payload)
        archive.addfile(entry, BytesIO(payload))

    def _archive(
        self,
        variant: str = "safe",
        *,
        include_manifest: bool = False,
        include_helper: bool = True,
    ) -> Path:
        archive_path = self.root / self.asset
        binary_mode = 0o4755 if variant == "setuid" else 0o755
        filler_mode = 0o666 if variant == "world-writable" else 0o644
        binary = b"""#!/bin/sh
if [ "${1-}" = "--version" ]; then
  echo "kettle 1.3.4"
fi
"""
        install = (ROOT / "scripts" / "install.sh").read_bytes()
        helper = (ROOT / "scripts" / "install-unix.py").read_bytes()
        with tarfile.open(
            archive_path,
            "w:gz",
            format=tarfile.GNU_FORMAT,
        ) as archive:
            self._add(archive, "kettle/", mode=0o755, kind=tarfile.DIRTYPE)
            self._add(
                archive,
                "kettle/kettle",
                data=binary,
                mode=binary_mode,
            )
            self._add(
                archive,
                "kettle/install.sh",
                data=install,
                mode=0o755,
            )
            if include_helper:
                self._add(
                    archive,
                    "kettle/install-unix.py",
                    data=helper,
                    mode=0o755,
                )
            self._add(
                archive,
                "kettle/packaging/",
                mode=0o755,
                kind=tarfile.DIRTYPE,
            )
            self._add(
                archive,
                "kettle/packaging/linux/",
                mode=0o755,
                kind=tarfile.DIRTYPE,
            )
            self._add(
                archive,
                "kettle/shell-integration/",
                mode=0o755,
                kind=tarfile.DIRTYPE,
            )
            for relative_root in ("packaging/linux", "shell-integration"):
                for source in sorted((ROOT / relative_root).iterdir()):
                    if source.is_file():
                        self._add(
                            archive,
                            f"kettle/{relative_root}/{source.name}",
                            data=source.read_bytes(),
                        )
            if include_manifest:
                self._add(
                    archive,
                    "kettle/kettle-package-manifest.json",
                    data=b"{}\n",
                )
            self._add(
                archive,
                "kettle/fixture.bin",
                data=deterministic_filler(),
                mode=filler_mode,
            )
            if variant == "symlink":
                self._add(
                    archive,
                    "kettle/link",
                    kind=tarfile.SYMTYPE,
                    linkname="/tmp/kettle-escape",
                )
            elif variant == "hardlink":
                self._add(
                    archive,
                    "kettle/hard",
                    kind=tarfile.LNKTYPE,
                    linkname="kettle/kettle",
                )
            elif variant == "traversal":
                self._add(archive, "kettle/../escape", data=b"escape")
            elif variant == "absolute":
                self._add(archive, "/tmp/kettle-escape", data=b"escape")
            elif variant == "case-alias":
                self._add(archive, "kettle/KETTLE", data=b"alias", mode=0o755)
            elif variant == "space":
                self._add(archive, "kettle/bad name", data=b"space")
            elif variant == "too-many":
                for index in range(125):
                    self._add(
                        archive,
                        f"kettle/extra-{index:03d}",
                        data=b"x",
                    )
        digest = hashlib.sha256(archive_path.read_bytes()).hexdigest()
        self.sidecar.write_text(
            f"{digest}  {self.asset}\n",
            encoding="ascii",
            newline="\n",
        )
        return archive_path

    def _run(
        self,
        archive: Path,
        *,
        version: str = "v1.3.4",
        sidecar: bytes | None = None,
        extra_environment: dict[str, str] | None = None,
        signed: bool = False,
        manifest_size: int | None = None,
    ) -> subprocess.CompletedProcess[str]:
        if sidecar is not None:
            self.sidecar.write_bytes(sidecar)
        prefix = self.root / "prefix"
        home = self.root / "home"
        home.mkdir(exist_ok=True)
        self.curl_log.unlink(missing_ok=True)
        manifest = self.root / "kettle-update-manifest.json"
        signature = self.root / "kettle-update-manifest.json.sig"
        if signed:
            self._write_fake_openssl()
            archive_size = archive.stat().st_size
            document = {
                "schema": 1,
                "product": "kettle",
                "channel": "stable",
                "version": version[1:],
                "tag": version,
                "published_at": "2026-07-26T00:00:00+00:00",
                "assets": [
                    {
                        "target": self.target,
                        "name": self.asset,
                        "size": (
                            archive_size
                            if manifest_size is None
                            else manifest_size
                        ),
                        "sha256": hashlib.sha256(archive.read_bytes()).hexdigest(),
                    }
                ],
            }
            manifest.write_text(
                json.dumps(
                    document,
                    sort_keys=True,
                    separators=(",", ":"),
                    ensure_ascii=True,
                )
                + "\n",
                encoding="ascii",
                newline="\n",
            )
            signature.write_text("A" * 88 + "\n", encoding="ascii", newline="\n")
        environment = os.environ.copy()
        environment.update(
            {
                "PATH": f"{self.fake_bin}{os.pathsep}{environment['PATH']}",
                "HOME": str(home),
                "KETTLE_PREFIX": str(prefix),
                "KETTLE_VERSION": version,
                "FIXTURE_ARCHIVE": str(archive),
                "FIXTURE_SIDECAR": str(self.sidecar),
                "FIXTURE_CURL_LOG": str(self.curl_log),
                "FIXTURE_MANIFEST": str(manifest),
                "FIXTURE_MANIFEST_SIGNATURE": str(signature),
            }
        )
        if extra_environment:
            environment.update(extra_environment)
        return subprocess.run(
            ["sh", str(INSTALLER)],
            cwd=ROOT,
            env=environment,
            check=False,
            capture_output=True,
            text=True,
            timeout=30,
        )

    def test_safe_checksum_only_archive_installs_after_bounded_checksum(self):
        result = self._run(self._archive())
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("same-origin checksum only", result.stdout)
        self.assertTrue((self.root / "prefix" / "bin" / "kettle").is_file())
        calls = self.curl_log.read_text(encoding="ascii")
        self.assertIn("--proto =https", calls)
        self.assertIn("--proto-redir =https", calls)
        self.assertIn("--tlsv1.2", calls)
        self.assertIn("--max-redirs 5", calls)
        self.assertIn("--connect-timeout 15", calls)
        self.assertIn("--max-time 600", calls)
        self.assertIn("--speed-limit 1024", calls)
        self.assertIn("--speed-time 30", calls)
        self.assertIn("--max-filesize 268435456", calls)
        self.assertIn("--max-filesize 1024", calls)

    def test_authenticated_archive_without_hardened_helper_is_refused(self):
        result = self._run(self._archive(include_helper=False))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(
            "release lacks the hardened install-unix.py helper",
            result.stderr,
        )
        self.assertFalse((self.root / "prefix").exists())

    def test_safe_modern_archive_uses_signed_manifest_and_inner_manifest(self):
        result = self._run(
            self._archive(include_manifest=True),
            version="v2.36.0",
            signed=True,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("Ed25519-signed manifest", result.stdout)
        self.assertNotIn("falling back", result.stderr)
        self.assertTrue((self.root / "prefix" / "bin" / "kettle").is_file())

    def test_modern_archive_requires_the_inner_package_manifest(self):
        result = self._run(
            self._archive(),
            version="v2.36.0",
            signed=True,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(
            "authenticated archive failed the bounded structural preflight",
            result.stderr,
        )
        self.assertFalse((self.root / "prefix").exists())

    def test_signed_manifest_rejects_an_unbounded_decimal_size(self):
        result = self._run(
            self._archive(include_manifest=True),
            version="v2.36.0",
            signed=True,
            manifest_size=10**30,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("signed manifest is non-canonical", result.stderr)
        self.assertFalse((self.root / "prefix").exists())

    def test_kernel_file_limit_stops_unknown_length_oversize_response(self):
        result = self._run(
            self._archive(),
            extra_environment={"FIXTURE_OVERSIZE": "1"},
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("download failed", result.stderr)
        self.assertFalse((self.root / "prefix").exists())

    def test_modern_release_cannot_downgrade_when_manifest_is_suppressed(self):
        result = self._run(self._archive(), version="v2.35.0")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(
            "Refusing to downgrade to the weaker same-origin checksum",
            result.stderr,
        )
        self.assertFalse((self.root / "prefix").exists())

    def test_noncanonical_sidecar_is_rejected(self):
        digest = hashlib.sha256(b"wrong").hexdigest().upper()
        result = self._run(
            self._archive(),
            sidecar=f"{digest}  {self.asset}\nextra\n".encode("ascii"),
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("is not one exact lowercase SHA-256 record", result.stderr)
        self.assertFalse((self.root / "prefix").exists())

    def test_unsafe_archives_fail_before_install(self):
        for variant in (
            "symlink",
            "hardlink",
            "traversal",
            "absolute",
            "case-alias",
            "space",
            "setuid",
            "world-writable",
            "too-many",
        ):
            with self.subTest(variant=variant):
                result = self._run(self._archive(variant))
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(
                    "authenticated archive failed the bounded structural preflight",
                    result.stderr,
                )
                shutil.rmtree(self.root / "prefix", ignore_errors=True)

    def test_security_parsing_forces_the_c_locale(self):
        result = self._run(
            self._archive("case-alias"),
            extra_environment={
                "LC_ALL": "tr_TR.UTF-8",
                "LANG": "tr_TR.UTF-8",
            },
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(
            "authenticated archive failed the bounded structural preflight",
            result.stderr,
        )
        self.assertFalse((self.root / "prefix").exists())


if __name__ == "__main__":
    unittest.main()
