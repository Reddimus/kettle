"""Exercise CI package setup without root privileges or network access."""

from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().with_name("install-ci-linux-deps.sh")

SUDO = r"""#!/usr/bin/env python3
import os
import sys
args = sys.argv[1:]
assert args.pop(0) == "-n", args
assert args[0] in ("sed", "env"), args
if args[0] == "sed":
    args[0] = "/usr/bin/sed"
    if sys.platform == "darwin":
        args.insert(2, "")
else:
    args[0] = "/usr/bin/env"
os.execvp(args[0], args)
"""

APT = r"""#!/usr/bin/env python3
import json
import os
from pathlib import Path
import sys
args = sys.argv[1:]
with Path(os.environ["KETTLE_CI_APT_TRACE"]).open("a") as log:
    entry = {"argv": args, "frontend": os.environ.get("DEBIAN_FRONTEND")}
    log.write(json.dumps(entry) + "\n")
if os.environ.get("KETTLE_CI_APT_FAIL") in args:
    sys.exit(23)
"""


@unittest.skipIf(sys.platform == "win32", "requires Bash and Unix command fixtures")
class PackageSetupTests(unittest.TestCase):
    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory(prefix="kettle-ci-apt-")
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.trace = self.root / "trace.jsonl"
        self.environment = {
            key: value
            for key, value in os.environ.items()
            if key not in {"BASH_ENV", "ENV", "KETTLE_CI_APT_FAIL"}
            and not key.startswith("BASH_FUNC_")
        }
        self.environment.update(
            PATH=f"{self.root}{os.pathsep}{os.environ['PATH']}",
            KETTLE_CI_APT_TRACE=str(self.trace),
        )
        self.bash = os.environ.get("KETTLE_CI_TEST_BASH", "/bin/bash")
        for name, text in (("sudo", SUDO), ("apt-get", APT)):
            path = self.root / name
            path.write_text(text)
            path.chmod(0o700)

    def shell(self, command: str, *arguments: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [
                self.bash,
                "--noprofile",
                "--norc",
                "-c",
                'source "$1"; ' + command,
                "fixture",
                str(SCRIPT),
                *arguments,
            ],
            cwd=self.root,
            env=self.environment,
            capture_output=True,
            text=True,
            check=False,
            timeout=20,
        )

    def calls(self) -> list[dict[str, object]]:
        return (
            [json.loads(line) for line in self.trace.read_text().splitlines()]
            if self.trace.exists()
            else []
        )

    def test_no_packages_fails_before_any_system_command(self) -> None:
        result = subprocess.run(
            [self.bash, str(SCRIPT)],
            cwd=self.root,
            env=self.environment,
            capture_output=True,
            text=True,
            check=False,
            timeout=20,
        )
        self.assertEqual(result.returncode, 64)
        self.assertIn("Usage:", result.stderr)
        self.assertEqual(self.calls(), [])

    def test_direct_sources_and_mirror_lists_retain_unrelated_urls(self) -> None:
        texts = [
            "deb http://azure.archive.ubuntu.com/ubuntu noble main\n",
            "Types: deb\nURIs: http://azure.archive.ubuntu.com/ubuntu\nSuites: noble\n",
            "http://azure.archive.ubuntu.com/ubuntu\nhttps://security.ubuntu.com/ubuntu\n",
            "http://azure.archive.ubuntu.com/ubuntu\nhttp://azureXarchive.ubuntu.com/ubuntu\n",
        ]
        (self.root / "sources.list.d").mkdir()
        names = [
            "sources.list",
            "sources.list.d/ubuntu.sources",
            "apt-mirrors.txt",
            "apt-security-mirrors.txt",
        ]
        sources = [(self.root / name, texts[index]) for index, name in enumerate(names)]
        for path, text in sources:
            path.write_text(text)
        result = self.shell('rewrite_ci_mirrors "$2"', str(self.root))
        self.assertEqual(result.returncode, 0, result.stderr)
        for path, text in sources:
            self.assertEqual(
                path.read_text(),
                text.replace(
                    "http://azure.archive.ubuntu.com/ubuntu",
                    "https://archive.ubuntu.com/ubuntu",
                ),
            )
        self.assertEqual(self.calls(), [])
        missing = self.shell('rewrite_ci_mirrors "$2"', str(self.root / "missing"))
        self.assertEqual(missing.returncode, 0, missing.stderr)

    def test_install_is_noninteractive_bounded_and_preserves_arguments(self) -> None:
        packages = ["libx11-dev", "literal; echo unexpected", "--help"]
        result = self.shell('install_ci_packages "${@:2}"', *packages)
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = self.calls()
        self.assertEqual(len(calls), 2)
        self.assertEqual(calls[0]["argv"][-1:], ["update"])
        self.assertEqual(
            calls[1]["argv"][-7:],
            ["install", "--no-install-recommends", "-y", "--", *packages],
        )
        for call in calls:
            self.assertEqual(call["frontend"], "noninteractive")
            for option in (
                "Acquire::Retries=2",
                "Acquire::http::Timeout=30",
                "Acquire::https::Timeout=30",
                "DPkg::Lock::Timeout=60",
            ):
                self.assertIn(option, call["argv"])

    def test_failed_update_never_starts_installation(self) -> None:
        self.environment["KETTLE_CI_APT_FAIL"] = "update"
        result = self.shell('install_ci_packages "${@:2}"', "fish")
        self.assertEqual(result.returncode, 23)
        self.assertEqual(len(self.calls()), 1)

    def test_install_failure_is_reported(self) -> None:
        self.environment["KETTLE_CI_APT_FAIL"] = "install"
        result = self.shell('install_ci_packages "${@:2}"', "fish")
        self.assertEqual(result.returncode, 23)
        self.assertEqual(len(self.calls()), 2)


if __name__ == "__main__":
    unittest.main()
