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
import time
from urllib.parse import quote


ROOT = Path(__file__).resolve().parent.parent
FIXTURES = ROOT / "scripts" / "fixtures" / "shell-integration"
INTEGRATION = ROOT / "shell-integration"
OSC_A = b"\x1b]133;A\x07"
OSC_B = b"\x1b]133;B\x07"
OSC_C = b"\x1b]133;C\x07"
OSC_D_1 = b"\x1b]133;D;1\x07"
COMPLETION_SAMPLE = "abc\U0001FAD6def"
COMPLETION_ENCODED = b"abc%F0%9F%AB%96"


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


def assert_completion_field(output: bytes, label: str) -> None:
    begin = b"KETTLE_COMPLETION_BEGIN"
    end = b"KETTLE_COMPLETION_END"
    if begin not in output or end not in output:
        raise RuntimeError(f"{label} omitted its completion sentinels: {output!r}")
    encoded = output.split(begin, 1)[1].split(end, 1)[0]
    if encoded != COMPLETION_ENCODED:
        raise RuntimeError(
            f"{label} cut a UTF-8 completion boundary: "
            f"expected {COMPLETION_ENCODED!r}, got {encoded!r}"
        )


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

    completion = run(
        [
            executable,
            "-f",
            "-c",
            'source "$1" >/dev/null; print -rn KETTLE_COMPLETION_BEGIN; '
            '__kettle_completion_encode "$2" 7; print -rn KETTLE_COMPLETION_END',
            "kettle-completion-check",
            str(INTEGRATION / "kettle.zsh"),
            COMPLETION_SAMPLE,
        ],
        "zsh Unicode completion field fixture",
        announce=False,
    )
    assert_completion_field(completion, "zsh Unicode completion field fixture")
    print("zsh Unicode completion field fixture: PASS")

    # Exercise the maximum displayed list with maximum field sizes. The first
    # encoder used `$(printf ...)` once per byte, turning this bounded 20 KiB
    # payload into tens of thousands of subshells and multi-second Tab presses.
    rows = []
    for index in range(64):
        rows.extend((f"item-{index:02d}-" + "x" * 56, "y" * 256))
    started = time.monotonic()
    maximum = run(
        [
            executable,
            "-f",
            "-c",
            'source "$1" >/dev/null; shift; '
            "kettle_completion_show completion perf 63 \"$@\"",
            "kettle-completion-maximum",
            str(INTEGRATION / "kettle.zsh"),
            *rows,
        ],
        "zsh maximum completion payload fixture",
        announce=False,
    )
    elapsed = time.monotonic() - started
    sequences = re.findall(rb"\x1b\]777;kettle-completion;[^\x07]*\x07", maximum)
    if not sequences or b";completion;63;perf;" not in sequences[-1]:
        raise RuntimeError(f"zsh maximum payload was malformed: {sequences[-1:]!r}")
    fields = sequences[-1][2:-1].split(b";")
    if len(fields) != 8 + 64 * 2:
        raise RuntimeError(
            f"zsh maximum payload kept {(len(fields) - 8) // 2} rows instead of 64"
        )
    if elapsed > 4.0:
        raise RuntimeError(
            f"zsh maximum payload took {elapsed:.2f}s; encoding likely forks per byte"
        )
    print(f"zsh maximum completion payload fixture: PASS ({elapsed:.2f}s)")


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

    completion = run(
        [
            executable,
            "--noprofile",
            "--norc",
            "-c",
            'source "$1" >/dev/null; trap - DEBUG; printf KETTLE_COMPLETION_BEGIN; '
            '__kettle_completion_encode "$2" 7; printf KETTLE_COMPLETION_END',
            "kettle-completion-check",
            str(INTEGRATION / "kettle.bash"),
            COMPLETION_SAMPLE,
        ],
        "Bash Unicode completion field fixture",
        announce=False,
    )
    assert_completion_field(completion, "Bash Unicode completion field fixture")
    print("Bash Unicode completion field fixture: PASS")


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

    completion = run(
        [
            executable,
            "--no-config",
            "-c",
            "source $argv[1] >/dev/null; printf KETTLE_COMPLETION_BEGIN; "
            "__kettle_completion_field $argv[2] 7; printf KETTLE_COMPLETION_END",
            str(INTEGRATION / "kettle.fish"),
            COMPLETION_SAMPLE,
        ],
        "Fish Unicode completion field fixture",
        announce=False,
    )
    assert_completion_field(completion, "Fish Unicode completion field fixture")
    print("Fish Unicode completion field fixture: PASS")

    # A real key-binding round trip, not just a helper call. Fish emits its
    # prompt event during an explicit `repaint`; the first overlay prototype
    # repainted after publishing and therefore cleared the list in the same
    # Tab press. TERM=dumb avoids terminal-query handshakes while retaining
    # Fish's interactive line editor and bindings.
    import fcntl
    import pty
    import select
    import termios
    import time

    master, slave = pty.openpty()

    def own_controlling_terminal() -> None:
        os.setsid()
        fcntl.ioctl(slave, termios.TIOCSCTTY, 0)

    env = os.environ.copy()
    env.update(
        TERM="dumb",
        TERM_PROGRAM="kettle",
        KETTLE_COMPLETION_OVERLAY="1",
    )
    process = subprocess.Popen(
        [
            executable,
            "--no-config",
            "-i",
            "-C",
            f"source {INTEGRATION / 'kettle.fish'}",
        ],
        cwd=ROOT,
        stdin=slave,
        stdout=slave,
        stderr=slave,
        env=env,
        preexec_fn=own_controlling_terminal,
        close_fds=True,
    )
    os.close(slave)

    def drain(seconds: float) -> bytes:
        deadline = time.monotonic() + seconds
        output = bytearray()
        while time.monotonic() < deadline:
            ready, _, _ = select.select([master], [], [], 0.05)
            if not ready:
                continue
            try:
                output.extend(os.read(master, 65536))
            except OSError:
                break
        return bytes(output)

    try:
        startup = drain(1.0)
        os.write(master, b"git ch\t")
        after_tab = drain(1.0)
        sequences = re.findall(
            rb"\x1b\]777;kettle-completion;[^\x07]*\x07", startup + after_tab
        )
        if not sequences or b";show;" not in sequences[-1] or b";checkout;" not in sequences[-1]:
            raise RuntimeError(
                "Fish Tab did not leave its completion list visible: "
                f"{sequences[-3:]!r}"
            )

        os.write(master, b"\x1b[Z")
        reverse = re.findall(
            rb"\x1b\]777;kettle-completion;[^\x07]*\x07", drain(0.5)
        )
        if (
            not reverse
            or b";update;" not in reverse[-1]
            or b";completion;2;fish;" not in reverse[-1]
        ):
            raise RuntimeError(
                "Fish Shift-Tab did not start at the final candidate: "
                f"{reverse[-3:]!r}"
            )

        os.write(master, b"\x03")
        drain(0.25)
        os.write(master, b"fish_vi_key_bindings; source " + str(INTEGRATION / "kettle.fish").encode() + b"\n")
        drain(0.75)
        os.write(master, b"git ch\t")
        vi_sequences = re.findall(
            rb"\x1b\]777;kettle-completion;[^\x07]*\x07", drain(0.75)
        )
        if not vi_sequences or b";show;" not in vi_sequences[-1]:
            raise RuntimeError(
                "Fish Vi insert-mode Tab did not publish completions: "
                f"{vi_sequences[-3:]!r}"
            )

        os.write(master, b"\x1b[D")
        drain(0.2)
        os.write(master, b"\t")
        moved_cursor = re.findall(
            rb"\x1b\]777;kettle-completion;[^\x07]*\x07", drain(0.5)
        )
        if not moved_cursor or b";show;" not in moved_cursor[-1]:
            raise RuntimeError(
                "Fish reused a completion cycle after the cursor moved: "
                f"{moved_cursor[-3:]!r}"
            )
    finally:
        try:
            os.write(master, b"\x03exit\n")
        except OSError:
            pass
        try:
            process.wait(timeout=2)
        except subprocess.TimeoutExpired:
            process.terminate()
            process.wait(timeout=2)
        os.close(master)
    print("Fish interactive completion binding fixture: PASS")


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

    completion_output = run(
        [
            *common,
            str(FIXTURES / "powershell-completion.ps1"),
            "-IntegrationPath",
            str(INTEGRATION / "kettle.ps1"),
        ],
        f"{Path(executable).name} Unicode completion field fixture",
    )
    if b"KETTLE_POWERSHELL_COMPLETION_OK" not in completion_output:
        raise RuntimeError("PowerShell completion fixture omitted its success sentinel")

    enabled_output = run(
        [
            *common,
            str(FIXTURES / "powershell-completion-enabled.ps1"),
            "-IntegrationPath",
            str(INTEGRATION / "kettle.ps1"),
        ],
        f"{Path(executable).name} completion binding fixture",
    )
    if b"KETTLE_POWERSHELL_COMPLETION_ENABLED_OK" not in enabled_output:
        raise RuntimeError("PowerShell enabled completion fixture omitted its success sentinel")


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
