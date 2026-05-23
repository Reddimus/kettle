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
MAN_DIR="${PREFIX}/share/man/man1"

if [[ "${UNINSTALL}" -eq 1 ]]; then
  echo "Removing kettle from ${PREFIX}…"
  rm -f \
    "${BIN_DIR}/kettle" \
    "${APP_DIR}/kettle.desktop" \
    "${MAN_DIR}/kettle.1" \
    "${ICON_BASE}/scalable/apps/kettle.svg" \
    "${ICON_BASE}/32x32/apps/kettle.png" \
    "${ICON_BASE}/48x48/apps/kettle.png" \
    "${ICON_BASE}/64x64/apps/kettle.png" \
    "${ICON_BASE}/128x128/apps/kettle.png" \
    "${ICON_BASE}/256x256/apps/kettle.png" \
    "${PREFIX}/share/kettle/install.sh"
  # Cycle 531: remove ${PREFIX}/share/kettle/ if it ends up empty
  # after the install.sh copy is gone. `rmdir` is non-recursive +
  # only succeeds on empty dirs — so a future addition (e.g.,
  # `${PREFIX}/share/kettle/themes/`) wouldn't be silently nuked,
  # but the bare cycle-530 dir gets cleaned up cleanly. Failure
  # is harmless: a user with extra files in there keeps them.
  rmdir "${PREFIX}/share/kettle" 2>/dev/null || true
  if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "${APP_DIR}" 2>/dev/null || true
  fi
  if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -f -t "${ICON_BASE}" 2>/dev/null || true
  fi
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

# 3b) Man page (cycle 279) — `man kettle` works after install if
# /usr/share/man/<...>/man1 (or the user's $MANPATH) is searched. Many
# distros pre-include ~/.local/share/man via /etc/manpath.config; if
# not, the user can `export MANPATH=~/.local/share/man:$MANPATH`.
MAN_SRC="${REPO_ROOT}/packaging/linux/kettle.1"
if [[ -f "${MAN_SRC}" ]]; then
  install -Dm644 "${MAN_SRC}" "${MAN_DIR}/kettle.1"
fi

# 3c) Cycle 530: drop a fresh copy of this install.sh into
# ${PREFIX}/share/kettle/ so `${PREFIX}/share/kettle/install.sh
# --uninstall` always points at the version that matched the
# binary. Without this, a contributor running `scripts/install.sh`
# from the repo would leave any pre-existing
# ${PREFIX}/share/kettle/install.sh stale (e.g., from the cycle-
# 253 tarball-install flow), and a later `--uninstall` would run
# a different version of the script than the binary it's removing.
# Works in both tarball and repo modes: ${SCRIPT_DIR}/install.sh
# = the script that's currently running.
install -Dm755 "${SCRIPT_DIR}/install.sh" "${PREFIX}/share/kettle/install.sh"

# 4) Refresh caches so GNOME/KDE pick the new entry up immediately.
# Both tools no-op silently if absent.
if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database "${APP_DIR}" 2>/dev/null || true
fi
# Cycle 540: only run gtk-update-icon-cache when the target dir has
# an index.theme file. Without one, gtk-update-icon-cache prints "No
# theme index file." (suppressed by 2>/dev/null) but still creates
# an empty/broken cache file (~584 bytes). That broken cache stops
# GNOME Shell from falling back to file-system icon scanning, so
# the kettle icon doesn't show in the Activities Super-key search
# even though the PNG/SVG files are correctly in place.
#
# The user-local hicolor dir (${PREFIX}/share/icons/hicolor/) is
# meant to inherit the system /usr/share/icons/hicolor/index.theme;
# no per-user index needs to exist. Skipping the cache call here
# lets GNOME do directory scanning, which finds kettle.{png,svg}
# correctly. If a future use case adds a custom theme with its own
# index.theme, this guard lets the cache call fire for that theme.
if command -v gtk-update-icon-cache >/dev/null 2>&1 \
    && [ -f "${ICON_BASE}/index.theme" ]; then
  gtk-update-icon-cache -f "${ICON_BASE}" 2>/dev/null || true
fi
# Clean up the broken empty cache cycle-253-era installs left in
# ${ICON_BASE}/icon-theme.cache (only safe to remove when the dir
# has no index.theme — otherwise it's a real cache for a real theme).
if [ ! -f "${ICON_BASE}/index.theme" ] && [ -f "${ICON_BASE}/icon-theme.cache" ]; then
  rm -f "${ICON_BASE}/icon-theme.cache"
fi

cat <<MSG

✓ kettle installed.

    binary  : ${BIN_DIR}/kettle
    desktop : ${APP_DIR}/kettle.desktop
    icons   : ${ICON_BASE}/{scalable,256x256,…}/apps/kettle.{svg,png}
    man page: ${MAN_DIR}/kettle.1   (try: man kettle)

Open the GNOME Activities overview (Super key) and type "kettle" to
launch it. If the entry doesn't appear immediately, log out and back
in once so the desktop database refresh takes effect.

Make sure ${BIN_DIR} is on your PATH:
    export PATH="${BIN_DIR}:\$PATH"

Three optional one-liners to finish setting things up:

    # Bootstrap a fully commented starter config:
    kettle --print-default-config > ~/.config/kettle/config

    # Enable OSC 133 jump-to-prompt (Ctrl+Up / Ctrl+Down in kettle):
    kettle --shell-integration bash >> ~/.bashrc      # or zsh / fish

    # Tab-complete every kettle CLI flag (kettle --li<TAB>):
    kettle --print-completions bash >> ~/.bashrc      # or zsh / fish

To uninstall: ./scripts/install.sh --uninstall
MSG
