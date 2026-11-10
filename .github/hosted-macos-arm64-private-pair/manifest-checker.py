"""Fixed exact-head Mac source and raw-pair checks; inert until Root binds it."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import stat
import subprocess
import sys
import time
import tomllib

PRODUCT_HEAD = "0b2139325bd27a18eb25e34e20cf64056e6ee8a9"
PRODUCT_TREE = "ba47c9e33e513eedd3eb767536e54013b2bfe08f"
MANIFEST_SHA = "39963143233138f049d8a998ddc08c320013e311f25084885fba3ea9fdd5cb75"
HERE = Path(__file__).resolve().parent
MIN_FREE = 8 * 1024**3
PHASES = (
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


def need(ok, message):
    if not ok:
        raise ValueError(message)


def sha(data):
    return hashlib.sha256(data).hexdigest()


def read(path, cap=16 * 1024**2):
    info = path.lstat()
    need(
        stat.S_ISREG(info.st_mode) and info.st_size <= cap,
        f"bounded regular input: {path}",
    )
    data = path.read_bytes()
    need(len(data) == info.st_size, f"input changed: {path}")
    return data


def load(path):
    return json.loads(read(path))


def fact(path):
    data = read(path, 128 * 1024**2)
    return {"bytes": len(data), "sha256": sha(data)}


def cargo_configuration(product):
    # Cargo searches cwd/ancestors and Cargo home, including extensionless config.
    cwd = product.resolve(strict=True)
    home = os.environ.get("CARGO_HOME")
    if not home:
        need(bool(os.environ.get("HOME")), "unknown Cargo home")
        home = str(Path(os.environ["HOME"]) / ".cargo")
    home = Path(home)
    if not home.is_absolute():
        home = cwd / home
    roots = [home, *(p / ".cargo" for p in (cwd, *cwd.parents))]
    need(len(roots) <= 130, "bounded Cargo configuration hierarchy")
    checked = []
    for root in dict.fromkeys(roots):
        for name in ("config", "config.toml"):
            path = root / name
            need(len(str(path).encode()) <= 4096, "bounded Cargo configuration path")
            try:
                path.lstat()
            except FileNotFoundError:
                checked.append(str(path))
            else:
                raise ValueError(f"Cargo configuration present: {path}")
    return {"absent_config_paths": checked, "contents_read": False}


def native_toolchain(product, phase):
    need(os.environ.get("RUSTUP_TOOLCHAIN") == "1.97.1", "pinned RUSTUP_TOOLCHAIN")

    def query(*argv):
        return subprocess.check_output(list(argv), cwd=product, text=True, timeout=15)

    active = query("rustup", "show", "active-toolchain").strip()
    need(
        active and active.split()[0] == "1.97.1-aarch64-apple-darwin",
        "native active toolchain",
    )
    # Resolve relative PATH entries as the product-cwd child process does.
    base = product.resolve(strict=True)
    search = os.pathsep.join(
        str(base / item) if not Path(item).is_absolute() else item
        for item in os.get_exec_path()
    )
    proxy = shutil.which("rustup", path=search)
    need(proxy is not None, "Rustup proxy unavailable")
    proxy_fact = fact(Path(proxy).resolve(strict=True))
    paths = {}
    for name in ("rustc", "cargo"):
        selected = Path(query("rustup", "which", "--toolchain", "1.97.1", name).strip())
        need(selected.is_absolute(), "absolute selected tool")
        selected = selected.resolve(strict=True)
        invoked = shutil.which(name, path=search)
        need(invoked is not None, "selected command unavailable")
        invoked = Path(invoked).resolve(strict=True)
        direct = invoked.samefile(selected)
        need(
            direct or fact(invoked) == proxy_fact,
            "command is neither selected toolchain nor Rustup proxy",
        )
        paths[name] = {
            "invoked": str(invoked),
            "selected": str(selected),
            "route": "direct-toolchain" if direct else "rustup-proxy",
            "selected_binary": fact(selected),
        }
    rustc = query("rustc", "-Vv")
    need(
        "host: aarch64-apple-darwin\n" in rustc and "release: 1.97.1\n" in rustc,
        "native pinned Rust toolchain",
    )
    cargo = query("cargo", "-Vv")
    need(
        re.match(r"cargo 1\.97\.1(?:[ (]|$)", cargo) is not None,
        "pinned native Cargo version",
    )
    selected_doc = Path(
        query("rustup", "which", "--toolchain", "1.97.1", "rustdoc").strip()
    )
    need(selected_doc.is_absolute(), "absolute selected rustdoc")
    doc_binary = fact(selected_doc)
    pin = os.environ.get("RUSTDOC")
    required = phase in (
        "before-core",
        "before-vendor",
        "before-vendor-clippy",
        "before-render",
    )
    need(not pin or pin == str(selected_doc), "unreviewed RUSTDOC")
    need(
        not required or pin == str(selected_doc),
        "native tests require selected RUSTDOC pin",
    )
    # Invoke only the selected absolute executable, never ambient PATH rustdoc.
    rustdoc = query(str(selected_doc), "-Vv")
    need(
        re.match(r"rustdoc 1\.97\.1(?:[ (]|$)", rustdoc) is not None
        and "host: aarch64-apple-darwin\n" in rustdoc
        and "release: 1.97.1\n" in rustdoc,
        "pinned native rustdoc version",
    )
    return {
        "rustdoc": {
            "selected": str(selected_doc),
            "selected_binary": doc_binary,
            "verbose": rustdoc,
            "active_pin": pin,
            "pin_required": required,
        },
        "rustc_verbose": rustc,
        "cargo_verbose": cargo,
        "rustup_active": active,
        "tool_paths": paths,
    }


def source(product, phase):
    # Reject all unfilled identity inputs before invoking Git or any compiler.
    need(
        re.fullmatch(r"[0-9a-f]{40}", PRODUCT_HEAD) is not None, "unfilled product HEAD"
    )
    need(
        re.fullmatch(r"[0-9a-f]{40}", PRODUCT_TREE) is not None, "unfilled product tree"
    )
    need(
        re.fullmatch(r"[0-9a-f]{64}", MANIFEST_SHA) is not None,
        "unfilled source manifest SHA",
    )
    cargo_config = cargo_configuration(product)
    raw = read(HERE / "expected-source.json")
    need(sha(raw) == MANIFEST_SHA, "source manifest hash")
    expected = json.loads(raw)
    need(
        expected["product_head"] == PRODUCT_HEAD
        and expected["product_tree"] == PRODUCT_TREE,
        "manifest identity",
    )
    files = expected["files"]
    need(
        type(expected["source_file_count"]) is int
        and 0 < expected["source_file_count"] <= 100_000
        and len(files) == expected["source_file_count"],
        "complete nonempty source manifest",
    )

    def git(*args):
        return subprocess.check_output(["git", *args], cwd=product, timeout=30)

    head = git("rev-parse", "HEAD").decode().strip()
    tree = git("rev-parse", "HEAD^{tree}").decode().strip()
    need((head, tree) == (PRODUCT_HEAD, PRODUCT_TREE), "literal product commit/tree")
    need(
        not git("status", "--porcelain=v1", "--untracked-files=all"),
        "product checkout not clean",
    )
    indexed = {}
    for row in git("ls-files", "--stage", "-z").split(b"\0"):
        if row:
            meta, name = row.split(b"\t", 1)
            mode, _, stage = meta.decode().split()
            need(stage == "0", "unmerged source")
            indexed[name.decode()] = mode
    need(set(indexed) == set(files), "full tracked file set")
    for name, expected_file in files.items():
        relative = Path(name)
        need(not relative.is_absolute() and ".." not in relative.parts, "source path")
        path = product / relative
        info = path.lstat()
        mode = "100755" if info.st_mode & 0o111 else "100644"
        need(
            indexed[name] == expected_file["git_mode"] == mode
            and fact(path) == {k: expected_file[k] for k in ("bytes", "sha256")},
            name,
        )
    need(
        sys.platform == "darwin" and os.uname().machine == "arm64", "native ARM64 macOS"
    )
    need(os.environ.get("RUNNER_ARCH") == "ARM64", "runner architecture")
    translated = subprocess.run(
        ["sysctl", "-n", "sysctl.proc_translated"],
        capture_output=True,
        text=True,
        timeout=10,
        cwd=product,
    )
    need(
        (translated.returncode == 0 and translated.stdout.strip() == "0")
        or (
            translated.returncode == 1
            and not translated.stdout.strip()
            and "unknown oid" in translated.stderr
            and "sysctl.proc_translated" in translated.stderr
        ),
        "Rosetta or unknown translation",
    )
    for key in (
        "RUSTC",
        "CARGO_BUILD_RUSTC",
        "CARGO_BUILD_RUSTFLAGS",
        "CARGO_BUILD_RUSTC_WRAPPER",
        "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER",
        "RUSTDOCFLAGS",
        "CARGO_BUILD_RUSTDOC",
        "CARGO_BUILD_RUSTDOCFLAGS",
        "CARGO_ENCODED_RUSTDOCFLAGS",
        "RUSTFLAGS",
        "CARGO_ENCODED_RUSTFLAGS",
        "RUSTC_WRAPPER",
        "RUSTC_WORKSPACE_WRAPPER",
        "CARGO_TARGET_DIR",
    ):
        need(not os.environ.get(key), f"unreviewed {key}")
    for key, value in os.environ.items():
        if (
            key.startswith(("CARGO_TARGET_", "TARGET_"))
            and key.endswith(("_RUSTFLAGS", "_RUSTDOCFLAGS"))
        ) or (key.startswith("CARGO_") and key.endswith("_RUNNER")):
            need(not value, f"unreviewed {key}")
    need(
        not any(k.startswith("CARGO_PROFILE_") for k in os.environ), "profile override"
    )
    need(
        os.environ.get("CARGO_INCREMENTAL") == "0"
        and os.environ.get("CARGO_BUILD_JOBS") == "1",
        "build environment",
    )
    toolchain = native_toolchain(product, phase)
    free = shutil.disk_usage(product).free
    need(
        phase in ("after", "pair", "native-unit") or free >= MIN_FREE,
        "8 GiB compiler floor",
    )
    return {
        "head": head,
        "tree": tree,
        "files_checked": len(files),
        "manifest_sha256": sha(raw),
        "free_bytes": free,
        "minimum_free": MIN_FREE,
        **toolchain,
        "cargo_configuration": cargo_config,
    }


def drained(directory, expected_argv):
    data = read(directory / "strict.log")
    log = load(directory / "strict-log.json")
    outcome = load(directory / "exit.json")
    need(
        log["pipe_eof"] is True
        and log["over_limit"] is False
        and log["bytes"]
        == log["retained_bytes"]
        == len(data)
        <= log["limit"]
        == 16 * 1024**2
        and log["sha256_all_drained_bytes"] == sha(data)
        and log["live_echo_bytes"] <= log["live_echo_limit"] == 65536,
        "complete bounded drainage",
    )
    need(
        outcome["command_exit"] == outcome["log_drain_exit"] == 0
        and outcome["argv"] == expected_argv
        and outcome["started_wall_ns"]
        < log["drained_wall_ns"]
        <= outcome["finished_wall_ns"],
        "actual command outcome/origin",
    )
    return data, outcome


def abi(evidence, name):
    need(
        read(evidence / f"{name}.archs.txt").decode().strip() == "arm64",
        "Mach-O architecture",
    )
    linked = read(evidence / f"{name}.otool-libraries.txt").decode()
    libraries = [
        line.strip().split(" (compatibility version", 1)[0]
        for line in linked.splitlines()
        if line.startswith(("\t", " "))
    ]
    need(
        libraries
        and all(x.startswith(("/System/Library/", "/usr/lib/")) for x in libraries),
        "unexpected non-system install name; no automatic dependency copying",
    )
    commands = read(evidence / f"{name}.otool-load-commands.txt").decode()
    need("LC_RPATH" not in commands, "unreviewed runtime search path")
    need(
        commands.count("cmd LC_BUILD_VERSION") == 1
        and re.search(r"\bplatform\s+(?:MACOS|1)\b", commands),
        "macOS build platform",
    )
    minimum = re.search(r"\bminos\s+([0-9.]+)", commands)
    sdk = re.search(r"\bsdk\s+([0-9.]+)", commands)
    need(minimum and sdk, "actual minimum OS and SDK")
    return {
        "libraries": libraries,
        "rpaths": [],
        "minos": minimum[1],
        "sdk": sdk[1],
        "onsite_compatibility": "Root must compare actual local OS before staging/execution.",
    }


def bounded_inventory(evidence):
    entries = list(evidence.rglob("*"))
    need(all(not p.is_symlink() for p in entries), "artifact symlink")
    retained = [p for p in entries if p.is_file()]
    need(
        len(retained) + 1 <= 128
        and sum(p.stat().st_size for p in retained) + 256 * 1024 <= 256 * 1024**2,
        "bounded artifact inventory with final-manifest reserve",
    )


def native_unit(evidence):
    bounded_inventory(evidence)
    bookends = {}
    for phase in (
        "entry",
        "before-core",
        "before-vendor",
        "before-vendor-clippy",
        "before-render",
        "after",
    ):
        path = evidence / f"binding-{phase}.json"
        record = load(path)
        need(
            record["pass"] is True
            and record["phase"] == phase
            and (record["head"], record["tree"], record["manifest_sha256"])
            == (PRODUCT_HEAD, PRODUCT_TREE, MANIFEST_SHA),
            "native-unit source bookends",
        )
        bookends[phase] = fact(path)
    data, outcome = drained(evidence / "native-unit", ["fixed-native-unit-sequence"])
    return {
        "scope": "COMPLETE_HOSTED_NATIVE_UNIT_EVIDENCE_NOT_ONSITE_ACCEPTANCE",
        "strict_log": {"bytes": len(data), "sha256": sha(data)},
        "strict_log_record": fact(evidence / "native-unit/strict-log.json"),
        "exit_record": fact(evidence / "native-unit/exit.json"),
        "command_outcome": outcome,
        "bookends": bookends,
        "tool_metadata": {
            name: fact(evidence / name)
            for name in ("os-tools-sdk.txt", "run-metadata.json")
        },
    }


def pair(product, evidence):
    after = load(evidence / "binding-after.json")
    need(
        after["pass"] is True
        and after["phase"] == "after"
        and (after["head"], after["tree"], after["manifest_sha256"])
        == (PRODUCT_HEAD, PRODUCT_TREE, MANIFEST_SHA),
        "after source bookend",
    )
    need(
        os.environ.get("VALIDATION_JOB_RESULT") == "success",
        "native focused validation job",
    )
    bounded_inventory(evidence)
    metadata = load(evidence / "cargo-metadata.json")
    packages = {p["name"]: p for p in metadata["packages"]}
    profiles = tomllib.loads(read(product / "Cargo.toml").decode())["profile"]
    need(
        profiles["release"]["panic"] == "abort"
        and profiles["media-worker"]["inherits"] == "release"
        and profiles["media-worker"]["panic"] == "unwind",
        "GUI/worker profile semantics",
    )
    result = {}
    for label, package, profile, target, raw_name in (
        ("main", "kettle", "--release", "release", "kettle"),
        (
            "worker",
            "kettle-media-worker",
            "media-worker",
            "media-worker",
            "kettle-media-worker",
        ),
    ):
        package_row = packages[package]
        need(
            Path(package_row["manifest_path"]).resolve()
            == (product / "crates" / package / "Cargo.toml").resolve(),
            "product package metadata",
        )
        args = ["nice", "-n", "10", "cargo", "build", "--locked"]
        args += [profile] if label == "main" else ["--profile", profile]
        args += [
            "--target",
            "aarch64-apple-darwin",
            "-p",
            package,
            "--message-format=json-render-diagnostics",
        ]
        data, outcome = drained(evidence / label, args)
        events = []
        for line in data.decode().splitlines():
            if line.startswith("{"):
                events.append(json.loads(line))
        source_hashes = {
            value
            for event in events
            if event.get("reason") == "build-script-executed"
            and event.get("package_id") == package_row["id"]
            for key, value in event.get("env", [])
            if key == "KETTLE_SOURCE_HASH"
        }
        need(
            len(source_hashes) == 1
            and re.fullmatch(r"[0-9a-f]{16}", next(iter(source_hashes))),
            "actual product build-script sourcehash16",
        )
        executable = product / "target" / "aarch64-apple-darwin" / target / raw_name
        artifacts = [
            event
            for event in events
            if event.get("reason") == "compiler-artifact"
            and event.get("package_id") == package_row["id"]
            and event.get("target", {}).get("name") == package
            and event.get("executable") == str(executable)
        ]
        need(
            len(artifacts) == 1
            and artifacts[0]["profile"]["test"] is False
            and artifacts[0]["profile"]["opt_level"] == "3"
            and type(artifacts[0]["fresh"]) is bool,
            "actual optimized binary artifact",
        )
        raw = evidence / "raw" / raw_name
        need(
            fact(raw) == fact(executable), "raw retained bytes differ from Cargo output"
        )
        result[label] = {
            "path": str(raw),
            **fact(raw),
            "source_hash": next(iter(source_hashes)),
            "cargo_artifact": artifacts[0],
            "command_outcome": outcome,
            "ABI": abi(evidence, label),
        }
    need(
        result["main"]["source_hash"] == result["worker"]["source_hash"],
        "GUI/worker source identity",
    )
    version = read(evidence / "version.txt").decode().strip()
    need(
        version == f"kettle {packages['kettle']['version']} ({PRODUCT_HEAD[:12]})",
        "actual main version",
    )
    need(
        set(p.name for p in (evidence / "raw").iterdir())
        == {"kettle", "kettle-media-worker"},
        "fixed raw pair only",
    )
    return {
        "scope": "RAW_PRIVATE_TEST_PAIR_NOT_SIGNED_RELEASE_OR_ONSITE_NATIVE_ACCEPTANCE",
        "product_head": PRODUCT_HEAD,
        "product_tree": PRODUCT_TREE,
        "source_hash": result["main"]["source_hash"],
        "version": version,
        "products": result,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--product", type=Path, required=True)
    parser.add_argument("--evidence", type=Path, required=True)
    parser.add_argument("--phase", choices=PHASES, required=True)
    args = parser.parse_args()
    record = {
        "phase": args.phase,
        "pass": False,
        "errors": [],
        "wall_ns": time.time_ns(),
    }
    try:
        record.update(source(args.product, args.phase))
        if args.phase == "pair":
            record["pair"] = pair(args.product, args.evidence)
        elif args.phase == "native-unit":
            record["native_unit"] = native_unit(args.evidence)
        record["pass"] = True
    except (
        OSError,
        ValueError,
        KeyError,
        TypeError,
        subprocess.SubprocessError,
    ) as error:
        record["errors"].append({"type": type(error).__name__, "message": str(error)})
    args.evidence.mkdir(mode=0o700, parents=True, exist_ok=True)
    payload = json.dumps(record, indent=2) + "\n"
    need(len(payload.encode()) <= 256 * 1024, "bounded binding record")
    with (args.evidence / f"binding-{args.phase}.json").open("x") as out:
        out.write(payload)
    print(json.dumps(record, sort_keys=True))
    return 0 if record["pass"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
