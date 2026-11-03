"""Bounded native macOS clipboard snapshots for disposable smoke probes."""

from contextlib import contextmanager
import json
from pathlib import Path
import subprocess


def pasteboard(request):
    helper = Path(__file__).with_name("modal-pasteboard.js")
    result = subprocess.run(
        ["osascript", "-l", "JavaScript", str(helper)],
        input=json.dumps(request),
        text=True,
        capture_output=True,
        timeout=10,
        check=True,
    )
    return json.loads(result.stdout)


@contextmanager
def clipboard_probe(text, *, request=None):
    request = pasteboard if request is None else request
    snapshot = None
    try:
        snapshot = request({"op": "snapshot"})
    except (subprocess.SubprocessError, ValueError) as error:
        print(
            f"clipboard probe skipped: snapshot failed ({type(error).__name__})",
            flush=True,
        )
    if snapshot is None or not snapshot["ok"]:
        if snapshot is not None:
            print(f"clipboard probe skipped: {snapshot['reason']}", flush=True)
        yield False
        return
    try:
        yield snapshot
    finally:
        try:
            restored = request({"op": "restore", "snapshot": snapshot, "text": text})
            if not restored["ok"]:
                print(f"clipboard restore skipped: {restored['reason']}", flush=True)
        except (subprocess.SubprocessError, ValueError) as error:
            print(
                f"clipboard restore skipped: helper failed ({type(error).__name__})",
                flush=True,
            )
