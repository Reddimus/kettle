#!/usr/bin/env python3
"""Cross-platform end-to-end smoke for Kettle's noninteractive CLI."""

from __future__ import annotations

import contextlib
import os
from pathlib import Path
import re
import secrets
import shutil
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


def expected_git_identity() -> str:
    """Return the exact identity the just-built development binary must report."""
    sha = subprocess.run(
        ["git", "rev-parse", "--short=12", "HEAD"],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
        encoding="utf-8",
    ).stdout.strip()
    dirty = subprocess.run(
        ["git", "status", "--porcelain"],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
        encoding="utf-8",
    ).stdout
    return sha + ("+dirty" if dirty else "")


@contextlib.contextmanager
def private_scratch() -> object:
    """Create a scratch root whose Windows ACL passes Kettle's trust policy."""
    if os.name != "nt":
        # Ubuntu's ordinary per-user-group default is 002. Exercise that state
        # even on hosts whose login umask is more restrictive: every private
        # directory below must name 0700 explicitly rather than succeeding by
        # ambient policy. This script is single-threaded, so changing the
        # process-wide umask cannot race another creator.
        previous_umask = os.umask(0o002)
        try:
            with tempfile.TemporaryDirectory(
                prefix="kettle-cli-smoke-"
            ) as temporary:
                yield Path(temporary)
        finally:
            os.umask(previous_umask)
        return

    local_app_data = os.environ.get("LOCALAPPDATA")
    system_root = os.environ.get("SYSTEMROOT")
    if not local_app_data or not system_root:
        raise RuntimeError("Windows CLI smoke requires LOCALAPPDATA and SYSTEMROOT")
    scratch = Path(local_app_data) / f"kettle-cli-smoke-{secrets.token_hex(16)}"
    powershell = (
        Path(system_root)
        / "System32"
        / "WindowsPowerShell"
        / "v1.0"
        / "powershell.exe"
    )
    created = subprocess.run(
        [
            str(powershell),
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "New-Item -ItemType Directory -Path $env:KETTLE_CLI_SCRATCH_PATH "
            "-ErrorAction Stop | Out-Null",
        ],
        check=False,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        env={**os.environ, "KETTLE_CLI_SCRATCH_PATH": str(scratch)},
    )
    if created.returncode != 0:
        raise RuntimeError(
            f"could not create private Windows CLI scratch: {created.stderr.strip()}"
        )
    try:
        yield scratch
    finally:
        shutil.rmtree(scratch, ignore_errors=True)


def main() -> int:
    # A previous checkout's binary is worse than no binary: every assertion can
    # pass while measuring code other than the tree named in `--version`.
    identity = expected_git_identity()
    subprocess.run(
        ["cargo", "build", "--locked", "-q", "-p", "kettle"],
        cwd=ROOT,
        check=True,
    )

    # Python's Windows `mkdir(0o700)` emits an OWNER RIGHTS ACE that Kettle
    # deliberately does not treat as an explicit user identity. The helper uses
    # the native shell's inherited per-user AppData DACL instead.
    with private_scratch() as scratch:
        environment = os.environ.copy()
        xdg_config = scratch / "xdg-config"
        if os.name != "nt":
            xdg_config.mkdir(mode=0o700)
        environment["XDG_CONFIG_HOME"] = str(xdg_config)

        version = run("--version", environment=environment)
        print(version, end="")
        match = re.search(
            r"^kettle [0-9]+\.[0-9]+\.[0-9]+ "
            r"\(([0-9a-f]{12}(?:\+dirty)?)\)",
            version,
            re.MULTILINE,
        )
        require(match is not None, "--version is missing the version and Git identity")
        assert match is not None
        require(
            match.group(1) == identity,
            f"--version identified {match.group(1)}, expected checkout {identity}",
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
        if os.name != "nt":
            config_path.chmod(0o600)
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
        if os.name == "nt":
            # Preserve the inherited per-user AppData ACL. Passing 0700 to
            # Python on Windows can synthesize an OWNER RIGHTS ACE, which is
            # intentionally outside Kettle's explicit-principal trust policy.
            profile_path.parent.mkdir(parents=True)
        else:
            profile_path.parent.mkdir(parents=True, mode=0o700)
            # `parents=True` applies `mode` only to the leaf on current Python,
            # and an older Kettle may already have created part of this
            # invocation-owned chain. Normalize the whole private fixture.
            for directory in (xdg_config / "kettle", profile_path.parent):
                directory.chmod(0o700)
        profile_path.write_text("font-size = not_a_number\n", encoding="ascii")
        if os.name != "nt":
            profile_path.chmod(0o600)
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
        require(
            "font-size" in bad_profile.stdout,
            "malformed --profile output omitted the field diagnostic: "
            f"{bad_profile.stdout!r}",
        )

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
                [str(EXE), *command],
                cwd=ROOT,
                env=environment,
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                text=True,
                encoding="utf-8",
                errors="replace",
                check=False,
            )
            require(
                result.returncode != 0,
                f"{' '.join(command)} unexpectedly succeeded: {result.stdout.strip()}",
            )

        missing = scratch / "definitely-no-such-path"
        for command in (
            ("--config", str(missing), "--config-path"),
            ("--working-directory", str(missing), "--config-path"),
        ):
            result = subprocess.run(
                [str(EXE), *command],
                cwd=ROOT,
                env=environment,
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                text=True,
                encoding="utf-8",
                errors="replace",
                check=False,
            )
            require(
                result.returncode != 0,
                f"{' '.join(command)} unexpectedly succeeded: {result.stdout.strip()}",
            )

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
