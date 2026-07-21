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
#   ~/.local/share/icons/hicolor/<NNN>x<NNN>/apps/kettle.png  (16,24,32,48,64,128,256)
#
# Usage (from the repo root):
#   ./scripts/install.sh           # cargo build --release && install
#   ./scripts/install.sh --skip-build   # use an existing target/release/kettle
#   ./scripts/install.sh --record-dir=$HOME/.cache/kettle/records
#                                 # source checkout only: build with dev-record
#                                 # and record launcher sessions
#   ./scripts/install.sh --prefix=/usr  # system install (needs sudo / writable prefix)
#
# Uninstall:
#   ./scripts/install.sh --uninstall
#
# After install, log out and back in (or run `update-desktop-database
# ~/.local/share/applications/ 2>/dev/null || true`) and search "kettle"
# from the Super key.

set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
PREFIX="${HOME}/.local"
PREFIX_ARG_SET=0
SKIP_BUILD=0
UNINSTALL=0
RECORD_DIR=""
RECORD_DIR_ARG_SET=0

for arg in "$@"; do
  case "$arg" in
    --prefix=*) PREFIX="${arg#--prefix=}"; PREFIX_ARG_SET=1 ;;
    --skip-build) SKIP_BUILD=1 ;;
    --record-dir=*) RECORD_DIR="${arg#--record-dir=}"; RECORD_DIR_ARG_SET=1 ;;
    --uninstall) UNINSTALL=1 ;;
    -h|--help)
      sed -n '2,/^set/p' "$0" | sed 's/^# \{0,1\}//;/^set/d'
      exit 0 ;;
    *) echo "unknown arg: $arg" >&2; exit 2 ;;
  esac
done

if [[ "${RECORD_DIR_ARG_SET}" -eq 1 && -z "${RECORD_DIR}" ]]; then
  echo "error: --record-dir requires a non-empty directory" >&2
  exit 2
fi

# If this script is the installed helper at <prefix>/share/kettle/install.sh,
# default to that prefix. This keeps `.../share/kettle/install.sh --uninstall`
# symmetrical for custom-prefix installs produced by install-online.sh. Repo and
# tarball layouts do not match the `/share/kettle` suffix, so they keep ~/.local.
if [[ "${PREFIX_ARG_SET}" -eq 0 \
    && "$(basename -- "${SCRIPT_DIR}")" == "kettle" \
    && "$(basename -- "$(dirname -- "${SCRIPT_DIR}")")" == "share" ]]; then
  PREFIX=$(cd -- "${SCRIPT_DIR}/../.." && pwd)
fi

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
if [[ -x "${SCRIPT_DIR}/kettle" && -d "${SCRIPT_DIR}/packaging/linux" ]]; then
  TARBALL_MODE=1
  REPO_ROOT="${SCRIPT_DIR}"
  BIN_SRC="${SCRIPT_DIR}/kettle"
else
  TARBALL_MODE=0
  REPO_ROOT=$(cd -- "${SCRIPT_DIR}/.." && pwd)
  BIN_SRC="${REPO_ROOT}/target/release/kettle"
fi

if [[ "${TARBALL_MODE}" -eq 1 && -n "${RECORD_DIR}" ]]; then
  echo "error: --record-dir requires a source checkout and a dev-record feature build" >&2
  exit 2
fi

# Desktop files require absolute executable/icon paths. Resolve a relative
# prefix against the invocation directory while preserving its punctuation.
if [[ "${PREFIX}" != /* ]]; then
  PREFIX="${PWD}/${PREFIX}"
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
    "${ICON_BASE}/16x16/apps/kettle.png" \
    "${ICON_BASE}/24x24/apps/kettle.png" \
    "${ICON_BASE}/32x32/apps/kettle.png" \
    "${ICON_BASE}/48x48/apps/kettle.png" \
    "${ICON_BASE}/64x64/apps/kettle.png" \
    "${ICON_BASE}/128x128/apps/kettle.png" \
    "${ICON_BASE}/256x256/apps/kettle.png" \
    "${PREFIX}/share/kettle/install.sh" \
    "${PREFIX}/share/kettle/install.json" \
    "${PREFIX}/share/kettle/install-real.sh"
  rm -rf "${PREFIX}/share/kettle/shell-integration"
  # Remove ${PREFIX}/share/kettle/ if it ends up empty
  # after the install.sh copy is gone. `rmdir` is non-recursive +
  # only succeeds on empty dirs — so a future addition (e.g.,
  # `${PREFIX}/share/kettle/themes/`) wouldn't be silently nuked,
  # but the bare directory gets cleaned up cleanly. Failure
  # is harmless: a user with extra files in there keeps them.
  rmdir "${PREFIX}/share/kettle" 2>/dev/null || true
  if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "${APP_DIR}" 2>/dev/null || true
  fi
  # Symmetric to the install-side icon-cache guard below —
  # if ${ICON_BASE} has no index.theme (the user-local hicolor
  # case), only rebuild the cache when there IS a theme to cache.
  # Otherwise the cache reference goes stale referencing now-
  # removed icons. Remove the broken cache instead so GNOME falls
  # back to file-system scanning.
  if command -v gtk-update-icon-cache >/dev/null 2>&1 \
      && [ -f "${ICON_BASE}/index.theme" ]; then
    gtk-update-icon-cache -f "${ICON_BASE}" 2>/dev/null || true
  fi
  if [ ! -f "${ICON_BASE}/index.theme" ] && [ -f "${ICON_BASE}/icon-theme.cache" ]; then
    rm -f "${ICON_BASE}/icon-theme.cache"
  fi
  echo "  removed."
  exit 0
fi

if [[ "${TARBALL_MODE}" -eq 0 && "${SKIP_BUILD}" -eq 0 ]]; then
  echo "Building kettle (release)…"
  if [[ -n "${RECORD_DIR}" ]]; then
    ( cd "${REPO_ROOT}" && cargo build --release -p kettle --features dev-record )
  else
    ( cd "${REPO_ROOT}" && cargo build --release -p kettle )
  fi
fi

if [[ ! -x "${BIN_SRC}" ]]; then
  if [[ "${TARBALL_MODE}" -eq 1 ]]; then
    echo "error: tarball is missing the bundled kettle binary at ${BIN_SRC}" >&2
  else
    echo "error: ${BIN_SRC} not found (did you skip the build but never run cargo build --release?)" >&2
  fi
  exit 1
fi

validate_desktop_value() {
  local label=$1
  local value=$2
  if [[ "${value}" == *$'\n'* ]] \
      || printf '%s' "${value}" | LC_ALL=C grep -q '[[:cntrl:]]'; then
    echo "error: ${label} contains a control character unsupported by Desktop Entry files" >&2
    exit 1
  fi
  if command -v iconv >/dev/null 2>&1 \
      && ! printf '%s' "${value}" | iconv -f UTF-8 -t UTF-8 >/dev/null 2>&1; then
    echo "error: ${label} is not valid UTF-8" >&2
    exit 1
  fi
}
validate_desktop_value "install prefix" "${PREFIX}"
validate_desktop_value "recording directory" "${RECORD_DIR}"
if [[ "${BIN_DIR}/kettle" == *"="* ]]; then
  echo "error: Desktop Entry executable paths cannot contain '=': ${BIN_DIR}/kettle" >&2
  exit 1
fi

if [[ -n "${RECORD_DIR}" ]]; then
  BIN_HELP=$("${BIN_SRC}" --help 2>/dev/null) || {
    echo "error: could not inspect the selected kettle binary for dev-record support" >&2
    exit 1
  }
  if ! grep -q -- '--record-dir' <<<"${BIN_HELP}"; then
    echo "error: --record-dir requires a kettle binary built with --features dev-record" >&2
    echo "       rebuild it or omit --skip-build" >&2
    exit 1
  fi
  if [[ -L "${RECORD_DIR}" ]]; then
    echo "error: recording directory must not be a symbolic link: ${RECORD_DIR}" >&2
    exit 1
  fi
  if [[ -e "${RECORD_DIR}" && ! -d "${RECORD_DIR}" ]]; then
    echo "error: recording path is not a directory: ${RECORD_DIR}" >&2
    exit 1
  fi
  mkdir -p -- "${RECORD_DIR}"
  if [[ -L "${RECORD_DIR}" || ! -d "${RECORD_DIR}" ]]; then
    echo "error: recording directory changed while it was being created: ${RECORD_DIR}" >&2
    exit 1
  fi
  if ! exec {RECORD_DIR_FD}<"${RECORD_DIR}"; then
    echo "error: could not open recording directory: ${RECORD_DIR}" >&2
    exit 1
  fi
  RECORD_DIR_HANDLE="/proc/$$/fd/${RECORD_DIR_FD}"
  RECORD_DIR_ID=$(stat -Lc '%d:%i' -- "${RECORD_DIR}")
  RECORD_DIR_HANDLE_ID=$(stat -Lc '%d:%i' -- "${RECORD_DIR_HANDLE}")
  if [[ -L "${RECORD_DIR}" || ! -d "${RECORD_DIR_HANDLE}" || "${RECORD_DIR_ID}" != "${RECORD_DIR_HANDLE_ID}" ]]; then
    exec {RECORD_DIR_FD}<&-
    echo "error: recording directory changed while it was being opened: ${RECORD_DIR}" >&2
    exit 1
  fi
  chmod 700 -- "${RECORD_DIR_HANDLE}"
  RECORD_DIR=$(readlink -f -- "${RECORD_DIR_HANDLE}")
  exec {RECORD_DIR_FD}<&-
fi

echo "Installing into ${PREFIX}…"

# 1) Binary.
install -Dm755 "${BIN_SRC}" "${BIN_DIR}/kettle"

# 2) XDG desktop entry. The packaging file ships relative `Exec` /
# `TryExec` plus themed `Icon=kettle` (the shape distro packages rely
# on, since the package manager keeps PATH + icon-theme.cache fresh).
# For this no-sudo *user* install we rewrite them to exact installed
# paths — see the note below the icon copy for the full why.
install -Dm644 "${REPO_ROOT}/packaging/linux/kettle.desktop" "${APP_DIR}/kettle.desktop"

# 3) Icons.
install -Dm644 "${REPO_ROOT}/packaging/linux/kettle.svg"     "${ICON_BASE}/scalable/apps/kettle.svg"
for size in 16 24 32 48 64 128 256; do
  src="${REPO_ROOT}/packaging/linux/kettle-${size}.png"
  if [[ -f "${src}" ]]; then
    install -Dm644 "${src}" "${ICON_BASE}/${size}x${size}/apps/kettle.png"
  fi
done

# 3a) Point the desktop entry at the exact user-installed paths.
#
# GNOME Shell's StIconTheme does NOT resolve a *themed* icon name
# (`Icon=kettle`) from a user-local hicolor dir that has no
# icon-theme.cache — so the Super-key / Activities search showed a blank
# tile even though the PNGs were correctly in place and 8-bit (verified:
# `Gtk.IconTheme` resolves + loads `kettle`, but gnome-shell does not).
# An earlier change removed the gtk-update-icon-cache call to avoid leaving a
# *broken* cache, but its assumption that GNOME would then directory-scan
# the icon by name was wrong for gnome-shell. An absolute path sidesteps
# icon-theme resolution entirely: the launcher icon renders regardless of
# cache state, and — unlike generating a user-level cache — it can't go
# stale and hide other apps' icons (the same footgun as before). The
# `#` sed delimiter avoids clashing with the `/`s in the path. System
# (`--prefix=/usr`) installs get a valid absolute path too; distro
# packages keep the themed `Icon=kettle` since their post-install hooks
# maintain the system hicolor cache.
ICON_ABS="${ICON_BASE}/256x256/apps/kettle.png"
BIN_ABS="${BIN_DIR}/kettle"
sed_repl() {
  printf '%s' "$1" | sed 's/[\\&#]/\\&/g'
}
desktop_string_escape() {
  # Desktop Entry string/iconstring values decode their own backslash escape
  # layer before Exec argument parsing. Encode that layer independently.
  printf '%s' "$1" | sed 's/\\/\\\\/g'
}
desktop_exec_quote() {
  # First escape the quoted Exec argument grammar, then encode the surrounding
  # string-value grammar. One literal backslash therefore becomes four in the
  # desktop file, as required by the Desktop Entry specification.
  local escaped
  # These sed replacements intentionally contain literals.
  # shellcheck disable=SC2016
  escaped=$(printf '%s' "$1" | sed \
    -e 's/\\/\\\\/g' \
    -e 's/"/\\"/g' \
    -e 's/`/\\`/g' \
    -e 's/\$/\\$/g' \
    -e 's/%/%%/g')
  printf '"%s"' "$(desktop_string_escape "${escaped}")"
}
require_desktop_template_line() {
  local expected=$1
  local count
  count=$(awk -v expected="${expected}" '$0 == expected { count++ } END { print count + 0 }' "${APP_DIR}/kettle.desktop")
  if [[ "${count}" -ne 1 ]]; then
    echo "error: desktop template must contain exactly one ${expected} entry (found ${count})" >&2
    exit 1
  fi
}
require_desktop_template_line 'Exec=kettle'
require_desktop_template_line 'TryExec=kettle'
require_desktop_template_line 'Icon=kettle'
BIN_EXEC=$(desktop_exec_quote "${BIN_ABS}")
BIN_REPL=$(sed_repl "${BIN_EXEC}")
BIN_VALUE_REPL=$(sed_repl "$(desktop_string_escape "${BIN_ABS}")")
ICON_REPL=$(sed_repl "$(desktop_string_escape "${ICON_ABS}")")
sed -i "s#^Exec=kettle\$#Exec=${BIN_REPL}#" "${APP_DIR}/kettle.desktop"
sed -i "s#^TryExec=kettle\$#TryExec=${BIN_VALUE_REPL}#" "${APP_DIR}/kettle.desktop"
sed -i "s#^Icon=kettle\$#Icon=${ICON_REPL}#" "${APP_DIR}/kettle.desktop"
if [[ -n "${RECORD_DIR}" ]]; then
  RECORD_ARG=$(desktop_exec_quote "KETTLE_RECORD_DIR=${RECORD_DIR}")
  RECORD_REPL=$(sed_repl "${RECORD_ARG}")
  sed -i "s#^Exec=.*\$#Exec=/usr/bin/env ${RECORD_REPL} ${BIN_REPL}#" "${APP_DIR}/kettle.desktop"
fi

# 3b) Man page — `man kettle` works after install if
# /usr/share/man/<...>/man1 (or the user's $MANPATH) is searched. Many
# distros pre-include ~/.local/share/man via /etc/manpath.config; if
# not, the user can `export MANPATH=~/.local/share/man:$MANPATH`.
MAN_SRC="${REPO_ROOT}/packaging/linux/kettle.1"
if [[ -f "${MAN_SRC}" ]]; then
  install -Dm644 "${MAN_SRC}" "${MAN_DIR}/kettle.1"
fi

# 3c) Drop a fresh copy of this install.sh into
# ${PREFIX}/share/kettle/ so `${PREFIX}/share/kettle/install.sh
# --uninstall` always points at the version that matched the
# binary. Without this, a contributor running `scripts/install.sh`
# from the repo would leave any pre-existing
# ${PREFIX}/share/kettle/install.sh stale (e.g., from an earlier
# tarball-install flow), and a later `--uninstall` would run
# a different version of the script than the binary it's removing.
# Works in both tarball and repo modes: ${SCRIPT_DIR}/install.sh
# = the script that's currently running.
install -Dm755 "${SCRIPT_DIR}/install.sh" "${PREFIX}/share/kettle/install.sh"

# Keep the shipped prompt-integration snippets beside the installed helper so
# an authenticated update refreshes the same self-contained layout.
if [[ -d "${REPO_ROOT}/shell-integration" ]]; then
  install -d "${PREFIX}/share/kettle/shell-integration"
  for snippet in "${REPO_ROOT}"/shell-integration/*; do
    [[ -f "${snippet}" ]] || continue
    install -m644 "${snippet}" "${PREFIX}/share/kettle/shell-integration/$(basename -- "${snippet}")"
  done
fi

# Explicit ownership marker consumed by `kettle update`. Distro packages,
# cargo installs, and manually copied binaries have no marker and are refused.
case "$(uname -m)" in
  x86_64|amd64) UPDATE_TARGET="x86_64-unknown-linux-gnu" ;;
  aarch64|arm64) UPDATE_TARGET="aarch64-unknown-linux-gnu" ;;
  *) UPDATE_TARGET="unsupported" ;;
esac
KETTLE_VERSION=$("${BIN_DIR}/kettle" --version 2>/dev/null | awk 'NR == 1 { print $2 }')
KETTLE_VERSION=${KETTLE_VERSION:-unknown}
if [[ -n "${RECORD_DIR}" ]]; then
  INSTALL_CHANNEL="local-dev-record"
elif [[ "${TARBALL_MODE}" -eq 1 ]]; then
  INSTALL_CHANNEL="stable"
else
  INSTALL_CHANNEL="local-dev"
fi
MARKER_PATH="${PREFIX}/share/kettle/install.json"
MARKER_TMP=$(mktemp "${PREFIX}/share/kettle/.install.json.tmp.XXXXXX")
trap 'rm -f -- "${MARKER_TMP:-}"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
chmod 600 "${MARKER_TMP}"
cat > "${MARKER_TMP}" <<EOF
{
  "schema": 1,
  "product": "kettle",
  "managed_by": "kettle-installer",
  "channel": "${INSTALL_CHANNEL}",
  "target": "${UPDATE_TARGET}",
  "version": "${KETTLE_VERSION}"
}
EOF
chmod 644 "${MARKER_TMP}"
# Rename replaces a pre-existing leaf symlink instead of following it. `-T`
# also refuses to reinterpret a malicious directory at the marker path as a
# destination directory.
mv -fT -- "${MARKER_TMP}" "${MARKER_PATH}"
MARKER_TMP=""
trap - EXIT INT TERM

# 4) Refresh caches so GNOME/KDE pick the new entry up immediately.
# Both tools no-op silently if absent.
if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database "${APP_DIR}" 2>/dev/null || true
fi
# Only run gtk-update-icon-cache when the target dir has
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
# Clean up the broken empty cache that installs predating this guard left in
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

To uninstall: ${PREFIX}/share/kettle/install.sh --uninstall
MSG
