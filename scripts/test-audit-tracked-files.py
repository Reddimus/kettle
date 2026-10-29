#!/usr/bin/env python3
"""Focused regression tests for the tracked-file integrity audit."""

from __future__ import annotations

import importlib.util
from pathlib import Path
import subprocess
import sys
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
    def test_regular_file_replaced_by_external_symlink_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "repo"
            root.mkdir()
            tracked = root / "plain.txt"
            external = root.parent / "external.txt"
            write_lf(tracked, "identical bytes\n")
            write_lf(external, "identical bytes\n")
            initialize_repository(root)
            tracked.unlink()
            tracked.symlink_to(external)

            report = AUDIT.audit(root)
            self.assertIn(
                "plain.txt: tracked path is not a regular file", report["errors"]
            )
            result = subprocess.run(
                [
                    sys.executable,
                    str(SCRIPT),
                    "--root",
                    str(root),
                    "--require-clean-index",
                ],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                check=False,
            )
            self.assertEqual(result.returncode, 1, result.stdout)
            self.assertIn("plain.txt: tracked path is not a regular file", result.stderr)

    def test_parent_directory_replaced_by_external_symlink_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "repo"
            root.mkdir()
            parent = root / "nested"
            parent.mkdir()
            write_lf(parent / "plain.txt", "identical bytes\n")
            initialize_repository(root)
            external = root.parent / "external"
            parent.rename(external)
            parent.symlink_to(external, target_is_directory=True)
            report = AUDIT.audit(root)
            self.assertTrue(report["errors"], "symlinked parent passed audit")
            self.assertEqual(report["files"], [], "external file was read")

    def test_nul_in_structured_data_is_rejected(self) -> None:
        for name, data in (("data.json", b'{"key": "bad\x00value"}\n'),
                           ("data.toml", b'key = "bad\x00value"\n')):
            with self.subTest(name=name), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                (root / name).write_bytes(data)
                initialize_repository(root)
                errors = AUDIT.audit(root)["errors"]
                self.assertTrue(any("structured-data parse failed" in e for e in errors), errors)

    def test_text_unset_files_keep_their_bytes(self) -> None:
        """A file `.gitattributes` marks `-text` keeps its CRLF, its last
        line without a newline and its trailing white space; any other text
        file is still held to the LF rules."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            write_lf(root / ".gitattributes", "* text=auto eol=lf\nexact/** -text\n")
            (root / "exact").mkdir()
            (root / "exact" / "LICENSE.txt").write_bytes(b"line one \r\nline two")
            (root / "exact" / "capture.txt").write_bytes(b"graph LR")
            initialize_repository(root)
            self.assertEqual(AUDIT.audit(root)["errors"], [])
            (root / "plain.txt").write_bytes(b"crlf\r\nno newline ")
            subprocess.run(["git", "add", "plain.txt"], cwd=root, check=True)
            errors = AUDIT.audit(root)["errors"]
            self.assertIn("plain.txt: CR/CRLF found; tracked text must use LF", errors)
            self.assertIn("plain.txt: missing final newline", errors)
            self.assertFalse(any(error.startswith("exact/") for error in errors), errors)

    def test_missing_local_markdown_link_with_inline_code_label_is_fatal(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            write_lf(root / "README.md", "[`guide`](docs/missing.md)\n")
            initialize_repository(root)

            result = subprocess.run(
                [sys.executable, str(SCRIPT), "--root", str(root)],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                check=False,
            )

            self.assertEqual(result.returncode, 1, result.stdout)
            self.assertIn(
                "README.md: unresolved local link docs/missing.md",
                result.stderr,
            )

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
