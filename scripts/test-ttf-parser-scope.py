#!/usr/bin/env python3
"""Focused tests for the RUSTSEC-2026-0192 dependency-scope guard."""

import importlib.util
import json
from pathlib import Path
import subprocess
import unittest
from unittest.mock import patch


SPEC = importlib.util.spec_from_file_location(
    "ttf_scope", Path(__file__).with_name("check-ttf-parser-scope.py")
)
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)

HOST_TREE = (
    "ttf-parser", "fontdb", "cosmic-text", "glyphon", "kettle-render",
    "kettle", "kettle-ui", "kettle", "kettle-ui",
)


def fixture():
    dependencies = {
        "ttf-parser": [], "fontdb": ["ttf-parser"],
        "cosmic-text": ["fontdb"], "glyphon": ["cosmic-text"],
        "kettle-render": ["glyphon"], "kettle-ui": ["kettle-render"],
        "kettle": ["kettle-render", "kettle-ui"],
    }
    return {
        "packages": [{"id": name, "name": name} for name in dependencies],
        "resolve": {"nodes": [
            {"id": name, "deps": [{"pkg": dep} for dep in deps]}
            for name, deps in dependencies.items()
        ]},
    }


class TtfScopeTests(unittest.TestCase):
    def run_guard(self, graph):
        def cargo(command, **kwargs):
            if command[1] == "metadata":
                resolved = graph
                if "--all-features" not in command or "--filter-platform" in command:
                    resolved = fixture()
                output = json.dumps(resolved)
            else:
                output = "\n".join(name + " v1.0.0" for name in HOST_TREE)
            return subprocess.CompletedProcess(command, 0, output, "")

        with patch.object(MODULE.subprocess, "run", side_effect=cargo):
            return MODULE.main()

    def test_reviewed_graph_is_accepted(self):
        self.assertEqual(self.run_guard(fixture()), 0)

    def test_all_platform_and_feature_consumers_are_rejected(self):
        for name, target in (("windows-user", "cfg(windows)"), ("feature-user", None)):
            with self.subTest(consumer=name):
                graph = fixture()
                graph["packages"].append({"id": name, "name": name})
                graph["resolve"]["nodes"].append({
                    "id": name, "features": ["extra"],
                    "deps": [{"pkg": "ttf-parser",
                              "dep_kinds": [{"kind": None, "target": target}]}],
                })
                self.assertEqual(self.run_guard(graph), 1)

    def test_every_reverse_edge_is_checked(self):
        graph = fixture()
        graph["resolve"]["nodes"][-1]["deps"].append({"pkg": "fontdb"})
        self.assertEqual(self.run_guard(graph), 1)


if __name__ == "__main__":
    unittest.main()
