#!/usr/bin/env bash
# kettle — regenerate the Linux hicolor PNG icons from the SVG.
#
# `packaging/linux/kettle.svg` is the single source of truth for the
# launcher / window icon. This script rasterizes it to the fixed-size
# PNGs that ship in the hicolor theme (and get embedded as the winit
# window icon). It is idempotent — re-run it after editing the SVG.
#
# WHY THIS EXISTS (the Super-key icon bug, v2.1.1): the committed PNGs
# were once 16-bit/color RGBA. GNOME Shell's icon loader silently fails
# on 16-bit PNGs, so the kettle tile showed blank in the Ubuntu Super-key
# / Activities search even though the files were correctly installed. The
# freedesktop icon spec and every desktop loader expect 8-bit/color RGBA.
# rsvg-convert emits 8-bit PNGs, so rasterizing from the vector both fixes
# the depth and keeps every size pixel-crisp from one source.
#
# Dependency: `rsvg-convert` (Debian/Ubuntu: `apt install librsvg2-bin`;
# Fedora: `dnf install librsvg2-tools`; macOS: `brew install librsvg`).
#
# Usage (from anywhere):
#   ./scripts/gen-icons.sh
#
# After running, `file packaging/linux/kettle-*.png` should report
# "8-bit/color RGBA" for every size.

set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
LINUX_DIR="${SCRIPT_DIR}/../packaging/linux"
SVG="${LINUX_DIR}/kettle.svg"

if ! command -v rsvg-convert >/dev/null 2>&1; then
  echo "error: rsvg-convert not found." >&2
  echo "  Debian/Ubuntu: sudo apt install librsvg2-bin" >&2
  echo "  Fedora:        sudo dnf install librsvg2-tools" >&2
  echo "  macOS:         brew install librsvg" >&2
  exit 1
fi

if [[ ! -f "${SVG}" ]]; then
  echo "error: source SVG not found at ${SVG}" >&2
  exit 1
fi

# 16/24 — GNOME panel + search-result list glyphs; 32–256 — launcher
# tiles / Alt-Tab / window icon. Square sizes only (the SVG viewBox is
# 512×512, so every output is 1:1).
SIZES=(16 24 32 48 64 128 256)

echo "Rasterizing ${SVG} → 8-bit PNGs:"
for size in "${SIZES[@]}"; do
  out="${LINUX_DIR}/kettle-${size}.png"
  # rsvg-convert emits 8-bit/color RGBA. No ImageMagick post-pass needed.
  rsvg-convert --width "${size}" --height "${size}" \
    --keep-aspect-ratio --format png \
    --output "${out}" "${SVG}"
  echo "  kettle-${size}.png"
done

echo
echo "✓ regenerated ${#SIZES[@]} icons. Verify depth with:"
echo "    file ${LINUX_DIR}/kettle-*.png"
