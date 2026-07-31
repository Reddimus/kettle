#!/usr/bin/env python3
"""Cross-platform end-to-end smoke for Kettle's noninteractive CLI."""

from __future__ import annotations

import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile


ROOT = Path(__file__).resolve().parent.parent
EXE = ROOT / "target" / "debug" / ("kettle.exe" if os.name == "nt" else "kettle")


def run(*arguments: str, environment: dict[str, str], expect: int = 0) -> str:
    result = subprocess.run(
        [str(EXE), *arguments],
        cwd=ROOT,
        env=environment,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        encoding="utf-8",
        errors="replace",
        check=False,
    )
    if result.returncode != expect:
        joined = " ".join(arguments)
        raise RuntimeError(
            f"kettle {joined} exited {result.returncode}, expected {expect}:\n"
            f"{result.stdout}"
        )
    return result.stdout


def require(condition: bool, message: str) -> None:
    if not condition:
        raise RuntimeError(message)


def main() -> int:
    if not EXE.is_file():
        subprocess.run(
            ["cargo", "build", "-q", "-p", "kettle"],
            cwd=ROOT,
            check=True,
        )

    with tempfile.TemporaryDirectory(prefix="kettle-cli-smoke-") as temporary:
        scratch = Path(temporary)
        environment = os.environ.copy()
        environment["XDG_CONFIG_HOME"] = str(scratch / "xdg-config")

        version = run("--version", environment=environment)
        print(version, end="")
        require(
            re.search(
                r"^kettle [0-9]+\.[0-9]+\.[0-9]+ "
                r"\([0-9a-f]+(?:\+dirty)?\)",
                version,
                re.MULTILINE,
            )
            is not None,
            "--version is missing the version and Git identity",
        )

        help_text = run("--help", environment=environment)
        require(re.search(r"^Usage: kettle", help_text, re.MULTILINE) is not None,
                "--help is missing the Usage line")
        for flag in (
            "--config",
            "--screenshot",
            "--gpu-info",
            "--shell-integration",
            "--print-completions",
            "--print-default-config",
        ):
            require(flag in help_text, f"--help is missing {flag}")

        check = run("--check-config", environment=environment)
        require(re.search(r"^kettle:  [0-9]", check, re.MULTILINE) is not None,
                "--check-config is missing its build identity")
        require(
            re.search(
                r"^hint: +kettle --print-default-config > ",
                check,
                re.MULTILINE,
            )
            is not None,
            "--check-config is missing its bootstrap hint",
        )
        require(run("--config-path", environment=environment).strip() != "",
                "--config-path returned no path")

        require(len(run("--list-themes", environment=environment).splitlines()) > 400,
                "--list-themes returned too few themes")
        require(len(run("--list-actions", environment=environment).splitlines()) > 50,
                "--list-actions returned too few actions")
        require(len(run("--list-keybinds", environment=environment).splitlines()) > 40,
                "--list-keybinds returned too few keybinds")
        require(
            run("--list-ssh-hosts", environment=environment).strip()
            == "(no ssh-host entries configured)",
            "--list-ssh-hosts did not emit its empty fallback",
        )

        default_config = run("--print-default-config", environment=environment)
        require(len(default_config.splitlines()) > 50,
                "--print-default-config returned too little data")
        config_path = scratch / "k.cfg"
        config_path.write_text(default_config, encoding="utf-8", newline="\n")
        require(
            re.search(
                r"^status: +OK",
                run("--config", str(config_path), "--check-config",
                    environment=environment),
                re.MULTILINE,
            )
            is not None,
            "the default config did not round-trip",
        )

        profile_path = (
            Path(environment["XDG_CONFIG_HOME"])
            / "kettle"
            / "profiles"
            / "cibad.config"
        )
        profile_path.parent.mkdir(parents=True)
        profile_path.write_text("font-size = not_a_number\n", encoding="ascii")
        bad_profile = subprocess.run(
            [str(EXE), "--profile", "cibad", "--check-config"],
            cwd=ROOT,
            env=environment,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            encoding="utf-8",
            errors="replace",
            check=False,
        )
        require(bad_profile.returncode != 0,
                "malformed --profile config unexpectedly succeeded")
        require("font-size" in bad_profile.stdout,
                "malformed --profile output omitted the field diagnostic")

        for shell in ("bash", "zsh", "fish", "powershell"):
            integration = run("--shell-integration", shell, environment=environment)
            require("OSC 133" in integration and len(integration.splitlines()) > 8,
                    f"{shell} integration output is incomplete")
            completions = run("--print-completions", shell, environment=environment)
            require(len(completions.encode("utf-8")) > 200 and "kettle" in completions,
                    f"{shell} completions output is incomplete")

        for command in (
            ("--shell-integration", "tcsh"),
            ("--print-completions", "tcsh"),
        ):
            result = subprocess.run(
                [str(EXE), *command], cwd=ROOT, env=environment, check=False
            )
            require(result.returncode != 0, f"{' '.join(command)} unexpectedly succeeded")

        missing = scratch / "definitely-no-such-path"
        for command in (
            ("--config", str(missing), "--config-path"),
            ("--working-directory", str(missing), "--config-path"),
        ):
            result = subprocess.run(
                [str(EXE), *command], cwd=ROOT, env=environment, check=False
            )
            require(result.returncode != 0, f"{' '.join(command)} unexpectedly succeeded")

        resolved = run(
            "--config", str(config_path), "--config-path", environment=environment
        ).strip()
        require(Path(resolved).name == config_path.name,
                "--config-path did not resolve the explicit config")

    print("cli-smoke PASSED")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, RuntimeError, subprocess.CalledProcessError) as error:
        print(f"cli-smoke FAILED: {error}", file=sys.stderr)
        raise SystemExit(1)
