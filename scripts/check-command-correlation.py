#!/usr/bin/env python3
"""Exercise command correlation in private native shells inside real Kettle windows.

Requires a macOS or Linux desktop and at least one installed supported shell.
The output directory must be new. Fixtures use private startup/history files.
"""

from __future__ import annotations

import argparse
from concurrent.futures import ThreadPoolExecutor
from contextlib import contextmanager
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import time


@contextmanager
def failure_evidence(app, root, report):
    """Capture the failed live fixture before its context manager closes it."""
    try:
        yield
    except (Exception, SystemExit):
        diagnostics = report.setdefault("failure_evidence", {})
        for method in ["read_screen", "get_state"]:
            try:
                cp = app.ctl(method, raw=True, allow_fail=True, timeout=3)
                (root / ("failure-" + method + ".json")).write_text(cp.stdout)
                diagnostics[method] = {"exit": cp.returncode, "stderr": cp.stderr}
            except (Exception, SystemExit) as error:
                diagnostics[method] = {"error": str(error)}
        try:
            app.screenshot(root / "failure.png")
            diagnostics["screenshot"] = True
        except (Exception, SystemExit) as error:
            diagnostics["screenshot"] = {"error": str(error)}
        raise


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--kettle", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument(
        "--shell",
        action="append",
        choices=["bash", "bash32", "zsh", "fish", "powershell"],
    )
    args = parser.parse_args()
    if os.name == "nt":
        parser.error(
            "this desktop harness requires macOS or Linux; use the native PowerShell fixtures on Windows"
        )
    repo = Path(__file__).resolve().parent.parent
    out = args.output.resolve()
    out.mkdir(mode=0o700, parents=True, exist_ok=False)
    spec = importlib.util.spec_from_file_location(
        "correlation_live", repo / "scripts/check-live-ui-smoke.py"
    )
    assert spec is not None and spec.loader is not None
    live = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = live
    spec.loader.exec_module(live)
    binary = out / "kettle"
    shutil.copyfile(args.kettle.resolve(), binary)
    binary.chmod(0o700)
    binary_hash = hashlib.sha256(binary.read_bytes()).hexdigest()
    reports = []
    cases = []
    if sys.platform == "darwin" and (not args.shell or "bash32" in args.shell):
        cases.append(("bash32", "/bin/bash"))
    for label, name in [
        ("bash", "bash"),
        ("zsh", "zsh"),
        ("fish", "fish"),
        ("powershell", "pwsh"),
    ]:
        if args.shell and label not in args.shell:
            continue
        shell = shutil.which(name)
        if shell is None:
            if args.shell:
                parser.error("requested shell is unavailable: " + label)
            print(label + ": SKIP (not installed)", flush=True)
            continue
        if label == "bash":
            version = subprocess.run(
                [shell, "--version"], capture_output=True, check=True, timeout=5
            ).stdout
            if b"version 3.2" in version:
                if any(kind == "bash32" for kind, _ in cases):
                    continue
                label = "bash32"
        cases.append((label, shell))
    if not cases:
        parser.error("no requested native shell is available")
    for label, shell in cases:
        root = out / label
        root.mkdir(mode=0o700)
        ready = root / "ready"
        setup = root / (
            "setup"
            + {
                "bash32": ".bash",
                "bash": ".bash",
                "zsh": ".zsh",
                "fish": ".fish",
                "powershell": ".ps1",
            }[label]
        )
        source = (
            repo
            / "shell-integration"
            / (
                "kettle."
                + {
                    "bash32": "bash",
                    "bash": "bash",
                    "zsh": "zsh",
                    "fish": "fish",
                    "powershell": "ps1",
                }[label]
            )
        )
        env = {
            "SHELL": shell,
            "ENV": None,
            "BASH_ENV": None,
            "HISTFILE": "/dev/null",
            "INPUTRC": "/dev/null",
            "fish_history": "",
        }
        if label.startswith("bash"):
            setup.write_text(
                "PS1='PROBE> '\nsource "
                + shlex.quote(str(source))
                + "\nprintf ready > "
                + shlex.quote(str(ready))
                + "\n"
            )
            argv = [shell, "--noprofile", "--rcfile", str(setup), "-i"]
            fail = "false"
            status = "printf 'STATUS=%s\\n' \"$?\"; audit_value='quoted value'; set -- 'argument space'"
            global_check = 'printf \'GLOBAL=%s;ARG=%s\\n\' "$audit_value" "$1"'
            multi = "audit_multiline='first line'\nprintf 'MULTI=%s\\n' \"$audit_multiline\""
            slow = "sleep 0.5; printf 'LATE_DONE\\n'"
            stale = "printf '\\033]133;C;kettle=ffffffffffffffffffffffffffffffff\\007\\033]133;D;66;kettle=ffffffffffffffffffffffffffffffff\\007\\033]133;D;77\\007'; sleep 0.3; printf 'TARGET_DONE\\n'; false"
            expected_status = "STATUS=1"
        elif label == "zsh":
            setup.write_text(
                "PROMPT='PROBE> '\nsource "
                + shlex.quote(str(source))
                + "\nprintf ready > "
                + shlex.quote(str(ready))
                + "\n"
            )
            zdir = root / "zdot"
            zdir.mkdir()
            (zdir / ".zshrc").write_text("source " + shlex.quote(str(setup)) + "\n")
            env["ZDOTDIR"] = str(zdir)
            argv = [shell, "-d", "-i"]
            fail = "false"
            status = "printf 'STATUS=%s\\n' \"$?\"; audit_value='quoted value'; set -- 'argument space'"
            global_check = 'printf \'GLOBAL=%s;ARG=%s\\n\' "$audit_value" "$1"'
            multi = "audit_multiline='first line'\nprintf 'MULTI=%s\\n' \"$audit_multiline\""
            slow = "sleep 0.5; printf 'LATE_DONE\\n'"
            stale = "printf '\\033]133;C;kettle=ffffffffffffffffffffffffffffffff\\007\\033]133;D;66;kettle=ffffffffffffffffffffffffffffffff\\007\\033]133;D;77\\007'; sleep 0.3; printf 'TARGET_DONE\\n'; false"
            expected_status = "STATUS=1"
        elif label == "fish":
            setup.write_text(
                "function fish_prompt; printf 'PROBE> '; end\nset -g fish_history ''\nsource "
                + shlex.quote(str(source))
                + "\nprintf ready > "
                + shlex.quote(str(ready))
                + "\n"
            )
            argv = [
                shell,
                "--no-config",
                "--interactive",
                "--init-command",
                "source " + shlex.quote(str(setup)),
            ]
            fail = "false"
            status = "printf 'STATUS=%s\\n' $status; set -g audit_value 'quoted value'"
            global_check = "printf 'GLOBAL=%s\\n' $audit_value"
            multi = "set -g audit_multiline 'first line'\nprintf 'MULTI=%s\\n' $audit_multiline"
            slow = "sleep 0.5; printf 'LATE_DONE\\n'"
            stale = "printf '\\033]133;C;kettle=ffffffffffffffffffffffffffffffff\\007\\033]133;D;66;kettle=ffffffffffffffffffffffffffffffff\\007\\033]133;D;77\\007'; sleep 0.3; printf 'TARGET_DONE\\n'; false"
            expected_status = "STATUS=1"
        else:

            def ps_literal(value):
                return "'" + str(value).replace("'", "''") + "'"

            setup.write_text(
                "Import-Module PSReadLine\nSet-PSReadLineOption -HistorySavePath "
                + ps_literal(root / "history.txt")
                + " -HistorySaveStyle SaveNothing -PredictionSource None -ErrorAction Stop\nfunction global:prompt { 'PROBE> ' }\n. "
                + ps_literal(source)
                + "\n[IO.File]::WriteAllText("
                + ps_literal(ready)
                + ",'ready')\n"
            )
            argv = [shell, "-NoLogo", "-NoProfile", "-NoExit", "-File", str(setup)]
            fail = "Write-Error 'fixture' -ErrorAction Ignore"
            status = "Write-Output ('STATUS=' + $?); $global:audit_value='quoted value'"
            global_check = "Write-Output ('GLOBAL=' + $global:audit_value)"
            multi = "$global:audit_multiline='first line'\nWrite-Output ('MULTI=' + $global:audit_multiline)"
            slow = "Start-Sleep -Milliseconds 500; Write-Output 'LATE_DONE'"
            stale = "[Console]::Write([char]27+']133;C;kettle=ffffffffffffffffffffffffffffffff'+[char]7+[char]27+']133;D;66;kettle=ffffffffffffffffffffffffffffffff'+[char]7+[char]27+']133;D;77'+[char]7); Start-Sleep -Milliseconds 300; Write-Output 'TARGET_DONE'; Write-Error 'fixture' -ErrorAction Ignore"
            expected_status = "STATUS=False"
        wrapper = root / "shell.sh"
        wrapper.write_text("#!/bin/sh\nexec " + shlex.join(argv) + "\n")
        wrapper.chmod(0o700)
        config = root / "kettle.conf"
        config.write_text(
            "agent-server = full\nrestore-session = false\nupdate-policy = off\nrecord = off\nlogin-shell = false\nshell-integration = false\nask-before-closing = never\ncursor-blink = false\nwindow-width = 100\nwindow-height = 25\nshell = "
            + str(wrapper)
            + "\n"
        )
        for name in ["CONFIG", "STATE", "DATA", "CACHE", "RUNTIME"]:
            path = root / name.lower()
            path.mkdir(mode=0o700)
            env["XDG_RUNTIME_DIR" if name == "RUNTIME" else "XDG_" + name + "_HOME"] = (
                str(path)
            )
        report = {
            "shell": shell,
            "label": label,
            "pass": False,
            "cleanup": False,
            "binary_sha256": binary_hash,
            "results": [],
        }
        reports.append(report)
        app = live.LiveKettle(str(binary), config, root / "kettle.log", extra_env=env)

        def wait(condition, seconds=15):
            until = time.monotonic() + seconds
            while time.monotonic() < until:
                if condition():
                    return
                time.sleep(0.05)
            raise RuntimeError(
                "native condition timed out at " + report.get("stage", "unknown")
            )

        def command(text, **params):
            report["stage"] = text
            admission_deadline = time.monotonic() + 5
            for admission in range(500):
                cp = app.ctl(
                    "run_command",
                    params={
                        "command": text,
                        "require_correlation": True,
                        "timeout_s": 3,
                        **params,
                    },
                    raw=True,
                    allow_fail=True,
                    timeout=10,
                )
                result = {
                    "command": text,
                    "returncode": cp.returncode,
                    "stdout": cp.stdout,
                    "stderr": cp.stderr,
                }
                report["results"].append(result)
                result["admission_attempt"] = admission + 1
                if cp.returncode and "[busy]" in cp.stderr:
                    if time.monotonic() >= admission_deadline:
                        raise RuntimeError("command was never admitted: " + cp.stderr)
                    result["refused_before_input"] = True
                    time.sleep(0.01)
                    continue
                if cp.returncode:
                    raise RuntimeError(cp.stderr)
                value = json.loads(cp.stdout)
                result["value"] = value
                return value
            raise RuntimeError("command was never admitted")

        try:
            with app, failure_evidence(app, root, report):
                report["pid"] = app.pid
                (root / "owner.json").write_text(
                    json.dumps({"pid": app.pid, "binary": str(binary), "shell": shell})
                    + "\n"
                )
                report["stage"] = "ready"
                wait(
                    lambda: (
                        ready.exists()
                        and "PROBE>" in app.json_ctl("read_screen").get("text", "")
                    )
                )
                time.sleep(0.1)
                assert app.json_ctl("get_state")["features"]["command_correlation"] == 1
                if label == "bash32":
                    refused = app.ctl(
                        "run_command",
                        params={
                            "command": "printf STRICT_SIDE_EFFECT > "
                            + shlex.quote(str(root / "must-not-exist")),
                            "require_correlation": True,
                        },
                        raw=True,
                        allow_fail=True,
                    )
                    assert (
                        refused.returncode != 0
                        and not (root / "must-not-exist").exists()
                    )
                    legacy = app.json_ctl(
                        "run_command",
                        {"command": "printf 'LEGACY_OK\\n'", "timeout_s": 3},
                    )
                    assert (
                        not legacy["timed_out"]
                        and legacy["exit_code"] == 0
                        and "LEGACY_OK" in legacy["output"]
                    ), legacy
                    report["legacy_fallback"] = True
                    report["strict_refusal_before_side_effect"] = True
                    report["pass"] = True
                else:
                    value = command(fail)
                    assert not value["timed_out"] and value["exit_code"] == 1, value
                    value = command(status)
                    assert expected_status in value["output"], value
                    value = command(global_check)
                    assert "GLOBAL=quoted value" in value["output"], value
                    sync_command = (
                        "[Console]::Write([char]27+'[?2026hSYNC_CAPTURE'+[char]10); Write-Error 'fixture' -ErrorAction Ignore"
                        if label == "powershell"
                        else "printf '\\033[?2026hSYNC_CAPTURE\\n'; false"
                    )
                    reset_command = (
                        "[Console]::Write([char]27+'[?2026lSYNC_RESET'+[char]10)"
                        if label == "powershell"
                        else "printf '\\033[?2026lSYNC_RESET\\n'"
                    )
                    try:
                        value = command(sync_command)
                        assert (
                            value["exit_code"] == 1
                            and "SYNC_CAPTURE" in value["output"]
                        ), value
                        report["synchronized_output"] = True
                    finally:
                        app.json_ctl("send_text", {"text": reset_command + "\r"})
                        wait(
                            lambda: (
                                "\nSYNC_RESET\n"
                                in app.json_ctl("read_screen").get("text", "")
                            )
                        )
                    if label == "zsh":
                        keymap_file = root / "keymap.txt"
                        command(
                            '__correlation_test_keymap() { printf "%s" "$KEYMAP" > '
                            + shlex.quote(str(keymap_file))
                            + "; }; zle -N __correlation_test_keymap; "
                            + "bindkey -M vicmd $'\\x18z' __correlation_test_keymap; "
                            + "bindkey -M viins $'\\x18z' __correlation_test_keymap; "
                            + "bindkey -v; KEYTIMEOUT=1"
                        )
                        app.json_ctl("send_keys", {"keys": ["escape"]})
                        time.sleep(0.05)
                        app.json_ctl("send_keys", {"keys": ["ctrl+x", "z"]})
                        wait(lambda: keymap_file.exists())
                        assert keymap_file.read_text() == "vicmd", (
                            keymap_file.read_text()
                        )
                        value = command(global_check)
                        assert (
                            value["exit_code"] == 0
                            and "GLOBAL=quoted value" in value["output"]
                        ), value
                        report["zsh_vicmd"] = True
                        command("bindkey -e")
                    if label == "fish":
                        mode_file = root / "fish-mode.txt"
                        observer = shlex.quote(
                            "printf '%s' $fish_bind_mode > "
                            + shlex.quote(str(mode_file))
                        )
                        command(
                            "fish_vi_key_bindings; set -g fish_escape_delay_ms 10; "
                            + "bind -M default \\cxz "
                            + observer
                            + "; bind -M insert \\cxz "
                            + observer
                        )
                        app.json_ctl("send_keys", {"keys": ["escape"]})
                        time.sleep(0.05)
                        app.json_ctl("send_keys", {"keys": ["ctrl+x", "z"]})
                        wait(lambda: mode_file.exists())
                        assert mode_file.read_text() == "default", mode_file.read_text()
                        value = command(global_check)
                        assert (
                            value["exit_code"] == 0
                            and "GLOBAL=quoted value" in value["output"]
                        ), value
                        report["fish_vi_normal"] = True
                        # Fish 3.7 --no-config lacks the mode-reset variable hook.
                        command("fish_default_key_bindings; set -g fish_bind_mode default")
                    # Keep raw typeahead below the platform input limit. Larger
                    # unpaced bursts can be dropped by the line discipline even
                    # without correlation, so they cannot identify this regression.
                    earlier = (
                        "Write-Output 'EARLIER_QUEUE'"
                        if label == "powershell"
                        else "printf 'EARLIER_QUEUE\\n'"
                    )
                    target = (
                        "Write-Output 'CORRELATED_TARGET_ONLY'; Write-Error 'fixture' -ErrorAction Ignore"
                        if label == "powershell"
                        else "printf 'CORRELATED_TARGET_ONLY\\n'; false"
                    )
                    report["queued_commands"] = []
                    for scenario, queued_command, expected_exit, expected_text in [
                        ("single line", target, 1, "CORRELATED_TARGET_ONLY"),
                        ("multiline", multi, 0, "MULTI=first line"),
                    ]:
                        report["stage"] = "queued " + scenario
                        app.json_ctl("send_text", {"text": (earlier + "\r") * 8})
                        admission_deadline = time.monotonic() + 5
                        for attempt in range(500):
                            queued = app.ctl(
                                "run_command",
                                params={
                                    "command": queued_command,
                                    "require_correlation": True,
                                    "timeout_s": 15,
                                },
                                raw=True,
                                allow_fail=True,
                                timeout=20,
                            )
                            if queued.returncode and "[busy]" in queued.stderr:
                                if time.monotonic() >= admission_deadline:
                                    raise RuntimeError(
                                        "queued command was never admitted"
                                    )
                                time.sleep(0.01)
                                continue
                            assert queued.returncode == 0, queued.stderr
                            value = json.loads(queued.stdout)
                            assert (
                                value["exit_code"] == expected_exit
                                and not value["timed_out"]
                            ), value
                            assert expected_text in value["output"], value
                            assert "EARLIER_QUEUE" not in value["output"], value
                            report["queued_commands"].append(
                                {
                                    "scenario": scenario,
                                    "earlier_lines": 8,
                                    "admission_attempts": attempt + 1,
                                    "result": value,
                                }
                            )
                            break
                        else:
                            raise RuntimeError("queued command was never admitted")
                    if label == "fish":
                        marker = root / "prompt-waiting"
                        setup_pause = (
                            "functions --copy __kettle_correlation_install __correlation_test_install; "
                            "function __kettle_correlation_install; printf waiting > "
                            + shlex.quote(str(marker))
                            + "; sleep 0.2; __correlation_test_install; end"
                        )
                        value = command(setup_pause)
                        assert value["exit_code"] == 0 and not value["timed_out"], value
                        app.json_ctl(
                            "send_text", {"text": "printf 'EARLIER_AUTO\\n'\r" * 8}
                        )
                        wait(lambda: marker.exists())
                        report["stage"] = "automatic with delayed next prompt"
                        admission_deadline = time.monotonic() + 5
                        for admission in range(500):
                            automatic = app.ctl(
                                "run_command",
                                params={"command": target, "timeout_s": 15},
                                raw=True,
                                allow_fail=True,
                                timeout=20,
                            )
                            if automatic.returncode and "[busy]" in automatic.stderr:
                                if time.monotonic() >= admission_deadline:
                                    raise RuntimeError(
                                        "automatic request was never admitted"
                                    )
                                time.sleep(0.01)
                                continue
                            assert automatic.returncode == 0, automatic.stderr
                            value = json.loads(automatic.stdout)
                            assert (
                                value["exit_code"] == 1
                                and "CORRELATED_TARGET_ONLY" in value["output"]
                                and "EARLIER_AUTO" not in value["output"]
                            ), value
                            report["automatic_correlation"] = {
                                "result": value,
                                "admission_attempts": admission + 1,
                            }
                            break
                        else:
                            raise RuntimeError("automatic request was never admitted")
                        value = command(
                            "functions -e __kettle_correlation_install; "
                            "functions --copy __correlation_test_install __kettle_correlation_install; "
                            "functions -e __correlation_test_install"
                        )
                        assert value["exit_code"] == 0 and not value["timed_out"], value
                    app.json_ctl("perform_action", {"action": "toggle_read_only"})
                    try:
                        locked = app.ctl(
                            "run_command",
                            params={
                                "command": target,
                                "require_correlation": True,
                            },
                            raw=True,
                            allow_fail=True,
                        )
                        assert (
                            locked.returncode != 0 and "[read_only]" in locked.stderr
                        ), locked.stderr
                        report["read_only_refusal"] = True
                    finally:
                        app.json_ctl("perform_action", {"action": "toggle_read_only"})
                    value = command(stale)
                    assert (
                        value["exit_code"] == 1
                        and value["duration_ms"] >= 250
                        and "TARGET_DONE" in value["output"]
                    ), value
                    value = command(multi)
                    assert (
                        value["exit_code"] == 0
                        and "MULTI=first line" in value["output"]
                    ), value
                    value = command(slow, timeout_s=0.1)
                    assert value["timed_out"], value
                    time.sleep(0.7)
                    value = command(global_check)
                    assert (
                        value["exit_code"] == 0
                        and "GLOBAL=quoted value" in value["output"]
                    ), value
                    app.json_ctl("perform_action", {"action": "split_right"})
                    wait(
                        lambda: "PROBE>" in app.json_ctl("read_screen").get("text", "")
                    )
                    closing_command = (
                        "Write-Output 'CLOSE_TARGET'; Start-Sleep -Seconds 10"
                        if label == "powershell"
                        else "printf 'CLOSE_TARGET\\n'; sleep 10"
                    )
                    with ThreadPoolExecutor(max_workers=1) as executor:
                        pending = executor.submit(
                            app.ctl,
                            "run_command",
                            params={
                                "command": closing_command,
                                "require_correlation": True,
                                "timeout_s": 15,
                            },
                            raw=True,
                            allow_fail=True,
                            timeout=20,
                        )
                        wait(
                            lambda: (
                                "CLOSE_TARGET"
                                in app.json_ctl("read_screen").get("text", "")
                            )
                        )
                        app.json_ctl("perform_action", {"action": "close_pane"})
                        closed = pending.result(timeout=5)
                        assert (
                            closed.returncode != 0 and "[no_such_pane]" in closed.stderr
                        ), closed.stderr
                        report["pane_closure_error"] = True
                    if label in {"bash", "fish"}:
                        if label == "bash":
                            command("set -o vi")
                        else:
                            command(
                                "bind \\cxz 'bind \\cxh self-insert; emit fish_prompt; commandline -f repaint'"
                            )
                            app.json_ctl("send_keys", {"keys": ["ctrl+x", "z"]})
                        time.sleep(0.1)
                        forbidden = root / "withdrawn-capability-must-not-exist"
                        refused = app.ctl(
                            "run_command",
                            params={
                                "command": "printf side_effect > "
                                + shlex.quote(str(forbidden)),
                                "require_correlation": True,
                                "timeout_s": 0.2,
                            },
                            raw=True,
                            allow_fail=True,
                            timeout=5,
                        )
                        assert (
                            refused.returncode != 0
                            and "[bad_params]" in refused.stderr
                            and not forbidden.exists()
                        ), refused.stdout + refused.stderr
                        report["withdrawn_capability_refusal"] = True
                    if label == "powershell":
                        command(
                            "Set-PSReadLineKeyHandler -Chord 'Ctrl+x,h' -Function BackwardChar"
                        )
                        time.sleep(0.1)
                        forbidden = root / "replaced-binding-must-not-exist"
                        refused = app.ctl(
                            "run_command",
                            params={
                                "command": "[IO.File]::WriteAllText("
                                + ps_literal(forbidden)
                                + ",'side effect')",
                                "require_correlation": True,
                            },
                            raw=True,
                            allow_fail=True,
                        )
                        assert (
                            refused.returncode != 0
                            and "[bad_params]" in refused.stderr
                            and not forbidden.exists()
                        ), refused.stderr
                        report["replaced_binding_refusal"] = True
                report["pass"] = True
            report["cleanup"] = True
        except (Exception, SystemExit) as error:
            report["error"] = str(error)
            report["cleanup"] = False
            report["fixture_process_exited"] = app.proc is not None and app.proc.poll() is not None
        finally:
            (out / "summary.json").write_text(json.dumps(reports, indent=2) + "\n")
            print(
                json.dumps({k: v for k, v in report.items() if k != "results"}),
                flush=True,
            )
    assert hashlib.sha256(binary.read_bytes()).hexdigest() == binary_hash
    return (
        0
        if len(reports) == len(cases)
        and all(r["pass"] and r["cleanup"] for r in reports)
        else 1
    )


if __name__ == "__main__":
    previous_umask = os.umask(0o077)
    try:
        raise SystemExit(main())
    finally:
        os.umask(previous_umask)
