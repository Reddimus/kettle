#!/usr/bin/env python3
"""Command-level regression tests for release.sh."""

from __future__ import annotations

from contextlib import contextmanager
import os
from pathlib import Path
import shutil
import stat
import subprocess
import tempfile
import unittest
from typing import Iterator


ROOT = Path(__file__).resolve().parent.parent
RELEASE_SCRIPT = ROOT / "scripts" / "release.sh"

README = """Windows support ended with 3.3.0.
Historical source: https://github.com/Reddimus/kettle/blob/v3.3.0/README.md
"""

INSTALL = """Pin a specific version (recommended for reproducible installs):

```sh
curl -fsSL https://raw.githubusercontent.com/Reddimus/kettle/main/scripts/install-online.sh \\
  | KETTLE_VERSION=v3.3.0 sh
```

Every release from **v1.3.4** onward ships a `.sha256` sidecar (current latest: v3.3.0)

```sh
curl -fLO https://github.com/Reddimus/kettle/releases/download/v3.3.0/kettle-linux-x86_64.tar.gz
curl -fLO https://github.com/Reddimus/kettle/releases/download/v3.3.0/kettle-linux-x86_64.tar.gz.sha256
```

Archived [v3.3.0 package](https://github.com/Reddimus/kettle/releases/tag/v3.3.0).
Historical installer: https://github.com/Reddimus/kettle/blob/v3.3.0/scripts/install.ps1
Historical download: https://github.com/Reddimus/kettle/releases/download/v3.3.0/kettle-windows-x86_64.zip
"""

HISTORY = """# Version history

- Latest version in this tree: `v3.3.0`, with matching source version,
- Current workspace version: `3.3.0`
- Release headings inspected: 3 across the root `CHANGELOG.md` and
  `docs/changelog/` archives. That count comprises `[Unreleased]` and 2 dated
  versions from `v0.1.0` through `v3.3.0`.
  During release preparation, the newest dated heading has no tag yet;
  `scripts/tag-release.sh` creates it after the release commit merges.

## Planned platform transition

- `v3.3.0` is the final Windows-supported release.
- `v4.0.0` removes Windows distribution support.
- Historical source: https://github.com/Reddimus/kettle/blob/v3.3.0/scripts/install.ps1

## Release eras

- `v2.29.0` to `v3.3.0` (2026-06-19 to 2026-08-24): final Windows era.
"""


def write(path: Path, text: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")


@contextmanager
def release_fixture(
    *, install: str = INSTALL, history: str = HISTORY
) -> Iterator[tuple[Path, dict[str, str], str]]:
    with tempfile.TemporaryDirectory() as temporary:
        root = Path(temporary)
        scripts = root / "scripts"
        scripts.mkdir()
        copied_release = scripts / "release.sh"
        shutil.copy2(RELEASE_SCRIPT, copied_release)

        write(
            root / "Cargo.toml",
            """[workspace]
members = []

[workspace.package]
version = "3.3.0"

[workspace.dependencies]
kettle-state = { path = "crates/kettle-state", version = "3.3.0" }
""",
        )
        write(root / "Cargo.lock", "# fixture lockfile\n")
        write(root / "flake.nix", '          version = "3.3.0";\n')
        write(
            root / "CHANGELOG.md",
            """# Changelog

## [4.0.0] — 2026-08-25
""",
        )
        write(root / "README.md", README)
        write(root / "docs" / "INSTALL.md", install)
        write(root / "docs" / "VERSION-HISTORY.md", history)

        real_git = shutil.which("git")
        if real_git is None:
            raise RuntimeError("git is required")
        fake_bin = root / "fake-bin"
        fake_bin.mkdir()
        write(
            fake_bin / "git",
            """#!/usr/bin/env bash
set -euo pipefail
if [ "${1:-}" = commit ]; then
    shift
    args=()
    for arg in "$@"; do
        [ "$arg" = -S ] || args+=("$arg")
    done
    exec "$KETTLE_TEST_REAL_GIT" commit --no-gpg-sign "${args[@]}"
fi
exec "$KETTLE_TEST_REAL_GIT" "$@"
""",
        )
        write(
            fake_bin / "cargo",
            """#!/usr/bin/env sh
test "${1:-}" = build
""",
        )
        for executable in (fake_bin / "git", fake_bin / "cargo"):
            executable.chmod(executable.stat().st_mode | stat.S_IXUSR)

        subprocess.run(
            [real_git, "init", "--quiet", "-b", "release-test"],
            cwd=root,
            check=True,
        )
        subprocess.run(
            [real_git, "config", "user.name", "Kettle fixture"],
            cwd=root,
            check=True,
        )
        subprocess.run(
            [
                real_git,
                "config",
                "user.email",
                "kettle-fixture@example.invalid",
            ],
            cwd=root,
            check=True,
        )
        subprocess.run([real_git, "add", "."], cwd=root, check=True)
        subprocess.run(
            [real_git, "commit", "--quiet", "-m", "fixture"],
            cwd=root,
            check=True,
        )
        subprocess.run([real_git, "tag", "v3.3.0"], cwd=root, check=True)

        environment = os.environ.copy()
        environment.update(
            {
                "KETTLE_TEST_REAL_GIT": real_git,
                "PATH": f"{fake_bin}{os.pathsep}{environment['PATH']}",
            }
        )
        yield root, environment, real_git


def run_release(root: Path, environment: dict[str, str]) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["bash", "scripts/release.sh", "4.0.0"],
        cwd=root,
        env=environment,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        check=False,
    )


@unittest.skipIf(os.name == "nt", "release.sh requires a Unix shell")
class ReleaseScriptTests(unittest.TestCase):
    def test_release_bump_preserves_archived_version_references(self) -> None:
        with release_fixture() as (root, environment, _real_git):
            result = run_release(root, environment)
            self.assertEqual(result.returncode, 0, result.stderr)

            readme = (root / "README.md").read_text(encoding="utf-8")
            install = (root / "docs" / "INSTALL.md").read_text(encoding="utf-8")
            history = (root / "docs" / "VERSION-HISTORY.md").read_text(
                encoding="utf-8"
            )

            self.assertEqual(readme, README)
            self.assertIn("KETTLE_VERSION=v4.0.0", install)
            self.assertIn("current latest: v4.0.0", install)
            self.assertEqual(install.count("releases/download/v4.0.0/"), 2)
            self.assertIn(
                "[v3.3.0 package](https://github.com/Reddimus/kettle/releases/tag/v3.3.0)",
                install,
            )
            self.assertIn("blob/v3.3.0/scripts/install.ps1", install)
            self.assertIn(
                "releases/download/v3.3.0/kettle-windows-x86_64.zip", install
            )
            self.assertIn("Latest version in this tree: `v4.0.0`", history)
            self.assertIn("Current workspace version: `4.0.0`", history)
            self.assertIn(
                "Release headings inspected: 3 across the root `CHANGELOG.md`",
                history,
            )
            self.assertIn("`[Unreleased]` and 2 dated", history)
            self.assertIn("`v0.1.0` through `v4.0.0`", history)
            self.assertIn(
                "During release preparation, the newest dated heading has no tag yet",
                history,
            )
            self.assertIn(
                "`scripts/tag-release.sh` creates it after the release commit merges",
                history,
            )
            self.assertIn("`v3.3.0` is the final Windows-supported release", history)
            self.assertIn("blob/v3.3.0/scripts/install.ps1", history)
            self.assertIn("`v2.29.0` to `v3.3.0`", history)

    def test_release_bump_fails_closed_on_anchor_drift(self) -> None:
        current_latest = (
            "Every release from **v1.3.4** onward ships a `.sha256` sidecar "
            "(current latest: v3.3.0)\n"
        )
        workspace_version = "- Current workspace version: `3.3.0`\n"
        fixtures = (
            ("missing install anchor", INSTALL.replace(current_latest, ""), HISTORY),
            (
                "duplicate history anchor",
                INSTALL,
                HISTORY.replace(workspace_version, workspace_version * 2),
            ),
        )

        for name, install, history in fixtures:
            with self.subTest(name=name), release_fixture(
                install=install, history=history
            ) as (root, environment, real_git):
                result = run_release(root, environment)
                self.assertNotEqual(result.returncode, 0, result.stdout)
                self.assertIn("expected exactly one matching line", result.stderr)
                status = subprocess.run(
                    [real_git, "status", "--porcelain=v1"],
                    cwd=root,
                    check=True,
                    stdout=subprocess.PIPE,
                    text=True,
                )
                self.assertEqual(status.stdout, "")


if __name__ == "__main__":
    unittest.main()
