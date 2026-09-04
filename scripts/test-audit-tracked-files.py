#!/usr/bin/env python3
"""Focused regression tests for the tracked-file integrity audit."""

from __future__ import annotations

import importlib.util
from pathlib import Path
import subprocess
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("audit-tracked-files.py")
SPEC = importlib.util.spec_from_file_location("audit_tracked_files", SCRIPT)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError(f"cannot load {SCRIPT}")
AUDIT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(AUDIT)


def initialize_repository(root: Path) -> None:
    subprocess.run(["git", "init", "--quiet"], cwd=root, check=True)
    subprocess.run(["git", "add", "."], cwd=root, check=True)
    subprocess.run(
        [
            "git",
            "-c",
            "user.name=Kettle fixture",
            "-c",
            "user.email=kettle-fixture@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "fixture",
        ],
        cwd=root,
        check=True,
    )


def write_lf(path: Path, text: str) -> None:
    """Create a fixture with the byte-level LF policy the audit expects."""
    path.write_bytes(text.encode("utf-8"))


class TrackedFileAuditTests(unittest.TestCase):
    def test_missing_local_markdown_link_is_fatal(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            write_lf(root / "README.md", "[missing](docs/missing.md)\n")
            initialize_repository(root)

            report = AUDIT.audit(root)

            self.assertIn(
                "README.md: unresolved local link docs/missing.md", report["errors"]
            )
            self.assertEqual(report["warnings"], [])

    def test_existing_local_markdown_link_is_clean(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            docs = root / "docs"
            docs.mkdir()
            write_lf(root / "README.md", "[guide](docs/guide.md)\n")
            write_lf(docs / "guide.md", "# Guide\n")
            initialize_repository(root)

            report = AUDIT.audit(root)

            self.assertEqual(report["errors"], [])
            self.assertEqual(report["warnings"], [])


if __name__ == "__main__":
    unittest.main()
