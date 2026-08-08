#!/usr/bin/env python3
"""Execute Kettle's shell snippets in the native interpreters that expose regressions."""

from __future__ import annotations

import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import tempfile
from urllib.parse import quote


ROOT = Path(__file__).resolve().parent.parent
FIXTURES = ROOT / "scripts" / "fixtures" / "shell-integration"
INTEGRATION = ROOT / "shell-integration"
OSC_A = b"\x1b]133;A\x07"
OSC_B = b"\x1b]133;B\x07"
OSC_C = b"\x1b]133;C\x07"
OSC_D_1 = b"\x1b]133;D;1\x07"


def run(command: list[str], label: str, announce: bool = True) -> bytes:
    result = subprocess.run(
        command,
        cwd=ROOT,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        check=False,
    )
    if result.returncode != 0:
        output = result.stdout.decode("utf-8", errors="replace")
        raise RuntimeError(f"{label} failed with exit {result.returncode}:\n{output}")
    # `announce=False` for a check whose assertions run in THIS file after the
    # subprocess returns. The other fixtures assert internally and exit nonzero,
    # so a clean exit really is a pass for them; printing PASS here for a check
    # that has not been verified yet would announce success before doing any.
    if announce:
        print(f"{label}: PASS")
    return result.stdout


def check_zsh(executable: str) -> None:
    prompt_output = run(
        [
            executable,
            "-f",
            "-i",
            str(FIXTURES / "zsh-prompt.zsh"),
            str(INTEGRATION / "kettle.zsh"),
        ],
        "zsh -f interactive prompt fixture",
    )
    begin = b"KETTLE_ZSH_RENDER_BEGIN"
    end = b"KETTLE_ZSH_RENDER_END"
    if begin not in prompt_output or end not in prompt_output:
        raise RuntimeError("zsh fixture omitted its rendered-prompt sentinels")
    rendered = prompt_output.split(begin, 1)[1].split(end, 1)[0]
    # Interactive zsh runs preexec before each fixture command, including the
    # two print commands around the prompt. Remove those legitimate C marks.
    rendered = rendered.replace(b"\x1b]133;C\x07", b"")
    expected = OSC_B + b"USER> "
    if rendered != expected:
        raise RuntimeError(
            "zsh rendered prompt did not contain exactly one real OSC 133;B "
            f"before the user prompt: {rendered!r}"
        )

    hook_output = run(
        [
            executable,
            "-f",
            str(FIXTURES / "zsh-hooks.zsh"),
            str(INTEGRATION / "kettle.zsh"),
        ],
        "zsh -f hook-preservation and re-source fixture",
    )
    if b"KETTLE_ZSH_HOOKS_OK" not in hook_output:
        raise RuntimeError("zsh hook fixture omitted its success sentinel")


def check_bash(executable: str, require_32: bool) -> None:
    version = run([executable, "--version"], "Bash version probe").splitlines()[0]
    if require_32 and b"version 3.2" not in version:
        raise RuntimeError(
            f"macOS fixture must use the shipped Bash 3.2, got {version!r}"
        )

    with tempfile.TemporaryDirectory(prefix="kettle shell ") as temporary:
        cwd = Path(temporary) / "kt test" / "\u00fcn\u00efcode"
        cwd.mkdir(parents=True)
        output = run(
            [
                executable,
                "--noprofile",
                "--norc",
                str(FIXTURES / "bash-osc7.bash"),
                str(INTEGRATION / "kettle.bash"),
                str(cwd),
            ],
            "Bash OSC 7 UTF-8 fixture",
        )

    prefix = b"\x1b]7;file://"
    start = output.rfind(prefix)
    if start < 0:
        raise RuntimeError(f"Bash fixture emitted no OSC 7 report: {output!r}")
    payload = output[start + len(prefix) :].split(b"\x07", 1)[0]
    slash = payload.find(b"/")
    if slash < 0:
        raise RuntimeError(f"Bash OSC 7 report omitted its absolute path: {payload!r}")
    encoded_path = payload[slash:].decode("ascii")
    expected_path = quote(str(cwd), safe="/:_.~-")
    if encoded_path != expected_path:
        raise RuntimeError(
            "Bash OSC 7 path was not encoded as three-character byte escapes: "
            f"expected {expected_path!r}, got {encoded_path!r}"
        )
    if "%" in re.sub(r"%[0-9A-F]{2}", "", encoded_path):
        raise RuntimeError(f"Bash OSC 7 report contains a malformed escape: {encoded_path!r}")


def check_fish(executable: str) -> None:
    with tempfile.TemporaryDirectory(prefix="kettle fish ") as temporary:
        cwd = Path(temporary) / "kt test" / "\u00fcn\u00efcode"
        cwd.mkdir(parents=True)
        output = run(
            [
                executable,
                "--no-config",
                str(FIXTURES / "fish-osc.fish"),
                str(INTEGRATION / "kettle.fish"),
                str(cwd),
            ],
            "Fish OSC 133 and OSC 7 fixture",
            announce=False,
        )

    marks = (OSC_A, OSC_C, OSC_D_1)
    positions = [output.find(mark) for mark in marks]
    if not (0 <= positions[0] < positions[1] < positions[2]):
        raise RuntimeError(
            "Fish fixture did not emit ordered OSC 133 A, C, D;1 marks: "
            f"{output!r}"
        )
    for mark in marks:
        if output.count(mark) != 1:
            raise RuntimeError(f"Fish fixture did not emit exactly one {mark!r}: {output!r}")

    prefix = b"\x1b]7;file://"
    start = output.find(prefix, positions[0] + len(OSC_A), positions[1])
    if start < 0:
        raise RuntimeError(f"Fish fixture emitted no OSC 7 cwd report: {output!r}")
    payload = output[start + len(prefix) :].split(b"\x07", 1)[0]
    slash = payload.find(b"/")
    if slash < 0:
        raise RuntimeError(f"Fish OSC 7 report omitted its absolute path: {payload!r}")
    encoded_path = payload[slash:].decode("ascii")
    expected_path = quote(str(cwd), safe="/:_.~-")
    if encoded_path != expected_path:
        raise RuntimeError(
            "Fish OSC 7 cwd did not preserve separators and URL-encode segments: "
            f"expected {expected_path!r}, got {encoded_path!r}"
        )

    print("Fish OSC 133 and OSC 7 fixture: PASS")


def check_powershell(executable: str) -> None:
    common = [
        executable,
        "-NoLogo",
        "-NoProfile",
        "-NonInteractive",
        "-ExecutionPolicy",
        "Bypass",
        "-File",
    ]
    prompt_output = run(
        [
            *common,
            str(FIXTURES / "powershell-prompt.ps1"),
            "-IntegrationPath",
            str(INTEGRATION / "kettle.ps1"),
        ],
        f"{Path(executable).name} prompt-status fixture",
    )
    prompt_start = prompt_output.find(b"\x1b]133;A\x07")
    user_prompt = prompt_output.find(b"USER-PROMPT")
    prompt_end = prompt_output.find(OSC_B)
    if not (0 <= prompt_start < user_prompt < prompt_end):
        raise RuntimeError(
            "PowerShell prompt markers were not ordered A, rendered prompt, B: "
            f"{prompt_output!r}"
        )
    if b"KETTLE_POWERSHELL_PROMPT_OK" not in prompt_output:
        raise RuntimeError("PowerShell prompt fixture omitted its success sentinel")

    enter_output = run(
        [
            *common,
            str(FIXTURES / "powershell-enter.ps1"),
            "-IntegrationPath",
            str(INTEGRATION / "kettle.ps1"),
        ],
        f"{Path(executable).name} Enter-handler preservation fixture",
    )
    if b"KETTLE_POWERSHELL_ENTER_OK" not in enter_output:
        raise RuntimeError("PowerShell Enter fixture omitted its success sentinel")


def main() -> int:
    system = platform.system()

    if system == "Darwin":
        check_zsh("/bin/zsh")
        check_bash("/bin/bash", require_32=True)
    elif os.name != "nt":
        zsh = shutil.which("zsh")
        if zsh is None:
            print("zsh fixture: SKIP (zsh unavailable; macOS CI is the required native leg)")
        else:
            check_zsh(zsh)
        bash = shutil.which("bash")
        if bash is None:
            raise RuntimeError("Bash fixture requires bash")
        check_bash(bash, require_32=False)

    fish = shutil.which("fish")
    if fish is None:
        print("fish fixture: SKIP (fish unavailable; Linux CI is the required native leg)")
    else:
        check_fish(fish)

    powershells: list[str] = []
    if os.name == "nt":
        for name in ("powershell.exe", "pwsh.exe"):
            executable = shutil.which(name)
            if executable is not None:
                powershells.append(executable)
        if not powershells:
            raise RuntimeError("PowerShell fixture requires powershell.exe or pwsh.exe")
    else:
        pwsh = shutil.which("pwsh")
        if pwsh is not None:
            powershells.append(pwsh)
        else:
            print("PowerShell fixture: SKIP (pwsh unavailable; Windows CI is required)")

    for executable in powershells:
        check_powershell(executable)

    print("shell-integration fixtures PASSED")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
