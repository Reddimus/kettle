"""Copy only finite diagnostic prefixes; never raw binaries or EOF acceptance."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import stat

LIMIT = 3 * 1024**2
REPORT_RESERVE = 32 * 1024
FILES = {
    **{
        f"binding-{phase}.json": 64 * 1024
        for phase in (
            "entry",
            "before-core",
            "before-vendor",
            "before-vendor-clippy",
            "before-render",
            "before-main",
            "before-worker",
            "after",
            "pair",
            "native-unit",
        )
    },
    "os-tools-sdk.txt": 128 * 1024,
    "run-metadata.json": 16 * 1024,
    "cargo-metadata.json": 256 * 1024,
    "version.txt": 4096,
    "file.txt": 4096,
    **{
        f"{name}.{suffix}": 64 * 1024
        for name in ("main", "worker")
        for suffix in (
            "archs.txt",
            "otool-libraries.txt",
            "otool-load-commands.txt",
        )
    },
    **{
        f"{name}/{suffix}": cap
        for name in ("main", "worker", "native-unit")
        for suffix, cap in (
            ("strict.log", 256 * 1024),
            ("strict-log.json", 16 * 1024),
            ("exit.json", 16 * 1024),
        )
    },
}


def prepare(evidence, output):
    if sum(FILES.values()) + REPORT_RESERVE > LIMIT:
        raise ValueError("fixed diagnostic caps exceed total")
    output.mkdir(mode=0o700)
    rows = []
    copied = 0
    for relative, cap in FILES.items():
        path = evidence / relative
        row = {"source_relative": relative, "cap": cap}
        rows.append(row)
        try:
            if evidence.is_symlink() or any(
                parent.is_symlink()
                for parent in path.parents
                if parent == evidence or evidence in parent.parents
            ):
                raise ValueError("symlink diagnostic hierarchy")
            before = path.lstat()
            if not stat.S_ISREG(before.st_mode):
                raise ValueError("not a regular diagnostic")
            descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
            with os.fdopen(descriptor, "rb") as source:
                actual = os.fstat(source.fileno())
                if (actual.st_dev, actual.st_ino) != (before.st_dev, before.st_ino):
                    raise ValueError("diagnostic identity changed")
                data = source.read(cap)
            destination = output / relative.replace("/", "__")
            descriptor = os.open(
                destination, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600
            )
            with os.fdopen(descriptor, "wb") as target:
                target.write(data)
            copied += len(data)
            row.update(
                status="DIAGNOSTIC_PREFIX_ONLY_NOT_FULL_STREAM_OR_EOF_PROOF",
                source_bytes=actual.st_size,
                retained_bytes=len(data),
                truncated=actual.st_size > len(data),
                sha256_retained_prefix=hashlib.sha256(data).hexdigest(),
                output=destination.name,
            )
        except FileNotFoundError:
            row["status"] = "MISSING"
        except (OSError, ValueError) as error:
            row.update(status="NOT_RETAINED", error_type=type(error).__name__)
    record = {
        "scope": "BOUNDED_DIAGNOSTIC_PREFIXES_ONLY_NO_RAW_PAIR_OR_EOF_ACCEPTANCE",
        "limit": LIMIT,
        "files": rows,
        "retained_prefix_bytes": copied,
        "raw_binaries_included": False,
        "original_logs": "Original files remain at RUNNER_TEMP until job end; only separately validated complete logs can establish EOF.",
    }
    payload = (json.dumps(record, indent=2) + "\n").encode()
    if len(payload) > REPORT_RESERVE or copied + len(payload) > LIMIT:
        raise ValueError("bounded diagnostics report")
    descriptor = os.open(
        output / "diagnostics.json", os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600
    )
    with os.fdopen(descriptor, "wb") as target:
        target.write(payload)
    return record


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--evidence", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    prepare(args.evidence, args.output)


if __name__ == "__main__":
    main()
