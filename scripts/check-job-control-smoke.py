#!/usr/bin/env python3
"""Exercise Unix suspend/resume through Kettle's real PTY and key encoder.

The default client is an offline fixture. --codex explicitly opts into a real
client and its local authentication/configuration; it never submits a prompt.
"""

from __future__ import annotations

import argparse
import contextlib
import importlib.util
import json
import os
import platform
import re
import shlex
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from pathlib import Path


class KeyboardRegression(RuntimeError):
    """A client left the shell with the wrong key encoding."""

    def __init__(self, label: str, step: str, evidence: str = ""):
        self.label = label
        self.step = step
        self.evidence = evidence
        match = re.fullmatch(r"(suspend|background) (\d+)", label)
        self.phase = match.group(1) if match else None
        self.cycle = int(match.group(2)) if match else None
        detail = f"{label}: {step}"
        if evidence:
            detail += f" ({evidence})"
        super().__init__(detail)


def leaked_key_report_tail(screen_text: str, probe: str) -> str | None:
    prefix = f"JOB> {probe} beta"
    encoded_reports = ("\x1b[127;5u", "\x1b[127;3u")
    for line in reversed(screen_text.splitlines()):
        if not line.startswith(prefix):
            continue
        tail = line[len(prefix) :].rstrip()
        if len(tail) >= 3 and any(report.endswith(tail) for report in encoded_reports):
            return tail
    return None


def is_leaked_key_report_tail(evidence: str) -> bool:
    return len(evidence) >= 3 and any(
        report.endswith(evidence) for report in ("\x1b[127;5u", "\x1b[127;3u")
    )


def background_job_stopped_evidence(screen_text: str) -> str | None:
    lines = screen_text.splitlines()
    for index, line in enumerate(lines):
        if re.match(r"^\[\d+\]\+\s+Stopped\b", line):
            context = lines[index : index + 3]
            if "--broken-background" in "".join(row.strip() for row in context):
                return "\n".join(row.strip() for row in context).rstrip()
    return None


def expected_negative_control_failure(phase: str, error: RuntimeError) -> bool:
    return (
        isinstance(error, KeyboardRegression)
        and error.phase == phase
        and error.cycle == 0
        and (
            error.step == "ctrl+backspace"
            or (
                phase == "background"
                and (
                    error.step == "shell input"
                    and is_leaked_key_report_tail(error.evidence)
                    or error.step == "background process stopped"
                    and background_job_stopped_evidence(error.evidence) == error.evidence
                )
            )
        )
    )


def run_negative_controls(exercise):
    detections = []
    for phase in ("suspend", "background"):
        try:
            exercise(phase)
        except RuntimeError as error:
            if not expected_negative_control_failure(phase, error):
                raise
            detections.append((phase, error))
        else:
            raise RuntimeError(f"broken {phase} client was accepted")
    return detections


def check_negative_control_failure_classification() -> None:
    if leaked_key_report_tail("JOB> EDIT3alpha beta7;5u", "EDIT3alpha") != "7;5u":
        raise AssertionError("failed to identify the leaked enhanced key report")
    if leaked_key_report_tail("JOB> EDIT3alpha beta", "EDIT3alpha") is not None:
        raise AssertionError("accepted shell input without a leaked key report")
    stopped = background_job_stopped_evidence(
        "JOB> bg\n[1]+  Stopped /tmp/client\n-smoke.py --broken-back\n"
        "ground\nJOB>"
    )
    if stopped != (
        "[1]+  Stopped /tmp/client\n-smoke.py --broken-back\nground"
    ):
        raise AssertionError("failed to identify a stopped background control")
    if background_job_stopped_evidence(
        "JOB> bg\n[1]+  Stopped /tmp/unrelated-command\nJOB>"
    ) is not None:
        raise AssertionError("accepted an unrelated stopped background process")

    accepted = {
        "suspend": KeyboardRegression("suspend 0", "ctrl+backspace"),
        "background": KeyboardRegression("background 0", "shell input", "7;5u"),
    }

    def exercise(phase: str) -> None:
        raise accepted[phase]

    detections = run_negative_controls(exercise)
    if [phase for phase, _ in detections] != ["suspend", "background"]:
        raise AssertionError("negative controls did not classify both expected failures")
    if not expected_negative_control_failure(
        "background",
        KeyboardRegression("background 0", "background process stopped", stopped),
    ):
        raise AssertionError("failed to classify a stopped background control")
    rejected = (
        ("background", KeyboardRegression("background 1", "ctrl+backspace")),
        ("suspend", KeyboardRegression("suspend 0", "shell input", "7;5u")),
        ("background", KeyboardRegression("background 0", "shell input", "oops")),
        (
            "background",
            KeyboardRegression("background 0", "background process stopped", "stopped"),
        ),
        ("background", RuntimeError("timed out waiting for background 0: shell input")),
    )
    if any(expected_negative_control_failure(phase, error) for phase, error in rejected):
        raise AssertionError("negative control accepted an unrelated failure")

    def unrelated_timeout(_phase: str) -> None:
        raise RuntimeError("timed out waiting for background 0: shell input")

    try:
        run_negative_controls(unrelated_timeout)
    except RuntimeError as error:
        if str(error) != "timed out waiting for background 0: shell input":
            raise
    else:
        raise AssertionError("negative control accepted an unrelated timeout")


def fixture(
    state: Path, alternate: bool, broken: bool, broken_background: bool
) -> None:
    import termios
    import tty

    saved = termios.tcgetattr(0)
    if broken_background:
        signal.signal(signal.SIGTTOU, signal.SIG_IGN)
    signal.signal(signal.SIGTSTP, signal.SIG_DFL)
    generation = 0
    received = ""

    def write(data: bytes) -> None:
        sys.stdout.buffer.write(data)
        sys.stdout.buffer.flush()

    def publish() -> None:
        temporary = state.with_suffix(".tmp")
        temporary.write_text(
            json.dumps({"generation": generation, "received": received})
        )
        temporary.replace(state)

    def enter() -> None:
        tty.setraw(0)
        if alternate:
            write(b"\x1b[?1049h")
        write(b"\x1b[>1u\r\nJOB_CLIENT_READY\r\n")

    def leave() -> None:
        write(b"\x1b[<u")
        if alternate:
            write(b"\x1b[?1049l")
        termios.tcsetattr(0, termios.TCSANOW, saved)

    enter()
    publish()
    pending = b""
    try:
        while True:
            chunk = os.read(0, 256)
            if not chunk:
                return
            pending += chunk
            if len(pending) > 4096:
                raise RuntimeError("fixture input exceeded its bound")
            # CSI-u is the only enhanced format the fixture negotiates.
            while pending:
                if pending.startswith(b"\x1b["):
                    match = re.match(rb"\x1b\[[0-9;:]+[u~A-D]", pending)
                    if match is None:
                        break
                    key = match.group()
                elif pending == b"\x1b":
                    break
                else:
                    key = pending[:1]
                pending = pending[len(key) :]
                if key == b"\x1b[113;5u":
                    return
                if key == b"\x1b[122;5u":
                    leave()
                    if broken:
                        enter()
                    signal.raise_signal(signal.SIGTSTP)
                    while not broken_background and os.tcgetpgrp(0) != os.getpgrp():
                        signal.raise_signal(signal.SIGSTOP)
                    if not broken:
                        enter()
                    generation += 1
                    received = ""
                else:
                    received = (received + key.hex())[-1024:]
                publish()
    finally:
        leave()


def load_live_helpers():
    path = Path(__file__).with_name("check-live-ui-smoke.py")
    spec = importlib.util.spec_from_file_location("kettle_job_control_live", path)
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def wait_until(check, label: str, timeout: float = 10):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = check()
        if value:
            return value
        time.sleep(0.05)
    raise RuntimeError(f"timed out waiting for {label}")


@contextlib.contextmanager
def failure_evidence(live, out: Path):
    try:
        yield
    except BaseException:
        try:
            result = live.ctl("read_screen", raw=True, timeout=2)
            (out / "failure-screen.json").write_text(result.stdout)
        except (OSError, RuntimeError, SystemExit, subprocess.TimeoutExpired) as error:
            print(f"could not save failure screen: {error}", file=sys.stderr)
        raise


def run(args) -> Path:
    helpers = load_live_helpers()
    kettle = shutil.which(args.kettle)
    shell = shutil.which(args.shell)
    if kettle is None or shell is None:
        raise RuntimeError("Kettle and the requested shell must be installed")
    root = Path(args.out_dir).resolve() if args.out_dir else Path(tempfile.gettempdir())
    root.mkdir(parents=True, exist_ok=True)
    out = Path(tempfile.mkdtemp(prefix="kettle-job-control-", dir=root))
    print(f"job-control smoke: artifacts={out}", flush=True)
    cfg = out / "config"
    cfg.write_text(
        "restore-session = false\nupdate-check = false\nrecord = off\n"
        f"window-width = {args.columns}\nwindow-height = {args.rows}\nfont-size = 14\n"
    )
    shell_args = [shell, "-i"]
    if not args.configured_shell:
        shell_args = (
            [shell, "-f", "-i"]
            if args.shell == "zsh"
            else [shell, "--noprofile", "--norc", "-i"]
        )
    launch = ["--working-directory", str(Path.cwd())]
    if args.hidden:
        launch.append("--hidden")
    if args.maximized:
        launch.append("--maximize")
    launch += ["-e", *shell_args]
    evidence = {
        "client": args.codex or "offline fixture",
        "codex_no_daemon": args.codex_no_daemon,
        "codex_yolo": args.codex_yolo,
        "shell": shell_args,
        "os": platform.platform(),
        "cycles": args.cycles,
        "configured_shell": args.configured_shell,
        "alternate": args.alternate,
        "background": args.background,
        "split": args.split,
        "hidden": args.hidden,
        "maximized": args.maximized,
        "external_editor": args.external_editor,
        "transcript": args.transcript,
        "startup_cells": [args.columns, args.rows],
        "kettle_version": helpers.run([kettle, "--version"]).stdout.strip(),
    }
    client = shutil.which(args.codex) if args.codex else None
    if args.codex:
        if client is None:
            raise RuntimeError("requested Codex binary does not exist")
        evidence["codex_version"] = helpers.run([client, "--version"]).stdout.strip()
    (out / "evidence.json").write_text(json.dumps(evidence, indent=2) + "\n")
    with (
        helpers.LiveKettle(kettle, cfg, out / "kettle.log", extra_args=launch) as live,
        failure_evidence(live, out),
    ):

        def text(value: str):
            live.ctl("send_text", params={"text": value})

        def keys(*values: str):
            live.ctl("send_keys", params={"keys": list(values)})

        def screen() -> str:
            return helpers.screen_text(live.json_ctl("read_screen")).rstrip()

        def command(value: str):
            text(value)
            keys("enter")

        # The shell must have entered its editor before input is queued.
        wait_until(lambda: screen().strip(), "initial shell prompt")
        keys("ctrl+c")
        time.sleep(0.2)
        if args.shell == "zsh":
            setup = "unset HISTFILE; PROMPT='JOB> '; bindkey -e; bindkey '^H' backward-kill-word; bindkey '^[^?' backward-kill-word"
        else:
            setup = "HISTFILE=/dev/null; PS1='JOB> '; bind '\"\\C-h\": backward-kill-word'; bind '\"\\e\\C-?\": backward-kill-word'"
        command(setup)
        wait_until(lambda: screen().endswith("JOB>"), "controlled shell prompt")

        edit_generation = 0

        def editing(label: str):
            nonlocal edit_generation
            for chord in ("ctrl+backspace", "alt+backspace"):
                edit_generation += 1
                probe = f"EDIT{edit_generation}alpha"
                text(probe + " beta")
                try:
                    wait_until(
                        lambda probe=probe: screen().endswith(f"JOB> {probe} beta"),
                        f"{label}: shell input",
                    )
                except RuntimeError as error:
                    screen_text = screen()
                    (out / "failure-screen.txt").write_text(screen_text)
                    tail = leaked_key_report_tail(screen_text, probe)
                    if (
                        args.broken_background
                        and label == "background 0"
                        and tail is not None
                    ):
                        raise KeyboardRegression(label, "shell input", tail) from error
                    stopped = background_job_stopped_evidence(screen_text)
                    if (
                        args.broken_background
                        and label == "background 0"
                        and stopped is not None
                    ):
                        raise KeyboardRegression(
                            label, "background process stopped", stopped
                        ) from error
                    raise
                keys(chord)
                try:
                    wait_until(
                        lambda probe=probe: screen().endswith(f"JOB> {probe}"),
                        f"{label}: {chord}",
                        3,
                    )
                except RuntimeError as error:
                    (out / "failure-screen.txt").write_text(screen())
                    raise KeyboardRegression(label, chord) from error
                keys("ctrl+c")
                wait_until(lambda: screen().endswith("JOB>"), "Ctrl+C at shell prompt")
            text("abc")
            wait_until(lambda: screen().endswith("JOB> abc"), "ordinary shell input")
            keys("backspace", "left", "right")
            wait_until(lambda: screen().endswith("JOB> ab"), "ordinary editing")
            keys("ctrl+c")
            wait_until(lambda: screen().endswith("JOB>"), "clean shell prompt")

        editing("before launch")
        if args.split:
            live.ctl("perform_action", params={"action": "split_right"})
            # Return input ownership to the original pane; the sibling stays idle.
            wait_until(
                lambda: len(live.json_ctl("list_panes")["panes"]) == 2, "split layout"
            )
            live.ctl("perform_action", params={"action": "go_left"})
            wait_until(lambda: screen().endswith("JOB>"), "original pane focus")
        state = out / "client-state.json"
        if args.codex:
            argv = [client]
            if args.codex_yolo:
                argv.append("--dangerously-bypass-approvals-and-sandbox")
            if args.codex_no_daemon:
                argv.append("--no-daemon")
            if not args.alternate:
                argv.append("--no-alt-screen")
            if args.external_editor:
                editor = out / "editor"
                editor.write_text(
                    f"#!{sys.executable}\n"
                    "from pathlib import Path\nimport sys\n"
                    "Path(sys.argv[-1]).write_text('EDITOR_PROBE')\n"
                )
                editor.chmod(0o700)
                argv = ["env", f"EDITOR={editor}", f"VISUAL={editor}", *argv]
        else:
            argv = [
                sys.executable,
                str(Path(__file__).resolve()),
                "--fixture",
                "--state",
                str(state),
            ]
            if args.alternate:
                argv.append("--alternate")
            if args.broken_fixture:
                argv.append("--broken-fixture")
            if args.broken_background:
                argv.append("--broken-background")
        command(shlex.join(argv))

        def ready(generation: int):
            if args.codex:
                # Never confirm trust dialogs or submit model work automatically.
                wait_until(
                    lambda: (
                        re.search(r"Ask Codex|context left|/model to change", screen())
                        and not re.search(r"(?:model|directory):\s+loading", screen())
                    ),
                    "Codex composer",
                    45,
                )
                probe = f"EDITPROBE{generation}"
                text(probe + " word")
                # A shell can echo and edit the same text after a client crash.
                # Require each unique probe on a Codex composer line.
                wait_until(
                    lambda: re.search(rf"(?m)^\u203a {probe} word[ \t]*$", screen()),
                    "live Codex composer input",
                )
                keys("ctrl+backspace")
                wait_until(
                    lambda: re.search(rf"(?m)^\u203a {probe}[ \t]*$", screen()),
                    "Codex word deletion",
                )
                keys("ctrl+u")
                wait_until(lambda: probe not in screen(), "Codex composer clear")
                return
            return wait_until(
                lambda: (
                    state.exists()
                    and json.loads(state.read_text())["generation"] == generation
                ),
                "fixture composer",
            )

        ready(0)
        if args.external_editor:
            keys("ctrl+g")
            wait_until(
                lambda: "EDITOR_PROBE" in screen(), "external editor draft handoff"
            )
            keys("ctrl+u")
            wait_until(lambda: "EDITOR_PROBE" not in screen(), "clear editor draft")
        for cycle in range(args.cycles):
            if not args.codex:
                keys("ctrl+backspace", "alt+backspace")
                expected = b"\x1b[127;5u\x1b[127;3u".hex()
                wait_until(
                    lambda expected=expected: json.loads(state.read_text())[
                        "received"
                    ].endswith(expected),
                    "enhanced client keys",
                )
            if args.transcript:
                keys("ctrl+t")
                time.sleep(0.2)
            keys("ctrl+z")
            wait_until(
                lambda: screen().rsplit("\n", 1)[-1].startswith("JOB>"),
                "suspended shell prompt",
            )
            editing(f"suspend {cycle}")
            if args.background:
                command("bg")
                time.sleep(0.3)
                # Ask the shell for a fresh prompt after its asynchronous job notice.
                keys("enter")
                wait_until(lambda: screen().endswith("JOB>"), "shell after bg")
                editing(f"background {cycle}")
            command("fg")
            # Resume probes and input draining precede composer readiness. Old
            # scrollback can already contain the readiness text during that gap.
            time.sleep(0.5)
            if args.transcript:
                keys("escape")
            ready(cycle + 1)
            # The resume path must finish before another Ctrl+Z is sent.
            time.sleep(0.1)
        if args.codex:
            keys("ctrl+c")
            time.sleep(0.5)
            keys("ctrl+c")
        else:
            keys("ctrl+q")
        wait_until(lambda: screen().endswith("JOB>"), "normal client exit")
        editing("after exit")
        command("printf '%s%s\\n' JOB_CONTROL_ OK")
        live.wait_for_text("JOB_CONTROL_OK")
        evidence["geometry"] = live.json_ctl("ui_geometry")
        evidence["panes"] = live.json_ctl("list_panes")
    (out / "evidence.json").write_text(json.dumps(evidence, indent=2) + "\n")
    return out


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--kettle", default=os.environ.get("KETTLE_BIN", "kettle"))
    parser.add_argument("--codex", help="opt in to this real Codex executable")
    parser.add_argument(
        "--codex-yolo",
        action="store_true",
        help="explicitly disable Codex approvals and sandboxing to reproduce codex-yolo",
    )
    parser.add_argument(
        "--codex-no-daemon",
        action="store_true",
        help="pass --no-daemon to a standalone Codex development build",
    )
    parser.add_argument("--shell", choices=("zsh", "bash"), default="bash")
    parser.add_argument("--configured-shell", action="store_true")
    parser.add_argument("--cycles", type=int, default=5)
    parser.add_argument("--columns", type=int, default=110)
    parser.add_argument("--rows", type=int, default=32)
    parser.add_argument("--out-dir")
    parser.add_argument("--hidden", action="store_true")
    parser.add_argument("--maximized", action="store_true")
    parser.add_argument("--split", action="store_true")
    parser.add_argument("--alternate", action="store_true")
    parser.add_argument("--background", action="store_true")
    parser.add_argument(
        "--negative-controls",
        action="store_true",
        help="require detection of deliberately broken suspend and background clients",
    )
    parser.add_argument(
        "--external-editor",
        action="store_true",
        help="exercise Codex's editor handoff with a disposable editor fixture",
    )
    parser.add_argument(
        "--transcript",
        action="store_true",
        help="suspend Codex from its transcript overlay",
    )
    parser.add_argument("--fixture", action="store_true", help=argparse.SUPPRESS)
    parser.add_argument("--state", type=Path, help=argparse.SUPPRESS)
    parser.add_argument("--broken-fixture", action="store_true", help=argparse.SUPPRESS)
    parser.add_argument(
        "--broken-background", action="store_true", help=argparse.SUPPRESS
    )
    args = parser.parse_args()
    if os.name != "posix":
        parser.error("Unix job control requires Linux or macOS")
    if not 1 <= args.cycles <= 1000:
        parser.error("--cycles must be between 1 and 1000")
    if args.hidden and args.maximized:
        parser.error("--maximized requires a visible window")
    if (
        args.external_editor
        or args.transcript
        or args.codex_no_daemon
        or args.codex_yolo
    ) and not args.codex:
        parser.error("Codex-specific options require --codex")
    if args.fixture:
        if args.state is None:
            parser.error("--fixture requires --state")
        fixture(args.state, args.alternate, args.broken_fixture, args.broken_background)
        return 0
    if args.negative_controls:
        if args.codex:
            parser.error("negative controls use only the offline fixture")
        check_negative_control_failure_classification()
        args.cycles = 1
        args.background = True
        for phase in ("suspend", "background"):
            args.broken_fixture = phase == "suspend"
            args.broken_background = phase == "background"
            try:
                run(args)
            except RuntimeError as error:
                if not expected_negative_control_failure(phase, error):
                    raise
                print(f"negative control: detected broken {phase} client ({error})")
            else:
                raise RuntimeError(f"broken {phase} client was accepted")
        return 0
    result = run(args)
    print(f"job-control smoke: OK artifacts={result}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
