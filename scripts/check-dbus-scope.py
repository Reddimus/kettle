#!/usr/bin/env python3
"""Guard informational D-Bus maintenance exceptions, not vulnerabilities.

RUSTSEC-2026-0370/0371/0372 report the zbus 5 family's move to zbus 6.
Review every resolved consumer and registry version before retaining these
exceptions. No patched compatible release is available through the current
accessibility and desktop notification dependencies. See #633, #634, #635.
"""

import json
import subprocess
import sys
from pathlib import Path

from dependency_scope import metadata_command
from dependency_scope import scope_errors as check_scope

SEEDS = {"zvariant", "zbus_names", "zvariant_derive"}
EXPECTED_EDGES = {
    ("atspi", "atspi-proxies"),
    ("zbus_xml", "zbus_names"),
    ("atspi-common", "zbus_names"),
    ("zbus_macros", "zbus_names"),
    ("accesskit_unix", "atspi"),
    ("zbus-lockstep-macros", "zbus_xml"),
    ("atspi-common", "zbus"),
    ("kettle-ui", "accesskit_winit"),
    ("accesskit_atspi_common", "zvariant"),
    ("atspi-common", "zbus-lockstep"),
    ("zbus", "zvariant"),
    ("zbus_names", "zvariant"),
    ("notify-rust", "zbus"),
    ("zbus-lockstep", "zbus_xml"),
    ("zbus-lockstep-macros", "zvariant"),
    ("zbus", "zbus_macros"),
    ("zvariant", "zvariant_derive"),
    ("kettle", "kettle-ui"),
    ("zbus", "zbus_names"),
    ("accesskit_winit", "accesskit_unix"),
    ("accesskit_unix", "accesskit_atspi_common"),
    ("atspi-proxies", "zbus"),
    ("accesskit_unix", "zbus"),
    ("zbus-lockstep", "zvariant"),
    ("kettle-ui", "zbus"),
    ("kettle-ui", "notify-rust"),
    ("accesskit_atspi_common", "atspi-common"),
    ("zbus_xml", "zvariant"),
    ("atspi-common", "zvariant"),
    ("atspi-proxies", "atspi-common"),
    ("zbus_macros", "zvariant"),
    ("zbus-lockstep-macros", "zbus-lockstep"),
    ("atspi", "atspi-common"),
    ("atspi-common", "zbus-lockstep-macros"),
}
REVIEWED_VERSIONS = {
    "zvariant_derive": {"5.15.0"},
    "atspi": {"0.29.0"},
    "zbus_names": {"4.3.4"},
    "accesskit_atspi_common": {"0.21.0"},
    "atspi-proxies": {"0.13.0"},
    "zvariant": {"5.15.0"},
    "atspi-common": {"0.13.0"},
    "notify-rust": {"4.18.2"},
    "accesskit_unix": {"0.24.0"},
    "zbus-lockstep-macros": {"0.5.2"},
    "zbus_macros": {"5.19.0"},
    "accesskit_winit": {"0.34.1"},
    "zbus": {"5.19.0"},
    "zbus_xml": {"5.2.0"},
    "zbus-lockstep": {"0.5.2"},
}
REVIEWED_SOURCES = {
    "zvariant_derive": {"registry+https://github.com/rust-lang/crates.io-index"},
    "atspi": {"registry+https://github.com/rust-lang/crates.io-index"},
    "zbus_names": {"registry+https://github.com/rust-lang/crates.io-index"},
    "accesskit_atspi_common": {"registry+https://github.com/rust-lang/crates.io-index"},
    "atspi-proxies": {"registry+https://github.com/rust-lang/crates.io-index"},
    "zvariant": {"registry+https://github.com/rust-lang/crates.io-index"},
    "atspi-common": {"registry+https://github.com/rust-lang/crates.io-index"},
    "notify-rust": {"registry+https://github.com/rust-lang/crates.io-index"},
    "kettle-ui": {None},
    "accesskit_unix": {"registry+https://github.com/rust-lang/crates.io-index"},
    "kettle": {None},
    "zbus-lockstep-macros": {"registry+https://github.com/rust-lang/crates.io-index"},
    "zbus_macros": {"registry+https://github.com/rust-lang/crates.io-index"},
    "accesskit_winit": {"registry+https://github.com/rust-lang/crates.io-index"},
    "zbus": {"registry+https://github.com/rust-lang/crates.io-index"},
    "zbus_xml": {"registry+https://github.com/rust-lang/crates.io-index"},
    "zbus-lockstep": {"registry+https://github.com/rust-lang/crates.io-index"},
}
WORKSPACE_PACKAGES = {"kettle", "kettle-ui"}


def scope_errors(metadata):
    return check_scope(
        metadata,
        SEEDS,
        EXPECTED_EDGES,
        REVIEWED_VERSIONS,
        REVIEWED_SOURCES,
        WORKSPACE_PACKAGES,
    )


def main():
    result = subprocess.run(
        metadata_command(),
        cwd=Path(__file__).resolve().parent.parent,
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode:
        print(result.stderr, file=sys.stderr)
        return 1
    try:
        errors = scope_errors(json.loads(result.stdout))
    except (ValueError, TypeError) as error:
        errors = [str(error)]
    if errors:
        print(
            "::error::D-Bus maintenance exception scope changed. Re-review #633-635.",
            file=sys.stderr,
        )
        for error in errors:
            print(f"- {error}", file=sys.stderr)
        return 1
    print(
        "D-Bus scope OK: exact reviewed versions, sources and all-target reverse graph."
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
