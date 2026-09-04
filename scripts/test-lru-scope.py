#!/usr/bin/env python3
"""Focused tests for the RUSTSEC-2026-0253 dependency-scope guard."""

from __future__ import annotations

import copy
import importlib.util
from pathlib import Path
import unittest


SCRIPT = Path(__file__).with_name("check-lru-scope.py")
SPEC = importlib.util.spec_from_file_location("check_lru_scope", SCRIPT)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError(f"could not load {SCRIPT}")
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


def fixture() -> dict[str, object]:
    identities = {
        "lru": ("lru", "0.16.4", MODULE.CRATES_IO_SOURCE),
        "glyphon": ("glyphon", "0.12.0", MODULE.CRATES_IO_SOURCE),
        "render": ("kettle-render", "2.56.0", None),
        "ui": ("kettle-ui", "2.56.0", None),
        "kettle": ("kettle", "2.56.0", None),
    }
    edges = {
        "lru": [],
        "glyphon": ["lru"],
        "render": ["glyphon"],
        "ui": ["render"],
        "kettle": ["render", "ui"],
    }
    return {
        "packages": [
            {"id": package_id, "name": name, "version": version, "source": source}
            for package_id, (name, version, source) in identities.items()
        ],
        "resolve": {
            "nodes": [
                {"id": package_id, "deps": [{"pkg": dep} for dep in deps]}
                for package_id, deps in edges.items()
            ]
        },
        "workspace_members": ["render", "ui", "kettle"],
    }


class LruScopeTests(unittest.TestCase):
    def test_reviewed_graph_is_accepted(self) -> None:
        self.assertEqual(MODULE.scope_errors(fixture()), [])

    def test_windows_only_consumer_requires_review(self) -> None:
        changed = copy.deepcopy(fixture())
        changed["packages"].append(  # type: ignore[index,union-attr]
            {"id": "windows-user", "name": "windows-user", "version": "1.0.0"}
        )
        # Cargo metadata includes target-specific edges without a platform
        # filter. This is the shape a cfg(windows) dependency contributes even
        # while the guard itself runs on Linux CI.
        changed["resolve"]["nodes"].append(  # type: ignore[index,union-attr]
            {"id": "windows-user", "deps": [{"pkg": "lru"}]}
        )
        errors = MODULE.scope_errors(changed)
        self.assertTrue(any("windows-user -> lru" in error for error in errors))

    def test_lru_version_change_requires_review(self) -> None:
        changed = fixture()
        changed["packages"][0]["version"] = "0.18.2"  # type: ignore[index]
        errors = MODULE.scope_errors(changed)
        self.assertIn(
            "lru versions changed: expected ['0.16.4'], got ['0.18.2']", errors
        )

    def test_glyphon_version_change_requires_review(self) -> None:
        changed = fixture()
        changed["packages"][1]["version"] = "0.13.0"  # type: ignore[index]
        errors = MODULE.scope_errors(changed)
        self.assertIn(
            "glyphon versions changed: expected ['0.12.0'], got ['0.13.0']",
            errors,
        )

    def test_upstream_source_change_requires_review(self) -> None:
        for index, name in ((0, "lru"), (1, "glyphon")):
            with self.subTest(name=name):
                changed = copy.deepcopy(fixture())
                changed["packages"][index]["source"] = (  # type: ignore[index]
                    f"git+https://example.invalid/{name}"
                )
                errors = MODULE.scope_errors(changed)
                self.assertTrue(
                    any(f"{name} sources changed" in error for error in errors),
                    errors,
                )

    def test_duplicate_consumer_identity_requires_review(self) -> None:
        changed = copy.deepcopy(fixture())
        changed["packages"].append(  # type: ignore[index,union-attr]
            {
                "id": "evil-render",
                "name": "kettle-render",
                "version": "999.0.0",
                "source": "git+https://example.invalid/evil",
            }
        )
        changed["resolve"]["nodes"].append(  # type: ignore[index,union-attr]
            {"id": "evil-render", "deps": [{"pkg": "glyphon"}]}
        )
        for node in changed["resolve"]["nodes"]:  # type: ignore[index,union-attr]
            if node["id"] == "kettle":
                node["deps"].append({"pkg": "evil-render"})
        errors = MODULE.scope_errors(changed)
        self.assertTrue(
            any("multiple resolved packages named kettle-render" in error for error in errors),
            errors,
        )

    def test_workspace_consumer_replacement_requires_review(self) -> None:
        changed = copy.deepcopy(fixture())
        changed["workspace_members"].remove("render")  # type: ignore[union-attr]
        errors = MODULE.scope_errors(changed)
        self.assertIn(
            "kettle-render is no longer the reviewed workspace package: render",
            errors,
        )

    def test_metadata_invocation_is_locked_and_all_target(self) -> None:
        command = MODULE.metadata_command()
        self.assertIn("--locked", command)
        self.assertIn("--all-features", command)
        self.assertNotIn("--filter-platform", command)

    def test_malformed_metadata_fails_closed(self) -> None:
        self.assertTrue(MODULE.scope_errors({}))


if __name__ == "__main__":
    unittest.main()
