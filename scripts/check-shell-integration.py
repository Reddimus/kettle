#!/usr/bin/env python3
"""Execute Kettle's shell snippets in the native interpreters that expose regressions."""

from __future__ import annotations

import getpass
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
COMPLETION_LONG_SAMPLE = "caf\u00e9-m\u00fcnchen-\u00fcber.txt"
COMPLETION_LONG_ENCODED = quote(COMPLETION_LONG_SAMPLE, safe="/:_.~-").encode("ascii")


def run(
    command: list[str],
    label: str,
    announce: bool = True,
    environment: dict[str, str] | None = None,
) -> bytes:
    process_environment = os.environ.copy()
    if environment is not None:
        process_environment.update(environment)
    result = subprocess.run(
        command,
        cwd=ROOT,
        env=process_environment,
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


def assert_completion_field(
    output: bytes, label: str, expected: bytes = COMPLETION_ENCODED
) -> None:
    begin = b"KETTLE_COMPLETION_BEGIN"
    end = b"KETTLE_COMPLETION_END"
    if begin not in output or end not in output:
        raise RuntimeError(f"{label} omitted its completion sentinels: {output!r}")
    encoded = output.split(begin, 1)[1].split(end, 1)[0]
    if encoded != expected:
        raise RuntimeError(
            f"{label} cut a UTF-8 completion boundary: "
            f"expected {expected!r}, got {encoded!r}"
        )


def check_no_inline_completion_fallbacks() -> None:
    fish_source = (INTEGRATION / "kettle.fish").read_text(encoding="utf-8")
    if "commandline -f complete" in fish_source:
        raise RuntimeError(
            "Fish completion can re-query through its inline stock pager"
        )
    if re.search(r"(?m)^function __kettle_completion_emit$", fish_source):
        raise RuntimeError(
            "Fish exposes a publisher that advances requests for keys Kettle does not own"
        )

    source = (INTEGRATION / "kettle.ps1").read_text(encoding="utf-8")
    forbidden = (
        "[Microsoft.PowerShell.PSConsoleReadLine]::TabCompleteNext()",
        "[Microsoft.PowerShell.PSConsoleReadLine]::TabCompletePrevious()",
    )
    present = [call for call in forbidden if call in source]
    if present:
        raise RuntimeError(
            "PowerShell completion can fall back to inline PSReadLine UI: "
            + ", ".join(present)
        )
    required_handlers = (
        "function global:__kettle_completion_handle_next",
        "function global:__kettle_completion_handle_previous",
        "-BriefDescription KettleCompleteNext",
        "-BriefDescription KettleCompletePrevious",
        "__kettle_completion_handle_next",
        "__kettle_completion_handle_previous",
    )
    missing = [handler for handler in required_handlers if handler not in source]
    if missing:
        raise RuntimeError(
            "PowerShell bindings bypass their detached-only failure handlers: "
            + ", ".join(missing)
        )
    print("Detached-only completion source guard: PASS")


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

    locale_probe = subprocess.run(
        [executable, "--noprofile", "--norc", "-c", "locale -a"],
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        check=False,
    )
    locale_output = (
        locale_probe.stdout.decode("ascii", errors="replace")
        if locale_probe.returncode == 0
        else ""
    )
    locales = [line.strip() for line in locale_output.splitlines() if line.strip()]
    normalized = {
        re.sub(r"[-_.]", "", locale.lower()): locale for locale in locales
    }
    utf8_locale = next(
        (normalized[name] for name in ("cutf8", "enusutf8") if name in normalized),
        None,
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

    if utf8_locale is None:
        print(
            "Bash complete Unicode field fixture: SKIP "
            f"(C.UTF-8/en_US.UTF-8 unavailable; locales={locales!r})"
        )
        return

    # Bash expands every assignment in one `local` command before applying
    # `LC_ALL=C`. A combined `local LC_ALL=C ... len=${#1}` therefore counted
    # Unicode characters while the loop sliced bytes, truncating longer labels.
    complete_unicode = run(
        [
            executable,
            "--noprofile",
            "--norc",
            "-c",
            'chars=${#2}; LC_ALL=C; bytes=${#2}; LC_ALL=$3; export LC_ALL; '
            'test "$chars" -lt "$bytes" || exit 89; '
            'source "$1" >/dev/null; trap - DEBUG; printf KETTLE_COMPLETION_BEGIN; '
            '__kettle_completion_encode "$2" 64; printf KETTLE_COMPLETION_END',
            "kettle-completion-complete-unicode",
            str(INTEGRATION / "kettle.bash"),
            COMPLETION_LONG_SAMPLE,
            utf8_locale,
        ],
        "Bash complete Unicode field fixture",
        announce=False,
        environment={"LC_ALL": utf8_locale},
    )
    assert_completion_field(
        complete_unicode,
        "Bash complete Unicode field fixture",
        COMPLETION_LONG_ENCODED,
    )
    print("Bash complete Unicode field fixture: PASS")


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

    rows = [f"item-{index:02d}\tdescription-{index}" for index in range(70)]
    paged = run(
        [
            executable,
            "--no-config",
            "-c",
            "source $argv[1] >/dev/null; "
            "set -g __kettle_completion_cycle_rows $argv[2..-1]; "
            "__kettle_completion_emit_cycle update 64",
            str(INTEGRATION / "kettle.fish"),
            *rows,
        ],
        "Fish completion paging fixture",
        announce=False,
    )
    paged_sequences = re.findall(
        rb"\x1b\]777;kettle-completion;[^\x07]*\x07", paged
    )
    if (
        not paged_sequences
        or b";update;" not in paged_sequences[-1]
        or b";completion;0;fish;;;64;70;item-64;description-64"
        not in paged_sequences[-1]
        or b"item-00" in paged_sequences[-1]
    ):
        raise RuntimeError(
            "Fish did not page a >64 result around the selected candidate: "
            f"{paged_sequences[-1:]!r}"
        )
    print("Fish completion paging fixture: PASS")

    wide_rows = [f"item-{index:03d}\t{'説明' * 80}" for index in range(128)]
    wide = run(
        [
            executable,
            "--no-config",
            "-c",
            "source $argv[1] >/dev/null; "
            "set -g __kettle_completion_cycle_prefix (string repeat -n 1024 a); "
            "set -g __kettle_completion_cycle_rows $argv[2..-1]; "
            "__kettle_completion_emit_cycle update 127",
            str(INTEGRATION / "kettle.fish"),
            *wide_rows,
        ],
        "Fish completion wire-budget paging fixture",
        announce=False,
    )
    wide_sequences = re.findall(
        rb"\x1b\]777;kettle-completion;[^\x07]*\x07", wide
    )
    wide_fields = wide_sequences[-1][2:-1].split(b";") if wide_sequences else []
    if (
        not wide_sequences
        or len(wide_fields) < 14
        or wide_fields[8] != b"63"
        or wide_fields[9] != b"fish"
        or wide_fields[10] != b""
        or wide_fields[11] != b"a" * 1024
        or wide_fields[12:14] != [b"64", b"128"]
        or b"item-127" not in wide_sequences[-1]
        or len(wide_sequences[-1]) > 65538
    ):
        raise RuntimeError(
            "Fish lost the selected row at the bounded wire budget: "
            f"{wide_sequences[-1:]!r}"
        )
    print("Fish completion wire-budget paging fixture: PASS")

    saturated_rows = [
        f"{'😀' * (16 if index < 63 else 15)}{'Z' if index == 63 else ''}\t{'😀' * 64}"
        for index in range(64)
    ]
    saturated = run(
        [
            executable,
            "--no-config",
            "-c",
            "source $argv[1] >/dev/null; "
            "set -g __kettle_completion_cycle_token "
            "(__kettle_completion_field (string repeat -n 32 😀) 128); "
            "set -g __kettle_completion_cycle_prefix "
            "(__kettle_completion_field (string repeat -n 256 😀) 1024); "
            "set -g __kettle_completion_cycle_rows $argv[2..-1]; "
            "__kettle_completion_emit_cycle update 63",
            str(INTEGRATION / "kettle.fish"),
            *saturated_rows,
        ],
        "Fish selected-row wire-budget retry fixture",
        announce=False,
    )
    saturated_sequences = re.findall(
        rb"\x1b\]777;kettle-completion;[^\x07]*\x07", saturated
    )
    saturated_fields = (
        saturated_sequences[-1][2:-1].split(b";") if saturated_sequences else []
    )
    if (
        not saturated_sequences
        or len(saturated_fields) < 16
        or saturated_fields[8] != b"0"
        or saturated_fields[12:14] != [b"63", b"64"]
        or b"Z" not in saturated_sequences[-1]
        or len(saturated_sequences[-1]) > 65538
    ):
        raise RuntimeError(
            "Fish did not re-page from a selected row dropped by the wire budget: "
            f"{saturated_sequences[-1:]!r}"
        )
    print("Fish selected-row wire-budget retry fixture: PASS")

    placeholders = run(
        [
            executable,
            "--no-config",
            "-c",
            "source $argv[1] >/dev/null; "
            "__kettle_completion_emit_rows show 1 20 22 1 "
            "(string join \\t -- '' dropped) (string join \\t -- safe kept)",
            str(INTEGRATION / "kettle.fish"),
        ],
        "Fish skipped-label position fixture",
        announce=False,
    )
    placeholder_sequences = re.findall(
        rb"\x1b\]777;kettle-completion;[^\x07]*\x07", placeholders
    )
    placeholder_fields = (
        placeholder_sequences[-1][2:-1].split(b";")
        if placeholder_sequences
        else []
    )
    if (
        len(placeholder_fields) < 18
        or placeholder_fields[8] != b"1"
        or placeholder_fields[12:14] != [b"20", b"22"]
        or placeholder_fields[14:18] != [b"", b"dropped", b"safe", b"kept"]
    ):
        raise RuntimeError(
            "Fish did not preserve absolute positions across an omitted label: "
            f"{placeholder_sequences[-1:]!r}"
        )
    print("Fish skipped-label position fixture: PASS")

    captured = run(
        [
            executable,
            "--no-config",
            "-c",
            "source $argv[1] >/dev/null; "
            "functions --erase __kettle_completion_rows; "
            "function __kettle_completion_rows; "
            "for index in (seq 1 2050); printf 'item-%04d\\tdescription\\n' $index; end; "
            "end; __kettle_completion_capture; "
            "printf 'KETTLE_CAPTURE=%d:%s:%s' "
            "(count $__kettle_completion_cycle_rows) "
            "$__kettle_completion_cycle_labels[1] "
            "$__kettle_completion_cycle_labels[-1]",
            str(INTEGRATION / "kettle.fish"),
        ],
        "Fish completion retained-state cap fixture",
        announce=False,
    )
    if b"KETTLE_CAPTURE=2048:item-0001:item-2048" not in captured:
        raise RuntimeError(
            "Fish did not enforce its retained completion cap: " f"{captured!r}"
        )
    print("Fish completion retained-state cap fixture: PASS")

    source_budgets = run(
        [
            executable,
            "--no-config",
            "-c",
            "source $argv[1] >/dev/null; "
            "functions --erase __kettle_completion_rows; "
            "function __kettle_completion_rows; string repeat -n 4097 x; end; "
            "__kettle_completion_capture; "
            "printf 'KETTLE_OVERSIZED=%d;' (count $__kettle_completion_cycle_rows); "
            "function __kettle_completion_rows; "
            "for index in (seq 1 100); "
            "printf '%s%04d\\n' (string repeat -n 3996 x) $index; end; end; "
            "__kettle_completion_capture; "
            "printf 'KETTLE_AGGREGATE=%d' (count $__kettle_completion_cycle_rows)",
            str(INTEGRATION / "kettle.fish"),
        ],
        "Fish completion source-byte budgets fixture",
        announce=False,
    )
    if b"KETTLE_OVERSIZED=0;KETTLE_AGGREGATE=65" not in source_budgets:
        raise RuntimeError(
            "Fish did not enforce its per-field and aggregate source-byte budgets: "
            f"{source_budgets!r}"
        )
    print("Fish completion source-byte budgets fixture: PASS")

    escaped_and_rollover = run(
        [
            executable,
            "--no-config",
            "-c",
            "source $argv[1] >/dev/null; "
            "functions --erase __kettle_completion_rows; "
            "function __kettle_completion_rows; "
            "printf '%s\\t%s\\n' 'foo\\\\nbar' description; "
            "printf '%s\\t%s\\n' ordinary second; end; "
            "__kettle_completion_capture; "
            "printf 'KETTLE_ESCAPED=%d:%d:%d:<%s>:<%s>;' "
            "(count $__kettle_completion_cycle_rows) "
            "(count $__kettle_completion_cycle_labels) "
            "(count $__kettle_completion_cycle_insertions) "
            "$__kettle_completion_cycle_labels[1] "
            "$__kettle_completion_cycle_insertions[1]; "
            "set -g __kettle_completion_request 4503599627370494; "
            "__kettle_completion_begin_request; set -l first_status $status; "
            "set -l first_request $__kettle_completion_request; "
            "__kettle_completion_begin_request; set -l second_status $status; "
            "printf 'KETTLE_ROLLOVER=%d:%s:%d:%s:%d;' "
            "$first_status $first_request $second_status "
            "$__kettle_completion_request "
            "(count $__kettle_completion_cycle_rows); "
            "set -g __kettle_completion_enabled 1; "
            "set -g __kettle_completion_generation $__kettle_completion_counter_max; "
            "__kettle_completion_begin_generation; set -l generation_status $status; "
            "set -g __kettle_completion_enabled 1; "
            "set -g __kettle_completion_session $__kettle_completion_counter_max; "
            "__kettle_completion_begin_session; set -l session_status $status; "
            "printf 'KETTLE_WIDE_ROLLOVER=%d:%d:%d' "
            "$generation_status $session_status $__kettle_completion_enabled",
            str(INTEGRATION / "kettle.fish"),
        ],
        "Fish escaped-control and request-rollover fixture",
        announce=False,
    )
    expected_escaped = (
        b"KETTLE_ESCAPED=2:2:2:<foo\\nbar>:<foo\\nbar>;"
        b"KETTLE_ROLLOVER=0:4503599627370495:1:4503599627370495:0;"
        b"KETTLE_WIDE_ROLLOVER=1:1:0"
    )
    if expected_escaped not in escaped_and_rollover:
        raise RuntimeError(
            "Fish split an escaped control candidate or reused an exhausted request: "
            f"{escaped_and_rollover!r}"
        )
    print("Fish escaped-control and request-rollover fixture: PASS")

    singleton_cases = run(
        [
            executable,
            "--no-config",
            "-c",
            "source $argv[1] >/dev/null; "
            "set -g KETTLE_BUFFER cmd; set -g KETTLE_CURSOR 3; "
            "set -g KETTLE_PROVIDER_CALLS 0; "
            "functions --erase __kettle_completion_rows; "
            "function __kettle_completion_rows; "
            "set -g KETTLE_PROVIDER_CALLS (math $KETTLE_PROVIDER_CALLS + 1); "
            "if test $KETTLE_PROVIDER_CALLS -eq 1; "
            "printf 'ordinary\\tdescription\\n'; else; "
            "printf 'changed-a\\none\\nchanged-b\\ntwo\\n'; end; end; "
            "function commandline; switch $argv[1]; "
            "case -b; printf %s $KETTLE_BUFFER; "
            "case -C; printf %s $KETTLE_CURSOR; case -ct; printf cmd; "
            "case -rt; set -g KETTLE_BUFFER $argv[-1]; "
            "set -g KETTLE_CURSOR (string length -- $KETTLE_BUFFER); "
            "case -i; set -g KETTLE_BUFFER \"$KETTLE_BUFFER$argv[-1]\"; "
            "set -g KETTLE_CURSOR (string length -- $KETTLE_BUFFER); "
            "case -f; printf 'KETTLE_STOCK=%s;' $argv[2]; end; end; "
            "__kettle_completion_cycle " + str(1) + "; "
            "printf 'KETTLE_BUFFER=<%s>;KETTLE_CALLS=%s;' "
            "$KETTLE_BUFFER $KETTLE_PROVIDER_CALLS; "
            "printf 'KETTLE_CASE_BREAK;'; "
            "set -g KETTLE_BUFFER cmd; set -g KETTLE_CURSOR 3; "
            "set -g KETTLE_PROVIDER_CALLS 0; "
            "function __kettle_completion_rows; "
            "set -g KETTLE_PROVIDER_CALLS (math $KETTLE_PROVIDER_CALLS + 1); "
            "if test $KETTLE_PROVIDER_CALLS -eq 1; "
            "printf 'bounded\\tdescription\\n'; string repeat -n 4097 x; "
            "else; printf 'changed-a\\none\\nchanged-b\\ntwo\\n'; end; end; "
            "__kettle_completion_cycle " + str(1) + "; "
            "printf 'KETTLE_BUFFER=<%s>;KETTLE_CALLS=%s;' "
            "$KETTLE_BUFFER $KETTLE_PROVIDER_CALLS; "
            "printf 'KETTLE_CASE_BREAK;'; "
            "set -g KETTLE_BUFFER cmd; set -g KETTLE_CURSOR 3; "
            "set -g KETTLE_PROVIDER_CALLS 0; "
            "function __kettle_completion_rows; "
            "set -g KETTLE_PROVIDER_CALLS (math $KETTLE_PROVIDER_CALLS + 1); "
            "printf 'folder/\\tdirectory\\n'; end; "
            "__kettle_completion_cycle " + str(1) + "; "
            "printf 'KETTLE_BUFFER=<%s>;KETTLE_CALLS=%s;' "
            "$KETTLE_BUFFER $KETTLE_PROVIDER_CALLS; "
            "printf 'KETTLE_CASE_BREAK;'; "
            "set -g KETTLE_BUFFER cmd; set -g KETTLE_CURSOR 3; "
            "set -g KETTLE_PROVIDER_CALLS 0; "
            "function __kettle_completion_rows; "
            "set -g KETTLE_PROVIDER_CALLS (math $KETTLE_PROVIDER_CALLS + 1); "
            "printf '~kettle-user\\tuser\\n'; end; "
            "__kettle_completion_cycle " + str(1) + "; "
            "printf 'KETTLE_BUFFER=<%s>;KETTLE_CALLS=%s' "
            "$KETTLE_BUFFER $KETTLE_PROVIDER_CALLS",
            str(INTEGRATION / "kettle.fish"),
        ],
        "Fish singleton completion stays detached fixture",
        announce=False,
    )
    cases = singleton_cases.split(b"KETTLE_CASE_BREAK;")
    singleton_expectations = (
        (b"KETTLE_BUFFER=<ordinary >;KETTLE_CALLS=1;", "ordinary singleton"),
        (b"KETTLE_BUFFER=<bounded >;KETTLE_CALLS=1;", "bounded singleton"),
        (b"KETTLE_BUFFER=<folder/>;KETTLE_CALLS=1;", "open directory singleton"),
        (b"KETTLE_BUFFER=<~kettle-user>;KETTLE_CALLS=1", "expandable user singleton"),
    )
    if len(cases) != len(singleton_expectations) or any(
        expected not in output
        or b"KETTLE_STOCK=" in output
        or b";show;" not in output
        for output, (expected, _label) in zip(cases, singleton_expectations)
    ):
        raise RuntimeError(
            "Fish singleton insertion lost its delimiter or re-queried its provider: "
            f"{singleton_cases!r}"
        )
    print("Fish singleton completion stays detached fixture: PASS")

    # A real key-binding round trip, not just a helper call. Fish emits its
    # prompt event during an explicit `repaint`; the first overlay prototype
    # repainted after publishing and therefore cleared the list in the same
    # Tab press. Use a capable terminal type so the no-pager assertion cannot
    # pass merely because Fish suppressed its pager for TERM=dumb.
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
        TERM="xterm-256color",
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
    answered_queries: dict[bytes, int] = {}
    query_transcript = bytearray()
    terminal_queries = {
        b"\x1b[?u": b"\x1b[?0u",
        b"\x1b[>0q": b"\x1bP>|Kettle 3.0.1\x1b\\",
        b"\x1b]11;?\x1b\\": b"\x1b]11;rgb:1818/1818/2020\x1b\\",
        b"\x1bP+q696e646e\x1b\\": b"\x1bP0+r696e646e\x1b\\",
        b"\x1bP+q71756572792d6f732d6e616d65\x1b\\": (
            b"\x1bP0+r71756572792d6f732d6e616d65\x1b\\"
        ),
        b"\x1b[6n": b"\x1b[1;1R",
        b"\x1b[0c": b"\x1b[?1;2c",
    }

    def answer_queries(chunk: bytes) -> None:
        query_transcript.extend(chunk)
        for query, response in terminal_queries.items():
            observed = query_transcript.count(query)
            answered = answered_queries.get(query, 0)
            for _ in range(observed - answered):
                os.write(master, response)
            answered_queries[query] = observed

    def drain(seconds: float) -> bytes:
        deadline = time.monotonic() + seconds
        output = bytearray()
        while time.monotonic() < deadline:
            ready, _, _ = select.select([master], [], [], 0.05)
            if not ready:
                continue
            try:
                chunk = os.read(master, 65536)
                output.extend(chunk)
            except OSError:
                break
            answer_queries(chunk)
        return bytes(output)

    def drain_until_completion(seconds: float) -> tuple[bytes, float]:
        started = time.monotonic()
        deadline = started + seconds
        output = bytearray()
        pattern = re.compile(rb"\x1b\]777;kettle-completion;[^\x07]*\x07")
        while time.monotonic() < deadline:
            ready, _, _ = select.select([master], [], [], 0.05)
            if not ready:
                continue
            try:
                chunk = os.read(master, 65536)
                output.extend(chunk)
            except OSError:
                break
            answer_queries(chunk)
            if pattern.search(output):
                break
        return bytes(output), time.monotonic() - started

    try:
        startup = drain(1.0)
        if not re.search(
            rb"\x1b\]777;kettle-completion;4;sync;[0-9]+;3\x07", startup
        ):
            raise RuntimeError(
                "Fish prompt did not advertise Kettle-owned Tab and Shift-Tab: "
                f"{startup[-1024:]!r}"
            )
        os.write(
            master,
            b"function __kettle_probe_keep; "
            b"printf '\\nKETTLE_AMBIGUOUS_BUFFER=<%s>\\n' (commandline -b); "
            b"end; bind \\cg __kettle_probe_keep\n",
        )
        startup += drain(0.5)
        syncs_before_clear = re.findall(
            rb"\x1b\]777;kettle-completion;4;sync;([0-9]+);3\x07", startup
        )
        if not syncs_before_clear:
            raise RuntimeError("Fish setup prompt did not establish a managed session")
        session_before_clear = syncs_before_clear[-1]
        os.write(master, b"\x0c")
        after_clear = drain(0.5)
        os.write(master, b"git ch\t")
        after_tab, initial_elapsed = drain_until_completion(2.0)
        # The private OSC is emitted before Fish has finished its complete key
        # binding. Drain a short quiet window as well; otherwise a pager drawn
        # immediately after the metadata escapes this assertion.
        after_tab += drain(0.25)
        sequences = re.findall(
            rb"\x1b\]777;kettle-completion;[^\x07]*\x07", startup + after_tab
        )
        if (
            not sequences
            or b";show;" not in sequences[-1]
            or b";fish;ch;git%20ch;" not in sequences[-1]
            or b";checkout;" not in sequences[-1]
        ):
            raise RuntimeError(
                "Fish Tab did not leave its completion list visible: "
                f"{sequences[-3:]!r}; output tail={(startup + after_tab)[-512:]!r}"
            )
        show_fields = sequences[-1][2:-1].split(b";")
        if len(show_fields) < 5 or show_fields[4] != session_before_clear:
            raise RuntimeError(
                "Fish Ctrl-L changed the prompt without preserving the managed session: "
                f"before={session_before_clear!r}, show={sequences[-1]!r}, "
                f"clear_tail={after_clear[-512:]!r}"
            )
        if initial_elapsed > 0.75:
            raise RuntimeError(
                f"Fish Tab took {initial_elapsed:.2f}s to publish completions"
            )
        shell_output = re.sub(
            rb"\x1b\]777;kettle-completion;[^\x07]*\x07", b"", after_tab
        )
        if b"checkout" in shell_output:
            raise RuntimeError(
                "Fish drew its pager as well as Kettle's detached list: "
                f"{shell_output[-512:]!r}"
            )
        os.write(master, b"\x07")
        ambiguous_buffer = drain(0.5)
        if b"KETTLE_AMBIGUOUS_BUFFER=<git ch>" not in ambiguous_buffer:
            raise RuntimeError(
                "Fish's first ambiguous Tab edited the command line instead of staying detached: "
                f"{ambiguous_buffer[-1024:]!r}"
            )

        os.write(master, b"\x1b[Z")
        reverse = re.findall(
            rb"\x1b\]777;kettle-completion;[^\x07]*\x07", drain(0.5)
        )
        reverse_fields = reverse[-1][2:-1].split(b";") if reverse else []
        version = reverse_fields[2] if len(reverse_fields) > 2 else b""
        reverse_header = {
            b"4": 14,
            b"3": 12,
            b"2": 10,
        }.get(version, 8)
        reverse_count = max(0, (len(reverse_fields) - reverse_header) // 2)
        selected_index = 8 if version in {b"3", b"4"} else 6
        selected_field = (
            reverse_fields[selected_index]
            if len(reverse_fields) > selected_index
            else b""
        )
        reverse_selected = (
            int(selected_field)
            if len(reverse_fields) >= reverse_header and selected_field.isdigit()
            else -1
        )
        if (
            not reverse
            or b";update;" not in reverse[-1]
            or b";fish;ch;git%20ch;" not in reverse[-1]
            or reverse_count < 2
            or reverse_selected != reverse_count - 1
        ):
            raise RuntimeError(
                "Fish Shift-Tab did not start at the final candidate: "
                f"{reverse[-3:]!r}"
            )

        # Exercise a real Fish editor buffer as well as the deterministic mock
        # above. An ordinary unique completion ends with one delimiter, while
        # Kettle's detached metadata remains the only list Fish publishes.
        os.write(master, b"\x03")
        drain(0.25)
        os.write(
            master,
            b"function __kettle_probe_buffer; "
            b"printf '\\nKETTLE_SINGLETON_BUFFER=<%s>\\n' (commandline -b); "
            b"commandline -r ''; end; "
            b"function kettle-singleton; end; "
            b"bind \\cg __kettle_probe_buffer; "
            b"complete -c kettle-singleton -f -a ordinary\n",
        )
        drain(0.5)
        os.write(master, b"kettle-singleton o\t")
        singleton_output, _ = drain_until_completion(2.0)
        os.write(master, b"\x07")
        singleton_output += drain(0.5)
        if b"KETTLE_SINGLETON_BUFFER=<kettle-singleton ordinary >" not in singleton_output:
            raise RuntimeError(
                "Fish's real editor lost the ordinary singleton delimiter: "
                f"{singleton_output[-1024:]!r}"
            )
        if b";show;" not in singleton_output:
            raise RuntimeError(
                "Fish's real singleton edit did not publish the detached card: "
                f"{singleton_output[-1024:]!r}"
            )

        # A leading-dash candidate is data, not an `abbr --query` option.
        # Without the explicit option terminator, `--help` exits successfully
        # and incorrectly suppresses Fish's ordinary singleton delimiter.
        os.write(
            master,
            b"function kettle-option; end; "
            b"complete -c kettle-option -f -a --help\n",
        )
        drain(0.25)
        os.write(master, b"kettle-option --h\t")
        option_output, _ = drain_until_completion(2.0)
        os.write(master, b"\x07")
        option_output += drain(0.5)
        if b"KETTLE_SINGLETON_BUFFER=<kettle-option --help >" not in option_output:
            raise RuntimeError(
                "Fish treated a leading-dash completion as an abbr option: "
                f"{option_output[-1024:]!r}"
            )
        if b";show;" not in option_output or b"string length:" in option_output:
            raise RuntimeError(
                "Fish did not publish a leading-dash completion cleanly: "
                f"{option_output[-1024:]!r}"
            )

        os.write(master, b"\x03")
        drain(0.25)
        os.write(
            master,
            b"function kettle-options; end; "
            b"complete -c kettle-options -f -a '--help --hidden'\n",
        )
        drain(0.25)
        os.write(master, b"kettle-options --h\t")
        option_list, _ = drain_until_completion(2.0)
        option_list += drain(0.25)
        option_sequences = re.findall(
            rb"\x1b\]777;kettle-completion;[^\x07]*\x07", option_list
        )
        if (
            not option_sequences
            or b";show;" not in option_sequences[-1]
            or b";--help;" not in option_sequences[-1]
            or b";--hidden;" not in option_sequences[-1]
            or b"string length:" in option_list
        ):
            raise RuntimeError(
                "Fish did not publish an ambiguous leading-dash completion list: "
                f"{option_list[-1024:]!r}"
            )

        # Fish marks ~user completions DONT_ESCAPE_TILDES + NO_SPACE. Its
        # public provider output hides those flags, so this live editor check
        # catches both quoting the tilde and adding a delimiter.
        user = getpass.getuser()
        prefix_len = min(2, len(user))
        os.write(master, b"\x03")
        drain(0.25)
        os.write(master, f"echo ~{user[:prefix_len]}\t".encode())
        user_output, _ = drain_until_completion(2.0)
        os.write(master, b"\x07")
        user_output += drain(0.5)
        expected_user = f"KETTLE_SINGLETON_BUFFER=<echo ~{user}>".encode()
        if expected_user not in user_output:
            raise RuntimeError(
                "Fish's ~user singleton lost its expandable native spelling: "
                f"{user_output[-1024:]!r}"
            )

        os.write(master, b"\x03")
        drain(0.25)
        os.write(master, b"fish_vi_key_bindings; source " + str(INTEGRATION / "kettle.fish").encode() + b"\n")
        vi_setup = drain(0.75)
        if not re.search(
            rb"\x1b\]777;kettle-completion;4;sync;[0-9]+;3\x07", vi_setup
        ):
            raise RuntimeError(
                "Fish Vi insert mode did not advertise its owned completion keys: "
                f"{vi_setup[-1024:]!r}"
            )
        os.write(
            master,
            b"bind -M custom x 'set -g fish_bind_mode insert'; "
            b"set -g fish_bind_mode custom\n",
        )
        custom_mode = drain(0.75)
        if not re.search(
            rb"\x1b\]777;kettle-completion;4;keymap;[0-9]+;0\x07", custom_mode
        ):
            raise RuntimeError(
                "Fish custom mode did not withdraw Kettle's completion keys: "
                f"{custom_mode[-1024:]!r}"
            )
        os.write(master, b"x")
        insert_mode = drain(0.4)
        if not re.search(
            rb"\x1b\]777;kettle-completion;4;keymap;[0-9]+;3\x07", insert_mode
        ):
            raise RuntimeError(
                "Fish Vi insert mode did not restore Kettle's completion keys: "
                f"{insert_mode[-1024:]!r}"
            )
        os.write(master, b"git ch\t")
        vi_output, vi_elapsed = drain_until_completion(2.0)
        vi_sequences = re.findall(
            rb"\x1b\]777;kettle-completion;[^\x07]*\x07", vi_output
        )
        if not vi_sequences or b";show;" not in vi_sequences[-1]:
            raise RuntimeError(
                "Fish Vi insert-mode Tab did not publish completions: "
                f"{vi_sequences[-3:]!r}; output tail={vi_output[-512:]!r}"
            )
        if vi_elapsed > 0.75:
            raise RuntimeError(
                f"Fish Vi insert-mode Tab took {vi_elapsed:.2f}s to publish completions"
            )

        os.write(master, b"\x1b[D")
        drain(0.2)
        os.write(master, b"\t")
        moved_output, moved_elapsed = drain_until_completion(2.0)
        moved_cursor = re.findall(
            rb"\x1b\]777;kettle-completion;[^\x07]*\x07", moved_output
        )
        if not moved_cursor or b";show;" not in moved_cursor[-1]:
            raise RuntimeError(
                "Fish reused a completion cycle after the cursor moved: "
                f"{moved_cursor[-3:]!r}; output tail={moved_output[-512:]!r}"
            )
        if moved_elapsed > 0.75:
            raise RuntimeError(
                f"Fish Tab after a cursor move took {moved_elapsed:.2f}s"
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
            try:
                process.wait(timeout=2)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=2)
        os.close(master)
    print(
        "Fish interactive completion binding fixture: PASS "
        f"(default {initial_elapsed:.2f}s, Vi {vi_elapsed:.2f}s, "
        f"moved cursor {moved_elapsed:.2f}s)"
    )


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
    syncs = re.findall(
        rb"\x1b\]777;kettle-completion;4;sync;([0-9]+);([0-3])\x07",
        prompt_output,
    )
    if not syncs or any(int(session) <= 0 for session, _keys in syncs):
        raise RuntimeError(
            "PowerShell prompt omitted a valid v4 completion-session sync: "
            f"{prompt_output!r}"
        )

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
    pages = re.findall(
        rb"\x1b\]777;kettle-completion;[^\x07]*\x07", enabled_output
    )
    expected_update = [
        b"777",
        b"kettle-completion",
        b"4",
        b"update",
        b"12",
        b"1",
        b"7",
        b"completion",
        b"1",
        b"powershell",
        b"",
        b"",
        b"64",
        b"66",
        b"item-65",
        b"description-65",
        b"item-66",
        b"description-66",
    ]
    page_fields = [page[2:-1].split(b";") for page in pages]
    if expected_update not in page_fields:
        raise RuntimeError(
            "PowerShell did not publish a structurally valid request-numbered second page: "
            f"{page_fields!r}"
        )
    lifecycle = {
        (fields[3], fields[4], fields[6])
        for fields in page_fields
        if len(fields) >= 7 and fields[2] == b"4" and fields[3] in {b"show", b"update"}
    }
    if (b"show", b"21", b"1") not in lifecycle or (
        b"update",
        b"21",
        b"2",
    ) not in lifecycle:
        raise RuntimeError(
            "PowerShell real-cycle mock did not advance and publish requests 1 then 2: "
            f"{page_fields!r}"
        )


def main() -> int:
    system = platform.system()
    check_no_inline_completion_fallbacks()

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
