#!/usr/bin/env python3
"""Hermetic tests for make-update-manifest.py."""

from __future__ import annotations

import base64
import hashlib
import importlib.util
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import unittest


sys.dont_write_bytecode = True
SCRIPT = Path(__file__).with_name("make-update-manifest.py")
ROOT = SCRIPT.parent.parent
SPEC = importlib.util.spec_from_file_location("make_update_manifest", SCRIPT)
assert SPEC and SPEC.loader
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)

# A workflow shaped like release.yml, with one extra step between the two
# the changelog test reads. Slicing from one step heading to another
# reports that extra step's script as part of the Linux step.
SPLIT_TRAP_WORKFLOW = """jobs:
  package:
    steps:
      - name: Package (Linux)
        run: |
          mkdir -p dist/kettle/packaging/linux
          cp LICENSE NOTICE README.md CHANGELOG.md dist/kettle/
      - name: Stage extra documentation
        run: |
          mkdir -p dist/kettle/docs
          cp -R docs/changelog dist/kettle/docs/
      - name: Package (macOS .app bundle)
        run: |
          mkdir -p "$APP/Contents/Resources/docs"
          cp -R docs/changelog "$APP/Contents/Resources/docs/"
      - name: Sign and notarize (macOS)
        run: codesign --sign "$SIGNING_IDENTITY" "$APP"
"""

STEP_HEADING = re.compile(r"^(?P<indent> *)- name: (?P<name>.*?) *$")


def workflow_step(workflow: str, name: str) -> str:
    """Return the workflow step named `name`, heading line included.

    Bounding a step with the name of the step that follows it ties the
    slice to an unrelated heading: insert a step between the two and the
    slice quietly grows to cover it, so assertions about the first step
    pass on the inserted step's script. Give the following step a new
    name and the slice runs to the end of the file instead, still
    silently. This locates the step by its own heading, requires that
    heading to appear exactly once, and ends the step where its block
    ends -- at the first line indented no deeper than the heading's `-`.
    """
    lines = workflow.splitlines(keepends=True)
    headings = [
        (index, match)
        for index, line in enumerate(lines)
        if (match := STEP_HEADING.match(line)) and match.group("name") == name
    ]
    if len(headings) != 1:
        raise LookupError(
            f"{len(headings)} workflow steps are named {name!r}, expected 1"
        )
    start, heading = headings[0]
    indent = len(heading.group("indent"))
    for end in range(start + 1, len(lines)):
        body = lines[end].lstrip(" ")
        if not body.strip():
            continue
        if len(lines[end]) - len(body) <= indent:
            return "".join(lines[start:end])
    return "".join(lines[start:])


class ManifestTests(unittest.TestCase):
    def assets(self, root: Path):
        result = []
        for target, name in MODULE.EXPECTED_NAMES.items():
            path = root / name
            path.write_bytes((target + "\n").encode("ascii"))
            result.append((target, path))
        return result

    def test_manifest_is_complete_deterministic_and_hashed(self):
        with tempfile.TemporaryDirectory() as raw:
            assets = self.assets(Path(raw))
            first = MODULE.build_manifest("v4.0.0", "2026-07-11T00:00:00Z", assets)
            second = MODULE.build_manifest(
                "v4.0.0", "2026-07-11T00:00:00Z", list(reversed(assets))
            )
            self.assertEqual(first, second)
            self.assertEqual(first["version"], "4.0.0")
            self.assertEqual(len(first["assets"]), 3)
            self.assertTrue(all(len(asset["sha256"]) == 64 for asset in first["assets"]))

    def test_existing_signed_manifest_binds_exact_local_artifacts(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            assets = self.assets(root)
            manifest = MODULE.build_manifest(
                "v4.1.0",
                "2026-07-26T10:00:00-07:00",
                assets,
            )
            path = root / "kettle-update-manifest.json"
            canonical = MODULE.encode_manifest(manifest)
            path.write_bytes(canonical)
            MODULE.verify_manifest(path, "v4.1.0", assets)

            path.write_bytes(canonical[:-1] + b" \n")
            with self.assertRaisesRegex(ValueError, "exactly bind"):
                MODULE.verify_manifest(path, "v4.1.0", assets)

            path.write_bytes(canonical)
            assets[0][1].write_bytes(b"substituted")
            with self.assertRaisesRegex(ValueError, "exactly bind"):
                MODULE.verify_manifest(path, "v4.1.0", assets)

    def test_rejects_prerelease_missing_and_misnamed_assets(self):
        with tempfile.TemporaryDirectory() as raw:
            assets = self.assets(Path(raw))
            with self.assertRaises(ValueError):
                MODULE.build_manifest("v4.0.0-rc.1", "now", assets)
            with self.assertRaises(ValueError):
                MODULE.build_manifest("v4.0.0", "now", assets[:-1])
            wrong = list(assets)
            wrong[0] = (wrong[0][0], Path(raw) / "wrong.zip")
            wrong[0][1].write_bytes(b"wrong")
            with self.assertRaises(ValueError):
                MODULE.build_manifest("v4.0.0", "now", wrong)

    def test_three_target_manifest_rejects_pre_retirement_releases(self):
        with tempfile.TemporaryDirectory() as raw:
            assets = self.assets(Path(raw))
            with self.assertRaisesRegex(ValueError, "v4.0.0 and later"):
                MODULE.build_manifest("v3.3.0", "now", assets)

    def test_release_generator_matches_shipped_client_artifact_limit(self):
        feed_source = (
            ROOT / "crates" / "kettle-update" / "src" / "feed.rs"
        ).read_text(encoding="utf-8")
        rust_match = re.search(
            r"const MAX_ARTIFACT_BYTES: u64 = ([0-9]+) \* 1024 \* 1024;",
            feed_source,
        )
        self.assertIsNotNone(rust_match)
        rust_limit = int(rust_match.group(1)) * 1024 * 1024
        self.assertEqual(MODULE.MAX_ARTIFACT_BYTES, rust_limit)

        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            assets = self.assets(root)
            oversized = root / MODULE.EXPECTED_NAMES["aarch64-unknown-linux-gnu"]
            with oversized.open("wb") as stream:
                stream.truncate(MODULE.MAX_ARTIFACT_BYTES + 1)
            with self.assertRaisesRegex(ValueError, "outside the accepted range"):
                MODULE.build_manifest("v4.0.0", "now", assets)

    def test_production_trust_root_is_locked_across_release_consumers(self):
        trusted_pem = (ROOT / "packaging" / "update-public.pem").read_text(
            encoding="ascii"
        )
        self.assertTrue(trusted_pem.endswith("\n"))
        pem_lines = trusted_pem.strip().splitlines()
        self.assertEqual(pem_lines[0], "-----BEGIN PUBLIC KEY-----")
        self.assertEqual(pem_lines[-1], "-----END PUBLIC KEY-----")
        public_der = base64.b64decode("".join(pem_lines[1:-1]), validate=True)
        ed25519_spki_prefix = bytes.fromhex("302a300506032b6570032100")
        self.assertTrue(public_der.startswith(ed25519_spki_prefix))
        public_key = public_der[len(ed25519_spki_prefix) :]
        self.assertEqual(len(public_key), 32)
        self.assertEqual(
            hashlib.sha256(public_key).hexdigest(),
            "e8e73619a959b34c24fa255714719a61c9cee810340bf041497c39475ab2dbb7",
        )

        rust_source = (
            ROOT / "crates" / "kettle-update" / "src" / "lib.rs"
        ).read_text(encoding="utf-8")
        rust_match = re.search(
            r"pub const UPDATE_PUBLIC_KEY: \[u8; 32\] = \[(.*?)\];",
            rust_source,
            flags=re.DOTALL,
        )
        self.assertIsNotNone(rust_match)
        rust_key = bytes(
            int(value, 16)
            for value in re.findall(r"0x([0-9a-fA-F]{2})", rust_match.group(1))
        )
        self.assertEqual(rust_key, public_key)

        installer = (ROOT / "scripts" / "install-online.sh").read_text(
            encoding="utf-8"
        )
        installer_match = re.search(
            r"MANIFEST_PUBLIC_KEY_PEM='(-----BEGIN PUBLIC KEY-----\n"
            r".*?\n-----END PUBLIC KEY-----)'",
            installer,
            flags=re.DOTALL,
        )
        self.assertIsNotNone(installer_match)
        self.assertEqual(installer_match.group(1), trusted_pem.strip())

        release_workflow = (
            ROOT / ".github" / "workflows" / "release.yml"
        ).read_text(encoding="utf-8")
        self.assertIn(
            "openssl pkey -pubin -in packaging/update-public.pem",
            release_workflow,
        )
        self.assertIn(
            "openssl pkeyutl -verify -rawin -pubin "
            "-inkey packaging/update-public.pem",
            release_workflow,
        )

    def test_release_finalizer_uses_the_bounded_extractor_and_remote_digests(self):
        release_workflow = (
            ROOT / ".github" / "workflows" / "release.yml"
        ).read_text(encoding="utf-8")
        self.assertEqual(
            release_workflow.count("scripts/package-manifest.py extract"),
            4,
        )
        for archive, target in (
            (
                "dist/kettle-linux-x86_64.tar.gz",
                "x86_64-unknown-linux-gnu",
            ),
            (
                "dist/kettle-linux-aarch64.tar.gz",
                "aarch64-unknown-linux-gnu",
            ),
        ):
            self.assertEqual(release_workflow.count(f"--archive {archive}"), 2)
            self.assertGreaterEqual(release_workflow.count(f"--target {target}"), 2)
        self.assertNotIn("tar -xzf dist/kettle-", release_workflow)
        self.assertNotIn("unzip -q dist/kettle-", release_workflow)
        self.assertIn(
            "python3 scripts/verify-release-assets.py",
            release_workflow,
        )
        self.assertIn(
            "--verify-existing dist/kettle-update-manifest.json",
            release_workflow,
        )
        self.assertGreaterEqual(release_workflow.count("cmp -s"), 4)
        self.assertEqual(
            release_workflow.count("persist-credentials: false"),
            4,
        )
        finalize = release_workflow.split("\n  finalize:\n", 1)[1].split(
            "\n  publish:\n", 1
        )[0]
        publish = release_workflow.split("\n  publish:\n", 1)[1]
        self.assertIn("environment: release-signing", finalize)
        self.assertIn("contents: read", finalize)
        self.assertNotIn("contents: write", finalize)
        self.assertNotIn("GH_TOKEN:", finalize)
        self.assertIn("contents: write", publish)
        self.assertIn("GH_TOKEN:", publish)
        self.assertNotIn("KETTLE_UPDATE_SIGNING_KEY_PEM", publish)
        package = release_workflow.split("\n  package:\n", 1)[1].split(
            "\n  finalize:\n", 1
        )[0]
        self.assertIn("environment: ${{ matrix.environment }}", package)
        self.assertEqual(package.count("environment: release-build"), 2)
        self.assertEqual(package.count("environment: macos-signing"), 1)
        self.assertIn("printf '%s' \"$APPLE_CERT_P12\" | base64 -D", package)
        self.assertIn('-k "$KEYCHAIN_PASSWORD" "$KEYCHAIN"', package)
        self.assertNotIn("APPLE_SIGNING_IDENTITY", package)
        self.assertIn("scripts/select-macos-signing-identity.py", package)
        self.assertIn(
            "scripts/configure-macos-signing-keychain.swift add", package
        )
        self.assertIn(
            "scripts/configure-macos-signing-keychain.swift remove", package
        )
        self.assertIn('--sign "$SIGNING_IDENTITY"', package)
        self.assertIn('SIGNED_TEAM=$(\n', package)
        self.assertIn('!= \"$APPLE_TEAM_ID\"', package)
        final_macos_archive = (
            "ditto -c -k --keepParent dist/kettle.app "
            "${{ matrix.artifact }}"
        )
        self.assertIn(final_macos_archive, package)
        self.assertNotIn("zip -r", package)
        self.assertLess(
            package.index('xcrun stapler staple "$APP"'),
            package.index(final_macos_archive),
        )
        self.assertNotIn("runs-on: ubuntu-latest", release_workflow)
        self.assertNotIn("runs-on: macos-latest", release_workflow)
        self.assertNotIn("runs-on: windows-latest", release_workflow)
        self.assertNotIn("toolchain: stable", release_workflow)
        for cargo_line in re.findall(
            r"^\s*cargo (?:build|test)\b.*$",
            release_workflow,
            flags=re.MULTILINE,
        ):
            self.assertIn("--locked", cargo_line)

    def test_workflow_step_ends_where_its_own_block_ends(self):
        # A step read as "everything between my heading and the next
        # heading I happen to name" absorbs whatever is inserted between
        # the two, so an assertion about the first step passes on text
        # that belongs to a later one. Here the changelog copy lives in
        # the step after the Linux one, and must not be found in it.
        linux = workflow_step(SPLIT_TRAP_WORKFLOW, "Package (Linux)")

        self.assertIn("mkdir -p dist/kettle/packaging/linux", linux)
        self.assertNotIn("cp -R docs/changelog dist/kettle/docs/", linux)
        self.assertNotIn("Stage extra documentation", linux)
        self.assertNotIn("Package (macOS .app bundle)", linux)

    def test_workflow_step_keeps_the_whole_step_it_was_asked_for(self):
        macos = workflow_step(SPLIT_TRAP_WORKFLOW, "Package (macOS .app bundle)")

        self.assertIn('mkdir -p "$APP/Contents/Resources/docs"', macos)
        self.assertIn(
            'cp -R docs/changelog "$APP/Contents/Resources/docs/"', macos
        )
        self.assertNotIn("Sign and notarize (macOS)", macos)

    def test_workflow_step_refuses_a_name_it_cannot_place_exactly_once(self):
        # Slicing on a missing name returns the rest of the file instead
        # of failing, which is the silent half of the same defect.
        with self.assertRaises(LookupError):
            workflow_step(SPLIT_TRAP_WORKFLOW, "Package (Windows)")
        with self.assertRaises(LookupError):
            workflow_step(
                SPLIT_TRAP_WORKFLOW + SPLIT_TRAP_WORKFLOW, "Package (Linux)"
            )

    def test_release_packages_complete_changelog_history(self):
        release_workflow = (
            ROOT / ".github" / "workflows" / "release.yml"
        ).read_text(encoding="utf-8")
        linux_package = workflow_step(release_workflow, "Package (Linux)")
        macos_package = workflow_step(
            release_workflow, "Package (macOS .app bundle)"
        )

        self.assertIn("mkdir -p dist/kettle/docs", linux_package)
        self.assertIn("cp -R docs/changelog dist/kettle/docs/", linux_package)
        self.assertIn(
            'mkdir -p "$APP/Contents/Resources/docs"', macos_package
        )
        self.assertIn(
            'cp -R docs/changelog "$APP/Contents/Resources/docs/"',
            macos_package,
        )
        # Each step carries its own copy; neither assertion above may be
        # satisfied by the other step's text.
        self.assertNotIn("$APP/Contents/Resources", linux_package)
        self.assertNotIn("dist/kettle/docs", macos_package)

    def test_macos_signing_selector_requires_one_distribution_identity(self):
        selector = ROOT / "scripts" / "select-macos-signing-identity.py"
        first = "A" * 40
        second = "b" * 40

        def select(output: str):
            return subprocess.run(
                [sys.executable, str(selector)],
                input=output,
                text=True,
                capture_output=True,
                check=False,
            )

        selected = select(
            f'  1) {first} "Apple Development: Test (TEAMID)"\n'
            f'  2) {second} "Developer ID Application: Test (TEAMID)"\n'
            "     2 valid identities found\n"
        )
        self.assertEqual(selected.returncode, 0, selected.stderr)
        self.assertEqual(selected.stdout, f"{second}\n")

        missing = select(f'1) {first} "Apple Development: Test (TEAMID)"\n')
        self.assertNotEqual(missing.returncode, 0)
        self.assertIn("found 0", missing.stderr)

        ambiguous = select(
            f'1) {first} "Developer ID Application: First (TEAMID)"\n'
            f'2) {second} "Developer ID Application: Second (TEAMID)"\n'
        )
        self.assertNotEqual(ambiguous.returncode, 0)
        self.assertIn("found 2", ambiguous.stderr)

    def test_macos_signing_keychain_search_list_is_reversible(self):
        if sys.platform != "darwin":
            self.skipTest("the search-list helper uses macOS Security.framework")

        helper = ROOT / "scripts" / "configure-macos-signing-keychain.swift"
        with tempfile.TemporaryDirectory() as directory:
            original = subprocess.run(
                [
                    "xcrun",
                    "swift",
                    "-suppress-warnings",
                    str(helper),
                    "list-json",
                ],
                check=True,
                capture_output=True,
                text=True,
            ).stdout
            command = [
                "xcrun",
                "swift",
                "-suppress-warnings",
                str(helper),
                "self-test",
                directory,
            ]
            subprocess.run(
                command,
                check=True,
                capture_output=True,
                text=True,
            )
            after = subprocess.run(
                [
                    "xcrun",
                    "swift",
                    "-suppress-warnings",
                    str(helper),
                    "list-json",
                ],
                check=True,
                capture_output=True,
                text=True,
            ).stdout
            self.assertEqual(after, original)

    def test_online_installer_shares_the_signed_channel_bounds(self):
        installer = (ROOT / "scripts" / "install-online.sh").read_text(
            encoding="utf-8"
        )
        constants = dict(
            re.findall(
                r"^((?:MAX|CURL)_[A-Z_]+)=([0-9]+)$",
                installer,
                flags=re.MULTILINE,
            )
        )
        self.assertEqual(
            int(constants["MAX_ARCHIVE_BYTES"]),
            MODULE.MAX_ARTIFACT_BYTES,
        )
        self.assertEqual(int(constants["MAX_MANIFEST_BYTES"]), 128 * 1024)
        self.assertEqual(int(constants["MAX_SIGNATURE_BYTES"]), 1024)
        self.assertEqual(int(constants["MAX_ARCHIVE_ENTRIES"]), 128)
        self.assertEqual(int(constants["MAX_UNPACKED_BYTES"]), 512 * 1024 * 1024)
        self.assertEqual(int(constants["MAX_LATEST_HEADERS_BYTES"]), 128 * 1024)
        self.assertEqual(int(constants["CURL_CONNECT_TIMEOUT_SECONDS"]), 15)
        self.assertEqual(int(constants["CURL_TOTAL_TIMEOUT_SECONDS"]), 600)
        self.assertIn("ulimit -f", installer)
        self.assertIn("--proto-redir =https", installer)
        self.assertIn("--max-redirs ${CURL_MAX_REDIRECTS}", installer)
        self.assertIn("LC_ALL=C", installer)
        self.assertIn("PACKAGE_MANIFEST_REQUIRED=", installer)
        self.assertIn("Install OpenSSL 3.0 or newer", installer)
        self.assertNotIn("OpenSSL 1.1.1", installer)
        self.assertIn(
            "Refusing to downgrade to the weaker same-origin checksum. Aborting.",
            installer,
        )
        self.assertIn(
            "authenticated archive failed the bounded structural preflight.",
            installer,
        )


if __name__ == "__main__":
    unittest.main()
