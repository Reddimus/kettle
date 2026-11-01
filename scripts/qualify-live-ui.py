#!/usr/bin/env python3
"""Run the six quarantined Linux scenarios repeatedly on one fixed artifact."""

import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import signal
import subprocess
import sys
import time

SCENARIOS = (
    "search-history",
    "text-presentation",
    "split-exit-resize",
    "program-keys",
    "color-scheme",
    "steady-uploads",
)
REPO = Path(__file__).resolve().parents[1]
HELPER = REPO / "scripts/check-live-ui-smoke.py"


def digest(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def save(path, value):
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(json.dumps(value, indent=2) + "\n", encoding="utf-8")
    temporary.replace(path)


def load_live():
    spec = importlib.util.spec_from_file_location("qualification_live", HELPER)
    live = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = live
    spec.loader.exec_module(live)
    return live


def stop_owned_tree(live, proc, out, subreaper=None):
    """Freeze, record and kill retained instances, including detached sessions."""
    handles = {}
    receipt = {"processes": [], "closed": False}
    path = out / "forced-cleanup.json"
    save(path, receipt)

    def retain(handle):
        if handle.identity in handles:
            handle.close()
            return False
        handles[handle.identity] = handle
        receipt["processes"].append(
            {"pid": handle.pid, "identity": list(handle.identity)}
        )
        # Persist ownership before any signal; never target an unrecorded PID.
        save(path, receipt)
        handle.signal(signal.SIGSTOP)
        return True

    try:
        if proc.returncode is None:
            # Popen has not reaped this child, so its PID cannot be reused.
            handle = live._open_stable_process_if_present(proc.pid, "case supervisor")
            if handle is not None:
                retain(handle)
        deadline = time.monotonic() + 8
        while time.monotonic() < deadline:
            try:
                listed = live.run(["ps", "-axo", "pid=,ppid=,uid="], timeout=2)
                if listed.returncode:
                    raise RuntimeError(
                        "could not inventory owned supervisor descendants"
                    )
                parents = {}
                for line in listed.stdout.splitlines():
                    pid, parent, uid = map(int, line.split())
                    if uid == os.getuid():
                        parents[pid] = parent
                added = False
                if subreaper is not None:
                    for handle in subreaper.adopted_roots(
                        parents, set(parents)
                    ).values():
                        added = retain(handle) or added
                owners = {handle.pid: handle for handle in handles.values()}
                for pid, parent in parents.items():
                    anchor = owners.get(parent)
                    if pid in owners or anchor is None or not anchor.matches_current():
                        continue
                    handle = live._open_stable_process_if_present(
                        pid, "case descendant"
                    )
                    if handle is None:
                        continue
                    try:
                        current = live.run(
                            ["ps", "-o", "ppid=", "-p", str(pid)], timeout=2
                        )
                        owned = (
                            current.returncode == 0
                            and current.stdout.strip() == str(parent)
                            and handle.matches_current()
                            and anchor.matches_current()
                        )
                    except BaseException:
                        handle.close()
                        raise
                    if owned:
                        added = retain(handle) or added
                    else:
                        handle.close()
                if not added and all(
                    (live._process_state(handle) or "Z").startswith(("T", "Z"))
                    for handle in handles.values()
                ):
                    break
            except Exception as error:
                receipt.setdefault("inventory_errors", []).append(str(error))
                save(path, receipt)
            time.sleep(0.02)
        else:
            raise RuntimeError("owned supervisor descendants did not quiesce")
    except BaseException as error:
        receipt["error"] = str(error)
    finally:
        errors = []
        for handle in reversed(list(handles.values())):
            try:
                handle.signal(signal.SIGKILL)
            except BaseException as error:
                errors.append(str(error))
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            alive = []
            for handle in handles.values():
                try:
                    state = live._process_state(handle)
                    if state is not None and not state.startswith("Z"):
                        alive.append(handle.pid)
                    elif subreaper is not None and handle.pid != proc.pid:
                        os.waitpid(handle.pid, os.WNOHANG)
                except ChildProcessError:
                    pass
                except BaseException as error:
                    errors.append(str(error))
                    alive.append(handle.pid)
            if not alive:
                receipt["closed"] = True
                break
            time.sleep(0.02)
        errors.extend(
            str(error) for error in live._close_stable_process_handles(handles)
        )
        if errors:
            receipt["errors"] = errors
        save(path, receipt)
    return receipt


def run_case(case, binary, out):
    """Keep fixture cleanup and its result observable even on an interrupt."""
    live = load_live()
    original = live.LiveKettle
    fixtures = []
    exit_before_cleanup = {}
    receipt = {"case": case, "status": "FAIL", "cleanup": False, "fixtures": []}
    path = out / "case.json"

    class RecordedLive(original):
        def __enter__(self):
            try:
                return super().__enter__()
            finally:
                if self.proc is not None:
                    fixtures.append(self.proc)
                    receipt["fixtures"].append({"pid": self.proc.pid, "closed": False})
                    save(path, receipt)

        def __exit__(self, *args):
            if self.proc is not None:
                exit_before_cleanup[id(self.proc)] = self.proc.poll()
            return super().__exit__(*args)

    live.LiveKettle = RecordedLive
    try:
        function = getattr(live, "run_" + case.replace("-", "_"))
        if case == "steady-uploads":
            function(str(binary), out, screenshots_only=True)
        else:
            function(str(binary), out)
        receipt["status"] = "PASS"
    finally:
        receipt["fixtures"] = []
        for proc in fixtures:
            code = proc.poll()
            before = exit_before_cleanup.get(id(proc))
            receipt["fixtures"].append(
                {
                    "pid": proc.pid,
                    "closed": code is not None,
                    "exit": code,
                    "exit_before_cleanup": before,
                    "unexpected_exit": before not in (None, 0)
                    or code not in (None, 0, -signal.SIGTERM),
                }
            )
        receipt["cleanup"] = bool(fixtures) and all(
            item["closed"] for item in receipt["fixtures"]
        )
        if not receipt["cleanup"] or any(
            item["unexpected_exit"] for item in receipt["fixtures"]
        ):
            receipt["status"] = "FAIL"
        save(path, receipt)
    return 0 if receipt["status"] == "PASS" else 1


def invoke_case(case, binary, out, timeout=300):
    out.mkdir(mode=0o700)
    command = [
        sys.executable,
        str(Path(__file__).resolve()),
        "case",
        "--scenario",
        case,
        "--kettle",
        str(binary),
        "--out",
        str(out),
    ]
    started = time.monotonic()
    live = load_live()
    scope = live.LinuxSubreaperScope.acquire() if platform.system() == "Linux" else None
    forced = None
    try:
        with (out / "supervisor.log").open("w", encoding="utf-8") as log:
            proc = subprocess.Popen(
                command, stdout=log, stderr=subprocess.STDOUT, start_new_session=True
            )
            timed_out = False
            try:
                save(out / "supervisor.json", {"pid": proc.pid, "command": command})
                code = proc.wait(timeout=timeout)
            except subprocess.TimeoutExpired:
                timed_out = True
                forced = stop_owned_tree(live, proc, out, scope)
                code = proc.wait(timeout=5)
            except BaseException:
                stop_owned_tree(live, proc, out, scope)
                proc.wait(timeout=5)
                raise
            if scope is not None and forced is None:
                # Even a supervisor that exited normally may have orphaned a child.
                forced = stop_owned_tree(live, proc, out, scope)
    finally:
        if scope is not None:
            scope.close()
    try:
        receipt = json.loads((out / "case.json").read_text(encoding="utf-8"))
    except (OSError, ValueError):
        receipt = {}
    passed = (
        code == 0
        and not timed_out
        and (
            forced is None
            or (
                not forced["processes"]
                and forced["closed"]
                and not forced.get("error")
                and not forced.get("errors")
                and not forced.get("inventory_errors")
            )
        )
        and receipt.get("case") == case
        and receipt.get("status") == "PASS"
        and receipt.get("cleanup") is True
    )
    return {
        "case": case,
        "passed": passed,
        "exit": code,
        "timed_out": timed_out,
        "forced_cleanup": forced,
        "seconds": time.monotonic() - started,
        "receipt": receipt,
    }


def summarize(receipts):
    if len(receipts) < 3:
        raise ValueError("at least three fresh allocations are required")
    if len({r["allocation"] for r in receipts}) != len(receipts):
        raise ValueError("runner allocations must be distinct")
    for key in ("source", "tree", "binary_sha256", "helper_sha256"):
        if len({r[key] for r in receipts}) != 1:
            raise ValueError(f"candidate identity differs: {key}")
    counts = {case: 0 for case in SCENARIOS}
    for receipt in receipts:
        if receipt["status"] != "PASS" or not receipt["unchanged"]:
            raise ValueError("failed or changed candidate cannot qualify")
        for case in SCENARIOS:
            if (
                type(receipt["counts"][case]) is not int
                or receipt["counts"][case] != 100
            ):
                raise ValueError(
                    "every allocation must complete 100 rounds of every scenario"
                )
            counts[case] += receipt["counts"][case]
    if any(n < 300 for n in counts.values()):
        raise ValueError("300 successful repetitions per scenario are required")
    return {
        "status": "PASS",
        "counts": counts,
        "allocations": len(receipts),
        "source": receipts[0]["source"],
        "tree": receipts[0]["tree"],
        "binary_sha256": receipts[0]["binary_sha256"],
        "helper_sha256": receipts[0]["helper_sha256"],
    }


def identity(binary):
    binary = binary.resolve(strict=True)
    if subprocess.check_output(
        ["git", "status", "--porcelain"], cwd=REPO, text=True
    ).strip():
        raise ValueError("qualification requires a clean source checkout")
    source = subprocess.check_output(
        ["git", "rev-parse", "HEAD"], cwd=REPO, text=True
    ).strip()
    tree = subprocess.check_output(
        ["git", "rev-parse", "HEAD^{tree}"], cwd=REPO, text=True
    ).strip()
    return {
        "source": source,
        "tree": tree,
        "binary_sha256": digest(binary),
        "helper_sha256": digest(HELPER),
    }


def campaign(binary, out, allocation, expected):
    out.mkdir(mode=0o700, parents=True, exist_ok=False)
    receipt = {
        "expected_identity": str(expected) if isinstance(expected, Path) else expected,
        "allocation": allocation,
        "counts": dict.fromkeys(SCENARIOS, 0),
        "status": "FAIL",
        "unchanged": False,
        "admitted": False,
    }
    save(out / "summary.json", receipt)
    try:
        if isinstance(expected, Path):
            expected = json.loads(expected.read_text(encoding="utf-8"))
            receipt["expected_identity"] = expected
        binary = binary.resolve(strict=True)
        candidate = identity(binary)
        receipt["observed_identity"] = candidate
        if candidate != expected:
            raise ValueError("downloaded artifact does not match the source and helper")
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        receipt["admission_error"] = str(error)
        save(out / "summary.json", receipt)
        return 1
    receipt.update(candidate)
    receipt["admitted"] = True
    try:
        with (out / "runs.jsonl").open("w", encoding="utf-8") as records:
            for repetition in range(1, 101):
                for case in SCENARIOS:
                    result = invoke_case(case, binary, out / f"{repetition:03d}-{case}")
                    result["repetition"] = repetition
                    records.write(json.dumps(result) + "\n")
                    records.flush()
                    if not result["passed"]:
                        return 1
                    receipt["counts"][case] += 1
                    save(out / "summary.json", receipt)
                print(
                    f"allocation {allocation}: {repetition}/100 rounds passed",
                    flush=True,
                )
        receipt["status"] = "PASS"
    finally:
        try:
            receipt["unchanged"] = identity(binary) == candidate
        except (OSError, ValueError, subprocess.CalledProcessError) as error:
            receipt["unchanged"] = False
            receipt["final_identity_error"] = str(error)
        if not receipt["unchanged"]:
            receipt["status"] = "FAIL"
        save(out / "summary.json", receipt)
    return 0 if receipt["status"] == "PASS" else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    for name in ("run", "case", "manifest"):
        command = sub.add_parser(name)
        command.add_argument("--kettle", type=Path, required=True)
        command.add_argument("--out", type=Path, required=True)
        if name == "run":
            command.add_argument("--allocation", required=True)
            command.add_argument("--identity", type=Path, required=True)
        elif name == "case":
            command.add_argument("--scenario", choices=SCENARIOS, required=True)
    command = sub.add_parser("summarize")
    command.add_argument("receipts", type=Path, nargs="+")
    command.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    if args.command == "case":
        return run_case(args.scenario, args.kettle, args.out)
    if args.command == "run":
        return campaign(
            args.kettle,
            args.out,
            args.allocation,
            args.identity,
        )
    if args.command == "manifest":
        save(args.out, identity(args.kettle))
        return 0
    try:
        result = summarize(
            [json.loads(p.read_text(encoding="utf-8")) for p in args.receipts]
        )
    except (OSError, ValueError, KeyError, TypeError) as error:
        result = {
            "status": "FAIL",
            "error": str(error),
            "receipts": [str(p) for p in args.receipts],
        }
    save(args.out, result)
    print(json.dumps(result))
    return 0 if result["status"] == "PASS" else 1


if __name__ == "__main__":
    raise SystemExit(main())
