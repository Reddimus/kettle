#!/usr/bin/env python3
"""Hermetic tests for verify-release-assets.py."""

from __future__ import annotations

import copy
from contextlib import redirect_stderr
import importlib.util
from io import StringIO
from pathlib import Path
import sys
import tempfile
import unittest


sys.dont_write_bytecode = True

SCRIPT = Path(__file__).with_name("verify-release-assets.py")
SPEC = importlib.util.spec_from_file_location("verify_release_assets", SCRIPT)
assert SPEC and SPEC.loader
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class ReleaseAssetTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.paths = []
        for name, data in (
            ("kettle-linux-x86_64.tar.gz", b"linux"),
            ("kettle-update-manifest.json", b'{"schema":1}\n'),
            ("kettle.rb", b'class Kettle\nend\n'),
        ):
            path = self.root / name
            path.write_bytes(data)
            self.paths.append(path)
        expected = MODULE.local_assets(self.paths)
        self.payload = {
            "tag_name": "v2.43.0",
            "draft": True,
            "prerelease": False,
            "assets": [
                {
                    "name": name,
                    "size": record["size"],
                    "digest": record["digest"],
                    "state": "uploaded",
                }
                for name, record in expected.items()
            ],
        }

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def assert_rejected(self, payload: object) -> None:
        with self.assertRaises(ValueError):
            MODULE.verify(payload, tag="v2.43.0", paths=self.paths)

    def test_exact_draft_is_accepted(self):
        MODULE.verify(self.payload, tag="v2.43.0", paths=self.paths)

    def test_release_identity_and_state_are_fail_closed(self):
        for field, value in (
            ("tag_name", "v9.9.9"),
            ("draft", False),
            ("prerelease", True),
        ):
            with self.subTest(field=field):
                payload = copy.deepcopy(self.payload)
                payload[field] = value
                self.assert_rejected(payload)

    def test_duplicate_extra_and_missing_assets_are_rejected(self):
        duplicate = copy.deepcopy(self.payload)
        duplicate["assets"][1]["name"] = duplicate["assets"][0]["name"]
        self.assert_rejected(duplicate)

        extra = copy.deepcopy(self.payload)
        extra["assets"].append(copy.deepcopy(extra["assets"][0]))
        extra["assets"][-1]["name"] = "unexpected"
        self.assert_rejected(extra)

        missing = copy.deepcopy(self.payload)
        missing["assets"].pop()
        self.assert_rejected(missing)

    def test_malformed_remote_asset_fields_are_rejected(self):
        mutations = (
            ("state", "new"),
            ("size", 0),
            ("size", True),
            ("digest", None),
            ("digest", "sha256:" + "A" * 64),
            ("digest", "sha512:" + "0" * 64),
        )
        for field, value in mutations:
            with self.subTest(field=field, value=value):
                payload = copy.deepcopy(self.payload)
                payload["assets"][0][field] = value
                self.assert_rejected(payload)

    def test_remote_size_or_digest_mismatch_is_rejected(self):
        wrong_size = copy.deepcopy(self.payload)
        wrong_size["assets"][0]["size"] += 1
        self.assert_rejected(wrong_size)

        wrong_digest = copy.deepcopy(self.payload)
        wrong_digest["assets"][0]["digest"] = "sha256:" + "0" * 64
        self.assert_rejected(wrong_digest)

    def test_cli_rejects_an_oversized_api_response_before_json_parsing(self):
        response = self.root / "response.json"
        with response.open("wb") as stream:
            stream.truncate(1024 * 1024 + 1)
        with redirect_stderr(StringIO()):
            with self.assertRaisesRegex(SystemExit, "2"):
                MODULE.main(
                    [
                        "--api-json",
                        str(response),
                        "--tag",
                        "v2.43.0",
                        "--asset",
                        str(self.paths[0]),
                    ]
                )


if __name__ == "__main__":
    unittest.main()
