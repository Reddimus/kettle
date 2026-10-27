#!/usr/bin/env python3
"""Regression coverage for bounded D-Bus maintenance exceptions."""

import copy
import importlib.util
import io
import json
import subprocess
import sys
import tempfile
import unittest
from contextlib import redirect_stdout
from pathlib import Path
from unittest import mock

spec = importlib.util.spec_from_file_location(
    "dbus_scope", Path(__file__).with_name("check-dbus-scope.py")
)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


def fixture():
    names = module.REVIEWED_SOURCES
    return {
        "packages": [
            {
                "id": name,
                "name": name,
                "version": next(iter(module.REVIEWED_VERSIONS.get(name, {"5.4.0"}))),
                "source": next(iter(source)),
            }
            for name, source in names.items()
        ],
        "resolve": {
            "nodes": [
                {
                    "id": name,
                    "deps": [
                        {"pkg": dep}
                        for consumer, dep in module.EXPECTED_EDGES
                        if consumer == name
                    ],
                }
                for name in names
            ]
        },
        "workspace_members": list(module.WORKSPACE_PACKAGES),
    }


class DbusScopeTests(unittest.TestCase):
    def test_metadata_is_utf8_under_a_legacy_host_locale(self):
        metadata = fixture()
        metadata["packages"][0]["description"] = "\u00c1nfora"
        real_run = subprocess.run

        def legacy_locale(*args, **kwargs):
            if kwargs.get("text") or kwargs.get("universal_newlines"):
                kwargs.setdefault("encoding", "cp1252")
            return real_run(*args, **kwargs)

        with tempfile.TemporaryDirectory() as root:
            path = Path(root) / "metadata.json"
            path.write_text(json.dumps(metadata, ensure_ascii=False), encoding="utf-8")
            command = [
                sys.executable,
                "-c",
                (
                    "import sys; from pathlib import Path; "
                    "sys.stdout.buffer.write(Path(sys.argv[1]).read_bytes())"
                ),
                str(path),
            ]
            with (
                mock.patch.object(module, "metadata_command", return_value=command),
                mock.patch.object(module.subprocess, "run", side_effect=legacy_locale),
                redirect_stdout(io.StringIO()),
            ):
                self.assertEqual(module.main(), 0)

    def test_reviewed_graph(self):
        self.assertEqual(module.scope_errors(fixture()), [])

    def test_mutations_require_review(self):
        original = fixture()
        for index, package in enumerate(original["packages"]):
            for key, value in (
                ("version", "999.0.0"),
                ("source", "git+https://example.invalid/fork"),
            ):
                if key == "version" and package["name"] in module.WORKSPACE_PACKAGES:
                    continue
                with self.subTest(package=package["name"], key=key):
                    changed = copy.deepcopy(original)
                    changed["packages"][index][key] = value
                    self.assertTrue(module.scope_errors(changed))

    def test_new_consumer_requires_review(self):
        changed = fixture()
        changed["packages"].append(
            {"id": "extra", "name": "extra", "version": "1", "source": None}
        )
        changed["resolve"]["nodes"].append(
            {"id": "extra", "deps": [{"pkg": "zvariant"}]}
        )
        self.assertTrue(module.scope_errors(changed))

    def test_removed_seed_requires_removing_exception(self):
        changed = fixture()
        changed["packages"] = [
            p for p in changed["packages"] if p["name"] != "zvariant_derive"
        ]
        self.assertTrue(module.scope_errors(changed))

    def test_malformed_graph_fails_closed(self):
        for changed in (
            {},
            {"packages": [], "resolve": {}},
            {"packages": [], "resolve": {"nodes": []}},
        ):
            self.assertTrue(module.scope_errors(changed))


if __name__ == "__main__":
    unittest.main()
