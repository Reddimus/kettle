"""Check the fixed indexed candidate or tests-only old hosted source tree."""

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

BASE = "c8da69902751c63748f8f3722a6cfa2b7166ea91"
TREE = "00be5bab75466005e13fea3679b9d5bee9835e89"
MANIFESTS = {
    "new": "6611b37e75231991c258db5a126446c538f485a044be00fb513eda20fa26affa",
    "old": "ee210392f823ff82a1c2355b1f0a8f81a02f17efbc563085ecb418ab45efe778",
}
PATCHES = {
    "new": (
        "source.patch.gz",
        "acec26478b3f028662d6a8e8dbdfb685feff43f12a86fd553b254e75b139947d",
    ),
    "old": (
        "old-tests.patch.gz",
        "ce9668b436ed087200bb72a65e9c1a881acc9ff664dab6da48c45eb2614e63a0",
    ),
}
ROWS = (
    "entry",
    "old-virama",
    "old-singletons",
    "old-retention",
    "new-vendor",
    "new-core",
    "new-replay",
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
        count = 1981 if args.variant == "new" else 1980
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
