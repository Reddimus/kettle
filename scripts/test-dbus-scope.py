#!/usr/bin/env python3
"""Regression coverage for bounded D-Bus maintenance exceptions."""

import copy
import importlib.util
import unittest
from pathlib import Path

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
