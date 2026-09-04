#!/usr/bin/env bash
# kettle — compatibility entry point for the canonical icon generator.
#
# The old shell implementation rasterized SVGs with librsvg while the Windows
# path reproduced their geometry with Pillow. Those renderers disagree at every
# resolution, so running the documented shell workflow produced files rejected
# by CI. One Python implementation now owns the geometry, emits both SVGs, and
# renders every platform artifact.
#
# WHY THIS EXISTS (the Super-key icon bug, v2.1.1): the committed PNGs
# were once 16-bit/color RGBA. GNOME Shell's icon loader silently fails
# on 16-bit PNGs, so the kettle tile showed blank in the Ubuntu Super-key
# / Activities search even though the files were correctly installed. The
# freedesktop icon spec and every desktop loader expect 8-bit/color RGBA.
# The canonical generator emits 8-bit PNGs, fixing the depth while keeping
# every size reproducible from one geometry model.
#
# Dependency: Python 3 + Pillow (`python3 -m pip install Pillow`).
#
# Usage (from anywhere):
#   ./scripts/gen-icons.sh
#
set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
exec python3 "${SCRIPT_DIR}/gen-icons.py" "$@"
