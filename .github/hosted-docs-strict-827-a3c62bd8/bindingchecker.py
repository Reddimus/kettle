"""Fixed 827+a3c62bd8 hosted source binding; no build, network or process control."""

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

BASE = "82772df1dcba0b1fc11657e106897f8f4b797489"
TREE = "ecf3e9bfe19e5b19799a56be64a38c62f3471465"
PATCH = "a3c62bd8359ce0365028a4b6c272080330e99c9996dd0233521c13d78c112749"
MANIFEST = "5478f7e0a2aca37736869592aef4bad8055517d7fcae5dfe993cb13a241297f6"
MINIMUM_FREE = 8 * 1024**3
HERE = Path(__file__).resolve().parent


def sha(data):
    return hashlib.sha256(data).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--product", required=True, type=Path)
    parser.add_argument("--evidence", required=True, type=Path)
    parser.add_argument("--phase", required=True, choices=("entry", "before", "after"))
    args = parser.parse_args()
    product = args.product.resolve(strict=True)
    args.evidence.mkdir(parents=True, exist_ok=True)
    record = {"scope": "HOSTED_LINUX_SOURCE_ONLY_NOT_MAC_DELL_OR_RELEASE",
              "phase": args.phase, "pass": False, "errors": []}
    try:
        raw = (HERE / "expected-files.json").read_bytes()
        if sha(raw) != MANIFEST or sha(gzip.decompress((HERE / "docs.patch.gz").read_bytes())) != PATCH:
            raise ValueError("carrier manifest or exact patch changed")
        expected = json.loads(raw)
        if (expected["baseline_head"] != BASE or expected["baseline_tree"] != TREE
                or expected["patch_sha256"] != PATCH
                or expected["source_file_count"] != 1980 or len(expected["files"]) != 1980):
            raise ValueError("wrong fixed source manifest")

        def git(*argv):
            return subprocess.check_output(["git", *argv], cwd=product)

        head = git("rev-parse", "HEAD").decode().strip()
        tree = git("rev-parse", "HEAD^{tree}").decode().strip()
        delta = git("diff", "--binary", "--no-ext-diff", "--abbrev=9", "--unified=3",
                    "--no-color", "--src-prefix=a/", "--dst-prefix=b/", "HEAD")
        record.update(head=head, tree=tree, diff_sha256=sha(delta))
        if head != BASE or tree != TREE or sha(delta) != PATCH:
            raise ValueError("baseline HEAD/tree or canonical two-doc diff mismatch")
        indexed = {}
        for row in git("ls-files", "--stage", "-z").split(b"\0"):
            if row:
                metadata, name = row.split(b"\t", 1)
                mode, _, stage = metadata.decode().split()
                if stage != "0":
                    raise ValueError("unmerged source index")
                indexed[name.decode()] = mode
        if set(indexed) != set(expected["files"]):
            raise ValueError("tracked file set differs from the exact 1980 files")
        mismatches = []
        for name, row in expected["files"].items():
            path = product / name
            info = path.lstat()
            if not stat.S_ISREG(info.st_mode):
                mismatches.append(name + ": not a regular file")
                continue
            mode = "100755" if info.st_mode & 0o111 else "100644"
            data = path.read_bytes()
            if (indexed[name] != row["git_mode"] or mode != row["git_mode"]
                    or len(data) != row["bytes"] or sha(data) != row["sha256"]):
                mismatches.append(name)
        record.update(files_checked=len(indexed), mismatches=mismatches,
                      free_bytes=shutil.disk_usage(product).free, minimum_free=MINIMUM_FREE,
                      platform=sys.platform, uid=os.getuid(), root_uid=Path("/").stat().st_uid,
                      manifest_sha256=sha(raw))
        if mismatches:
            raise ValueError("source bytes or executable modes changed")
        if sys.platform != "linux":
            raise ValueError("this carrier qualifies hosted Linux only")
        if args.phase != "after" and record["free_bytes"] < MINIMUM_FREE:
            raise ValueError("fresh free space is below the unchanged 8 GiB floor")
        record["pass"] = True
    except (OSError, ValueError, KeyError, subprocess.SubprocessError) as error:
        record["errors"].append({"type": type(error).__name__, "message": str(error)})
    (args.evidence / ("binding-" + args.phase + ".json")).write_text(
        json.dumps(record, indent=2) + "\n")
    print(json.dumps(record, sort_keys=True))
    return 0 if record["pass"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
