"""Check the fixed dirty4 resize candidate or exact old-helper policy control hosted source tree."""

import argparse
import gzip
import hashlib
import json
import os
from pathlib import Path
import shutil
import stat
import subprocess
import sys

BASE = "614b6cfbd40161821dbfd4077462897c9248ab07"
TREE = "232f8e30ec3d5ee8433fdd06395585902a3de429"
MANIFESTS = {
    "new": "6c0e5ffc0b75a57a6bce2f046a8d2d6188db7c378cf300c496671ed6b8d8c414",
    "old": "a52a117e180fe5f78f2d553b7e4907f6287ce4b3b27068dd568855f5d4235d5a",
}
PATCHES = {
    "new": (
        "source.patch.gz",
        "fda4c2ed8568d25b83cbcdd6148295953f971e042abe72868c489c6f24cf03a1",
    ),
    "old": (
        "old-control.patch.gz",
        "59fb1b7590c9e453386b8d754b20872172eac03f1cab9fd8fad348dc80fe55fc",
    ),
}
ROWS = (
    "entry",
    "old-light",
    "old-themes",
    "new-renderer",
    "strict",
    "final",
)
MINIMUM_FREE = 8 * 1024**3
HERE = Path(__file__).resolve().parent


def sha(data):
    return hashlib.sha256(data).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--product", required=True, type=Path)
    parser.add_argument("--evidence", required=True, type=Path)
    parser.add_argument("--variant", required=True, choices=("new", "old"))
    parser.add_argument("--row", required=True, choices=ROWS)
    parser.add_argument("--phase", required=True, choices=("entry", "before", "after"))
    args = parser.parse_args()
    product = args.product.resolve(strict=True)
    record = {
        "scope": "HOSTED_LINUX_SOURCE_ONLY_NOT_MAC_DELL_NATIVE_RELEASE",
        "variant": args.variant,
        "row": args.row,
        "phase": args.phase,
        "pass": False,
        "errors": [],
    }
    try:
        raw = (HERE / f"expected-{args.variant}-files.json").read_bytes()
        patchfile, patchhash = PATCHES[args.variant]
        if (
            sha(raw) != MANIFESTS[args.variant]
            or sha(gzip.decompress((HERE / patchfile).read_bytes())) != patchhash
        ):
            raise ValueError("fixed manifest or patch changed")
        expected = json.loads(raw)
        count = 1981
        if (
            expected["baseline_head"] != BASE
            or expected["baseline_tree"] != TREE
            or expected["patch_sha256"] != patchhash
            or expected["source_file_count"] != count
            or len(expected["files"]) != count
        ):
            raise ValueError("wrong fixed source manifest")

        def git(*argv):
            return subprocess.check_output(
                ["git", *argv],
                cwd=product,
                env={**os.environ, "GIT_OPTIONAL_LOCKS": "0"},
            )

        head = git("rev-parse", "HEAD").decode().strip()
        tree = git("rev-parse", "HEAD^{tree}").decode().strip()
        delta = git(
            "diff",
            "--binary",
            "--no-ext-diff",
            "--abbrev=9",
            "--unified=3",
            "--no-color",
            "--src-prefix=a/",
            "--dst-prefix=b/",
            "HEAD",
        )
        record.update(head=head, tree=tree, diff_sha256=sha(delta))
        if head != BASE or tree != TREE or sha(delta) != patchhash:
            raise ValueError("baseline HEAD/tree or canonical source diff mismatch")
        indexed = {}
        for row in git("ls-files", "--stage", "-z").split(b"\0"):
            if row:
                metadata, name = row.split(b"\t", 1)
                mode, blob, stage = metadata.decode().split()
                if stage != "0":
                    raise ValueError("unmerged source index")
                indexed[name.decode()] = (mode, blob)
        if set(indexed) != set(expected["files"]):
            raise ValueError("indexed file set differs from the exact manifest")
        untracked = git("ls-files", "--others", "--exclude-standard", "-z")
        if untracked:
            raise ValueError(
                "unexpected untracked source, including an unindexed new ISC file"
            )
        mismatches = []
        for name, row in expected["files"].items():
            path = product / name
            info = path.lstat()
            if not stat.S_ISREG(info.st_mode):
                mismatches.append(name + ": not a regular file")
                continue
            mode = "100755" if info.st_mode & 0o111 else "100644"
            data = path.read_bytes()
            if (
                indexed[name] != (row["git_mode"], row["index_blob"])
                or mode != row["git_mode"]
                or len(data) != row["bytes"]
                or sha(data) != row["sha256"]
            ):
                mismatches.append(name)
        record.update(
            files_checked=len(indexed),
            mismatches=mismatches,
            free_bytes=shutil.disk_usage(product).free,
            minimum_free=MINIMUM_FREE,
            platform=sys.platform,
            uid=os.getuid(),
            manifest_sha256=sha(raw),
            cargo_target_dir=os.environ.get("CARGO_TARGET_DIR"),
        )
        if mismatches:
            raise ValueError("source bytes, index blobs or executable modes changed")
        if sys.platform != "linux":
            raise ValueError("this carrier qualifies hosted Linux only")
        if args.phase != "after" and record["free_bytes"] < MINIMUM_FREE:
            raise ValueError("fresh hosted disk is below the unchanged 8 GiB floor")
        record["pass"] = True
    except (OSError, ValueError, KeyError, subprocess.SubprocessError) as error:
        record["errors"].append({"type": type(error).__name__, "message": str(error)})
    target = args.evidence / f"binding-{args.variant}-{args.row}-{args.phase}.json"
    with target.open("x", encoding="utf-8") as output:
        output.write(json.dumps(record, indent=2) + "\n")
    print(json.dumps(record, sort_keys=True))
    return 0 if record["pass"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
