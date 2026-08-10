#!/usr/bin/env python3
"""Hermetic security tests for the POSIX online installer."""

from __future__ import annotations

from io import BytesIO
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import platform
import shutil
import signal
import ssl
import subprocess
import tarfile
import tempfile
import threading
import time
import unittest
from urllib.parse import urlsplit


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
disable_config=0
if [ "${1-}" = "-q" ]; then
  disable_config=1
  shift
fi
if [ "${1-}" = "--help" ] && [ "${2-}" = "all" ]; then
  echo "     --max-filesize <bytes>"
  echo "     --retry-connrefused"
  exit 0
fi
output=
url=
retries=0
retry_delay=
retry_max_time=
retry_connrefused=0
proto=
proto_redir=
tls=0
max_redirs=
connect_timeout=
total_timeout=
low_speed_bytes=
low_speed_seconds=
max_filesize=
while [ "$#" -gt 0 ]; do
  case "$1" in
    -o)
      output=$2
      shift 2
      ;;
    --max-filesize)
      max_filesize=$2
      shift 2
      ;;
    --retry)
      retries=$2
      shift 2
      ;;
    --retry-delay)
      retry_delay=$2
      shift 2
      ;;
    --retry-max-time)
      retry_max_time=$2
      shift 2
      ;;
    --retry-connrefused)
      retry_connrefused=1
      shift
      ;;
    --proto)
      proto=$2
      shift 2
      ;;
    --proto-redir)
      proto_redir=$2
      shift 2
      ;;
    --tlsv1.2)
      tls=1
      shift
      ;;
    --max-redirs)
      max_redirs=$2
      shift 2
      ;;
    --connect-timeout)
      connect_timeout=$2
      shift 2
      ;;
    --max-time)
      total_timeout=$2
      shift 2
      ;;
    --speed-limit)
      low_speed_bytes=$2
      shift 2
      ;;
    --speed-time)
      low_speed_seconds=$2
      shift 2
      ;;
    *)
      url=$1
      shift
      ;;
  esac
done
[ -n "$output" ] || exit 2
printf 'config=%s max=%s retry=%s delay=%s retry-max=%s refused=%s proto=%s redir=%s tls=%s redirs=%s connect=%s total=%s low-bytes=%s low-seconds=%s\\n' \
  "$disable_config" "$max_filesize" "$retries" "$retry_delay" \
  "$retry_max_time" "$retry_connrefused" \
  "$proto" "$proto_redir" "$tls" "$max_redirs" "$connect_timeout" \
  "$total_timeout" "$low_speed_bytes" "$low_speed_seconds" \
  >> "${FIXTURE_CURL_LOG:?}"

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
    cp "${FIXTURE_ARCHIVE:?}" "$output"
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

    def _write_real_curl_proxy(self) -> None:
        """Route production curl arguments to the local TLS fixture.

        The first transport tests modeled curl's classifier inside the fake,
        which made them self-fulfilling: a fake that elects not to retry a 404
        cannot prove the real invocation would do the same. This proxy changes
        only the destination and CA; the installed curl parses and executes the
        exact retry, failure, timeout, and size-limit flags from the installer.
        """
        script = self.fake_bin / "curl"
        script.write_text(
            """#!/usr/bin/env python3
import os
import subprocess
import sys
from urllib.parse import urlsplit

args = sys.argv[1:]
real_curl = os.environ["FIXTURE_REAL_CURL"]
if "--help" in args:
    raise SystemExit(subprocess.run([real_curl, *args], check=False).returncode)

endpoint = os.environ["FIXTURE_HTTPS_ENDPOINT"]
original = urlsplit(args[-1])
local = urlsplit(endpoint)
args[-1] = endpoint + original.path
if original.query:
    args[-1] += "?" + original.query

# `-q` must remain curl's first argument or it does not suppress .curlrc.
insert_at = 1 if args and args[0] == "-q" else 0
if os.environ.get("FIXTURE_STRIP_MAX_FILESIZE") == "1":
    stripped = []
    index = 0
    while index < len(args):
        if args[index] == "--max-filesize":
            index += 2
        else:
            stripped.append(args[index])
            index += 1
    args = stripped
args[insert_at:insert_at] = [
    "--resolve",
    f"github.com:{local.port}:127.0.0.1",
    "--noproxy",
    "github.com",
    "--cacert",
    os.environ["FIXTURE_HTTPS_CERT"],
]
result = subprocess.run([real_curl, *args], check=False)
status_log = os.environ.get("FIXTURE_REAL_CURL_STATUS_LOG")
if status_log:
    with open(status_log, "a", encoding="ascii") as stream:
        stream.write(str(result.returncode) + "\\n")
raise SystemExit(result.returncode)
""",
            encoding="ascii",
            newline="\n",
        )
        script.chmod(0o755)

    def _run_with_real_curl(
        self,
        archive: Path,
        scenario: str,
        *,
        strip_max_filesize: bool = False,
    ) -> tuple[subprocess.CompletedProcess[str], dict[str, int], list[int]]:
        real_curl = shutil.which("curl")
        real_openssl = shutil.which("openssl")
        if real_curl is None or real_openssl is None:
            self.skipTest("real curl retry fixtures require curl and openssl")

        cert = self.root / "fixture-cert.pem"
        key = self.root / "fixture-key.pem"
        generated = subprocess.run(
            [
                real_openssl,
                "req",
                "-x509",
                "-newkey",
                "rsa:2048",
                "-sha256",
                "-nodes",
                "-keyout",
                str(key),
                "-out",
                str(cert),
                "-days",
                "1",
                "-subj",
                "/CN=github.com",
                "-addext",
                "subjectAltName=DNS:github.com",
            ],
            check=False,
            capture_output=True,
            text=True,
        )
        if generated.returncode != 0:
            self.skipTest(f"openssl cannot make the TLS fixture: {generated.stderr}")

        counts: dict[str, int] = {}
        archive_bytes = archive.read_bytes()
        test = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, _format: str, *_args: object) -> None:
                return

            def reply(
                self,
                status: int,
                body: bytes = b"",
                *,
                length: bool = True,
                headers: dict[str, str] | None = None,
            ) -> None:
                self.send_response(status)
                if length:
                    self.send_header("Content-Length", str(len(body)))
                for name, value in (headers or {}).items():
                    self.send_header(name, value)
                self.end_headers()
                try:
                    self.wfile.write(body)
                except (BrokenPipeError, ConnectionResetError, ssl.SSLError):
                    pass

            def do_GET(self) -> None:
                path = urlsplit(self.path).path
                counts[path] = counts.get(path, 0) + 1
                if path.endswith("/kettle-update-manifest.json"):
                    if scenario == "recover-manifest" and counts[path] <= 2:
                        self.reply(503)
                    elif scenario == "exhaust-manifest":
                        self.reply(503)
                    elif scenario == "long-retry-after":
                        self.reply(503, headers={"Retry-After": "60"})
                    elif scenario == "missing-manifest":
                        self.reply(404)
                    elif scenario == "oversize-manifest":
                        self.reply(200, b"x" * 131_073, length=False)
                    else:
                        self.reply(
                            200,
                            (test.root / "kettle-update-manifest.json").read_bytes(),
                        )
                elif path.endswith("/kettle-update-manifest.json.sig"):
                    self.reply(
                        200,
                        (test.root / "kettle-update-manifest.json.sig").read_bytes(),
                    )
                elif path.endswith(".tar.gz.sha256"):
                    self.reply(200, test.sidecar.read_bytes())
                elif path.endswith(".tar.gz"):
                    self.reply(200, archive_bytes)
                else:
                    self.reply(404)

        self._write_real_curl_proxy()
        server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        thread: threading.Thread | None = None
        status_log = self.root / "real-curl-status.log"
        try:
            context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
            context.load_cert_chain(cert, key)
            server.socket = context.wrap_socket(server.socket, server_side=True)
            thread = threading.Thread(target=server.serve_forever, daemon=True)
            thread.start()
            result = self._run(
                archive,
                version="v2.36.0",
                signed=True,
                extra_environment={
                    "FIXTURE_REAL_CURL": real_curl,
                    "FIXTURE_HTTPS_ENDPOINT": (
                        f"https://github.com:{server.server_port}"
                    ),
                    "FIXTURE_HTTPS_CERT": str(cert),
                    "FIXTURE_REAL_CURL_STATUS_LOG": str(status_log),
                    "FIXTURE_STRIP_MAX_FILESIZE": (
                        "1" if strip_max_filesize else "0"
                    ),
                    # If `-q` ever stops being curl's first argument, this makes
                    # permanent errors retry and the request-count tests fail.
                    "FIXTURE_HOSTILE_CURLRC": "1",
                },
            )
        finally:
            if thread is not None:
                server.shutdown()
            server.server_close()
            if thread is not None:
                thread.join(timeout=5)
        statuses = [
            int(line)
            for line in status_log.read_text(encoding="ascii").splitlines()
        ]
        return result, counts, statuses

    def _write_fake_openssl(self, *, verification_succeeds: bool = True) -> None:
        """Stand-in for openssl.

        `pkeyutl -verify` returns what `verification_succeeds` asks for. Every
        test used to get an unconditional success, which meant the signed path
        was exercised but the REFUSAL was not: removing the verification
        entirely, or accepting a bad signature, failed nothing. The signature
        is the only thing standing between a user and an attacker-supplied
        hash, so the failing case needs a test at least as much as the passing
        one.
        """
        verify_exit = "0" if verification_succeeds else "1"
        script = self.fake_bin / "openssl"
        # Plain string plus a substitution rather than an f-string: the body is
        # shell, and `${1-}` would collide with f-string interpolation.
        body = """#!/bin/sh
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
  exit @VERIFY_EXIT@
fi
exit 2
"""
        script.write_text(
            body.replace("@VERIFY_EXIT@", verify_exit),
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
        signature_verifies: bool = True,
        manifest_size: int | None = None,
    ) -> subprocess.CompletedProcess[str]:
        if sidecar is not None:
            self.sidecar.write_bytes(sidecar)
        prefix = self.root / "prefix"
        home = self.root / "home"
        home.mkdir(exist_ok=True)
        xdg_config = home / "xdg-config"
        xdg_config.mkdir(exist_ok=True)
        fixture_tmp = self.root / "tmp"
        fixture_tmp.mkdir(exist_ok=True)
        self.curl_log.unlink(missing_ok=True)
        manifest = self.root / "kettle-update-manifest.json"
        signature = self.root / "kettle-update-manifest.json.sig"
        if signed:
            self._write_fake_openssl(verification_succeeds=signature_verifies)
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
                "CURL_HOME": str(home),
                "XDG_CONFIG_HOME": str(xdg_config),
                "TMPDIR": str(fixture_tmp),
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
        if environment.get("FIXTURE_HOSTILE_CURLRC") == "1":
            (home / ".curlrc").write_text(
                "retry-all-errors\n",
                encoding="ascii",
                newline="\n",
            )
        command = ["sh", str(INSTALLER)]
        process = subprocess.Popen(
            command,
            cwd=ROOT,
            env=environment,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            start_new_session=True,
        )
        try:
            stdout, stderr = process.communicate(timeout=30)
        except subprocess.TimeoutExpired:
            # curl is a grandchild behind the POSIX installer shell. Killing
            # only `sh` leaves the proxy, curl and TLS request alive; own a
            # process group so a failed fixture cannot leak any of them.
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.communicate()
            raise
        return subprocess.CompletedProcess(
            command,
            process.returncode,
            stdout,
            stderr,
        )

    def test_safe_checksum_only_archive_installs_after_bounded_checksum(self):
        result = self._run(self._archive())
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("same-origin checksum only", result.stdout)
        self.assertTrue((self.root / "prefix" / "bin" / "kettle").is_file())
        calls = self.curl_log.read_text(encoding="ascii")
        common = (
            "config=1",
            "retry=2",
            "delay=0",
            "retry-max=30",
            "refused=1",
            "proto==https",
            "redir==https",
            "tls=1",
            "redirs=5",
            "connect=15",
            "total=600",
            "low-bytes=1024",
            "low-seconds=30",
        )
        for call in calls.splitlines():
            with self.subTest(call=call):
                for field in common:
                    self.assertIn(field, call)
        self.assertIn("max=268435456", calls)
        self.assertIn("max=1024", calls)

    def test_real_curl_retries_a_transient_manifest_then_installs(self):
        result, counts, _statuses = self._run_with_real_curl(
            self._archive(include_manifest=True),
            "recover-manifest",
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(
            sum(
                count
                for path, count in counts.items()
                if path.endswith("/kettle-update-manifest.json")
            ),
            3,
        )
        self.assertTrue((self.root / "prefix" / "bin" / "kettle").is_file())

    def test_real_curl_exhausts_the_manifest_attempt_bound(self):
        result, counts, _statuses = self._run_with_real_curl(
            self._archive(include_manifest=True),
            "exhaust-manifest",
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(
            sum(
                count
                for path, count in counts.items()
                if path.endswith("/kettle-update-manifest.json")
            ),
            3,
        )
        self.assertIn("must ship a bounded Ed25519-signed manifest", result.stderr)
        self.assertFalse((self.root / "prefix").exists())

    def test_retry_after_cannot_choose_an_unbounded_wait(self):
        started = time.monotonic()
        result, counts, _statuses = self._run_with_real_curl(
            self._archive(include_manifest=True),
            "long-retry-after",
        )
        elapsed = time.monotonic() - started
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(
            sum(
                count
                for path, count in counts.items()
                if path.endswith("/kettle-update-manifest.json")
            ),
            1,
            "a retry beyond the 30-second admission timer must not start",
        )
        self.assertLess(elapsed, 10, "curl waited for the server's 60-second delay")

    def test_real_curl_does_not_retry_a_missing_manifest(self):
        result, counts, _statuses = self._run_with_real_curl(
            self._archive(include_manifest=True),
            "missing-manifest",
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(
            sum(
                count
                for path, count in counts.items()
                if path.endswith("/kettle-update-manifest.json")
            ),
            1,
            "404 must stay permanent even with retry-all-errors in .curlrc",
        )
        self.assertIn("must ship a bounded Ed25519-signed manifest", result.stderr)
        self.assertFalse((self.root / "prefix").exists())

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

    def test_a_manifest_that_fails_ed25519_verification_is_refused(self):
        """The signature must be load-bearing, not merely consulted.

        Every other signed-path test ran against a stub whose
        `pkeyutl -verify` always succeeded, so the whole verification block
        could have been deleted — or made to accept a forged signature — and
        nothing would have gone red. This is the case that gives the check its
        meaning: the manifest is where the archive's hash comes from, so a
        manifest kettle cannot authenticate must not be trusted for one.
        """
        result = self._run(
            self._archive(include_manifest=True),
            version="v2.36.0",
            signed=True,
            signature_verifies=False,
        )
        self.assertNotEqual(
            result.returncode, 0, "an unverifiable manifest must not install"
        )
        self.assertIn("FAILED Ed25519 verification", result.stderr)
        self.assertIn(
            "Refusing to trust a hash from an unauthenticated manifest",
            result.stderr,
        )
        # Fail CLOSED: no silent downgrade to the unsigned path, and nothing
        # installed. A fallback here would make the signature decorative.
        self.assertNotIn("falling back", result.stderr)
        self.assertFalse((self.root / "prefix" / "bin" / "kettle").exists())

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
        result, counts, statuses = self._run_with_real_curl(
            self._archive(include_manifest=True),
            "oversize-manifest",
            strip_max_filesize=True,
        )
        self.assertIn(
            -signal.SIGXFSZ,
            statuses,
            "with curl's userspace limit removed, RLIMIT_FSIZE must stop the body",
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(
            sum(
                count
                for path, count in counts.items()
                if path.endswith("/kettle-update-manifest.json")
            ),
            1,
            "the kernel size-limit failure must not become retryable",
        )
        self.assertIn("must ship a bounded Ed25519-signed manifest", result.stderr)
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
