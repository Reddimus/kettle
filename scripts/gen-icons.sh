#!/usr/bin/env bash
# kettle — compatibility entry point for the canonical icon generator.
#
# One Python implementation owns the geometry, emits both SVGs, and renders
# every platform artifact. librsvg and Pillow disagree at every resolution, so
# a second renderer would produce files CI rejects.
#
# The generator emits 8-bit/color RGBA PNGs. GNOME Shell's icon loader silently
# fails on 16-bit PNGs, which leaves the kettle tile blank in the Ubuntu
# Super-key / Activities search even when the files are correctly installed.
# The freedesktop icon spec and every desktop loader expect 8-bit/color RGBA.
#
# Dependency: Python 3 + Pillow (`python3 -m pip install Pillow`).
#
# Usage (from anywhere):
#   ./scripts/gen-icons.sh
#
set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
exec python3 "${SCRIPT_DIR}/gen-icons.py" "$@"
