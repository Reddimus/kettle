#!/usr/bin/env bash
# kettle — Linux user-install (no sudo required)
#
# Builds the release binary and drops everything into the standard
# XDG user paths so the kettle entry shows up in the GNOME Activities
# overview, Ubuntu Super-key search, KDE Krunner, etc. — no system-wide
# changes, no `sudo`.
#
#   ~/.local/bin/kettle                          ← the binary
#   ~/.local/share/applications/kettle.desktop   ← XDG launcher entry
#   ~/.local/share/icons/hicolor/scalable/apps/kettle.svg
#   ~/.local/share/icons/hicolor/<NNN>x<NNN>/apps/kettle.png  (32,48,64,128,256)
#
# Usage (from the repo root):
#   ./scripts/install.sh           # cargo build --release && install
#   ./scripts/install.sh --skip-build   # use an existing target/release/kettle
#   ./scripts/install.sh --prefix=/usr  # system install (needs sudo / writable prefix)
#
# Uninstall:
#   ./scripts/install.sh --uninstall
#
# After install, log out and back in (or run `update-desktop-database
# ~/.local/share/applications/ 2>/dev/null || true`) and search "kettle"
# from the Super key.

set -euo pipefail

PREFIX="${HOME}/.local"
SKIP_BUILD=0
UNINSTALL=0

for arg in "$@"; do
  case "$arg" in
    --prefix=*) PREFIX="${arg#--prefix=}" ;;
    --skip-build) SKIP_BUILD=1 ;;
    --uninstall) UNINSTALL=1 ;;
    -h|--help)
      sed -n '2,/^set/p' "$0" | sed 's/^# \{0,1\}//;/^set/d'
      exit 0 ;;
    *) echo "unknown arg: $arg" >&2; exit 2 ;;
  esac
done

# The script runs in two layouts:
#
#  - in-tree repo:  scripts/install.sh  → binary at target/release/kettle,
#                                         assets at packaging/linux/.
#  - extracted tarball: kettle/install.sh → binary and packaging/linux/
#                                         live as siblings of the script.
#
# We detect tarball mode by looking for a `kettle` binary next to the
# script — that file only exists in the release tarball, never in the
# repo (the in-tree binary lives under target/release/).
SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
if [[ -x "${SCRIPT_DIR}/kettle" && -d "${SCRIPT_DIR}/packaging/linux" ]]; then
  TARBALL_MODE=1
  REPO_ROOT="${SCRIPT_DIR}"
  BIN_SRC="${SCRIPT_DIR}/kettle"
else
  TARBALL_MODE=0
  REPO_ROOT=$(cd -- "${SCRIPT_DIR}/.." && pwd)
  BIN_SRC="${REPO_ROOT}/target/release/kettle"
fi

BIN_DIR="${PREFIX}/bin"
APP_DIR="${PREFIX}/share/applications"
ICON_BASE="${PREFIX}/share/icons/hicolor"

if [[ "${UNINSTALL}" -eq 1 ]]; then
  echo "Removing kettle from ${PREFIX}…"
  rm -f \
    "${BIN_DIR}/kettle" \
    "${APP_DIR}/kettle.desktop" \
    "${ICON_BASE}/scalable/apps/kettle.svg" \
    "${ICON_BASE}/32x32/apps/kettle.png" \
    "${ICON_BASE}/48x48/apps/kettle.png" \
    "${ICON_BASE}/64x64/apps/kettle.png" \
    "${ICON_BASE}/128x128/apps/kettle.png" \
    "${ICON_BASE}/256x256/apps/kettle.png"
  command -v update-desktop-database >/dev/null 2>&1 \
    && update-desktop-database "${APP_DIR}" 2>/dev/null || true
  command -v gtk-update-icon-cache >/dev/null 2>&1 \
    && gtk-update-icon-cache -f -t "${ICON_BASE}" 2>/dev/null || true
  echo "  removed."
  exit 0
fi

if [[ "${TARBALL_MODE}" -eq 0 && "${SKIP_BUILD}" -eq 0 ]]; then
  echo "Building kettle (release)…"
  ( cd "${REPO_ROOT}" && cargo build --release -p kettle )
fi

if [[ ! -x "${BIN_SRC}" ]]; then
  if [[ "${TARBALL_MODE}" -eq 1 ]]; then
    echo "error: tarball is missing the bundled kettle binary at ${BIN_SRC}" >&2
  else
    echo "error: ${BIN_SRC} not found (did you skip the build but never run cargo build --release?)" >&2
  fi
  exit 1
fi

echo "Installing into ${PREFIX}…"

# 1) Binary.
install -Dm755 "${BIN_SRC}" "${BIN_DIR}/kettle"

# 2) XDG desktop entry — the file in packaging/linux/ already names
# `Icon=kettle` so it resolves against hicolor.
install -Dm644 "${REPO_ROOT}/packaging/linux/kettle.desktop" "${APP_DIR}/kettle.desktop"

# 3) Icons.
install -Dm644 "${REPO_ROOT}/packaging/linux/kettle.svg"     "${ICON_BASE}/scalable/apps/kettle.svg"
for size in 32 48 64 128 256; do
  src="${REPO_ROOT}/packaging/linux/kettle-${size}.png"
  if [[ -f "${src}" ]]; then
    install -Dm644 "${src}" "${ICON_BASE}/${size}x${size}/apps/kettle.png"
  fi
done

# 4) Refresh caches so GNOME/KDE pick the new entry up immediately.
# Both tools no-op silently if absent.
command -v update-desktop-database >/dev/null 2>&1 \
  && update-desktop-database "${APP_DIR}" 2>/dev/null || true
command -v gtk-update-icon-cache >/dev/null 2>&1 \
  && gtk-update-icon-cache -f -t "${ICON_BASE}" 2>/dev/null || true

cat <<MSG

✓ kettle installed.

    binary  : ${BIN_DIR}/kettle
    desktop : ${APP_DIR}/kettle.desktop
    icons   : ${ICON_BASE}/{scalable,256x256,…}/apps/kettle.{svg,png}

Open the GNOME Activities overview (Super key) and type "kettle" to
launch it. If the entry doesn't appear immediately, log out and back
in once so the desktop database refresh takes effect.

Make sure ${BIN_DIR} is on your PATH:
    export PATH="${BIN_DIR}:\$PATH"

Two optional one-liners to finish setting things up:

    # Bootstrap a fully commented starter config:
    kettle --print-default-config > ~/.config/kettle/config

    # Enable OSC 133 jump-to-prompt (Ctrl+Up / Ctrl+Down in kettle):
    kettle --shell-integration bash >> ~/.bashrc      # or zsh / fish

To uninstall: ./scripts/install.sh --uninstall
MSG
