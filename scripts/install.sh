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
#                                 # auto-record every launcher session into DIR
#                                 # (wires KETTLE_RECORD_DIR into the .desktop
#                                 # entry; recording ships in every build). The
#                                 # same effect is available via `record = on`
#                                 # + `record-dir` in the config file.
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

# The `--record-dir` launcher wiring only survives on a source (`local-dev`)
# install: it lives in the .desktop `Exec=` line, which the signed self-updater
# regenerates from the pristine template on every update (dropping the env var).
# A release-tarball install is `stable` and self-updates, so refuse the combo and
# steer to the config key `record = on`, which lives in the config file and DOES
# survive updates. (Recording ships in every build regardless.)
if [[ "${TARBALL_MODE}" -eq 1 && -n "${RECORD_DIR}" ]]; then
  echo "error: --record-dir wires recording into the desktop launcher, which a" >&2
  echo "       self-updating release install would drop on its next update." >&2
  echo "       Set 'record = on' (and optionally 'record-dir = <path>') in your" >&2
  echo "       kettle config instead — that survives updates. See docs/RECORDING.md." >&2
  exit 2
fi

# Desktop files require absolute executable/icon paths. Resolve a relative
# prefix lexically; the descriptor-relative helper rejects aliases, symlinks,
# and unsafe ownership/modes before it mutates the target.
if [[ "${PREFIX}" != /* ]]; then
  PREFIX="${PWD}/${PREFIX}"
fi
while [[ "${PREFIX}" != "/" && "${PREFIX}" == */ ]]; do
  PREFIX=${PREFIX%/}
done

BIN_DIR="${PREFIX}/bin"
APP_DIR="${PREFIX}/share/applications"
ICON_BASE="${PREFIX}/share/icons/hicolor"
MAN_DIR="${PREFIX}/share/man/man1"
HELPER_SRC="${SCRIPT_DIR}/install-unix.py"

if ! command -v python3 >/dev/null 2>&1; then
  echo "error: the hardened Linux installer requires Python 3" >&2
  exit 1
fi
if [[ ! -f "${HELPER_SRC}" || -L "${HELPER_SRC}" ]]; then
  echo "error: hardened installer helper is missing or is a symlink: ${HELPER_SRC}" >&2
  exit 1
fi

if [[ "${UNINSTALL}" -eq 1 ]]; then
  echo "Removing kettle from ${PREFIX}…"
  # The helper preflights the complete recorded set before unlinking anything.
  # A legacy install without install-files.json, a changed file, or any
  # no-follow/ownership/mode failure is intentionally refused.
  python3 "${HELPER_SRC}" uninstall --prefix "${PREFIX}"
  if command -v update-desktop-database >/dev/null 2>&1 \
      && [[ -d "${APP_DIR}" && ! -L "${APP_DIR}" ]]; then
    update-desktop-database "${APP_DIR}" 2>/dev/null || true
  fi
  if command -v gtk-update-icon-cache >/dev/null 2>&1 \
      && [[ -d "${ICON_BASE}" && ! -L "${ICON_BASE}" ]] \
      && [[ -f "${ICON_BASE}/index.theme" && ! -L "${ICON_BASE}/index.theme" ]]; then
    gtk-update-icon-cache -f "${ICON_BASE}" 2>/dev/null || true
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

case "$(uname -m)" in
  x86_64|amd64) UPDATE_TARGET="x86_64-unknown-linux-gnu" ;;
  aarch64|arm64) UPDATE_TARGET="aarch64-unknown-linux-gnu" ;;
  *) UPDATE_TARGET="unsupported" ;;
esac
version_output=''
if version_output=$("${BIN_SRC}" --version 2>&1); then
  :
else
  probe_status=$?
  echo "error: ${BIN_SRC} exists but cannot run; refusing to install an unusable binary:" >&2
  if [[ -n "${version_output}" ]]; then
    printf '%s\n' "${version_output}" >&2
  fi
  exit "${probe_status}"
fi
KETTLE_VERSION=$(printf '%s\n' "${version_output}" | awk 'NR == 1 { print $2 }')
KETTLE_VERSION=${KETTLE_VERSION:-unknown}
if [[ "${TARBALL_MODE}" -eq 1 ]]; then
  INSTALL_CHANNEL="stable"
else
  INSTALL_CHANNEL="local-dev"
fi

INSTALL_FILES=(
  --file "bin/kettle" "0755" "${BIN_SRC}"
  --file "share/kettle/install.sh" "0755" "${SCRIPT_DIR}/install.sh"
  --file "share/kettle/install-unix.py" "0755" "${HELPER_SRC}"
  --file "share/icons/hicolor/scalable/apps/kettle.svg" "0644" "${REPO_ROOT}/packaging/linux/kettle.svg"
)
for size in 16 24 32 48 64 128 256; do
  src="${REPO_ROOT}/packaging/linux/kettle-${size}.png"
  if [[ -f "${src}" && ! -L "${src}" ]]; then
    INSTALL_FILES+=(
      --file "share/icons/hicolor/${size}x${size}/apps/kettle.png" "0644" "${src}"
    )
  fi
done
MAN_SRC="${REPO_ROOT}/packaging/linux/kettle.1"
if [[ -f "${MAN_SRC}" && ! -L "${MAN_SRC}" ]]; then
  INSTALL_FILES+=(--file "share/man/man1/kettle.1" "0644" "${MAN_SRC}")
fi
for shell_name in bash fish ps1 zsh; do
  snippet="${REPO_ROOT}/shell-integration/kettle.${shell_name}"
  if [[ -f "${snippet}" && ! -L "${snippet}" ]]; then
    INSTALL_FILES+=(
      --file "share/kettle/shell-integration/kettle.${shell_name}" "0644" "${snippet}"
    )
  fi
done

echo "Installing into ${PREFIX}…"
python3 "${HELPER_SRC}" install \
  --prefix "${PREFIX}" \
  --channel "${INSTALL_CHANNEL}" \
  --target "${UPDATE_TARGET}" \
  --version "${KETTLE_VERSION}" \
  --desktop-template "${REPO_ROOT}/packaging/linux/kettle.desktop" \
  --desktop-binary "${BIN_DIR}/kettle" \
  --desktop-icon "${ICON_BASE}/256x256/apps/kettle.png" \
  --record-dir "${RECORD_DIR}" \
  "${INSTALL_FILES[@]}"

# Refresh only shared caches. They are not Kettle-owned leaves and therefore
# are never added to, or removed by, the provenance manifest.
if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database "${APP_DIR}" 2>/dev/null || true
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1 \
    && [[ -f "${ICON_BASE}/index.theme" && ! -L "${ICON_BASE}/index.theme" ]]; then
  gtk-update-icon-cache -f "${ICON_BASE}" 2>/dev/null || true
fi

cat <<MSG

Kettle installed with no-follow path validation and recorded provenance.

    binary  : ${BIN_DIR}/kettle
    desktop : ${APP_DIR}/kettle.desktop
    icons   : ${ICON_BASE}/{scalable,256x256,...}/apps/kettle.{svg,png}
    man page: ${MAN_DIR}/kettle.1   (try: man kettle)

Open the GNOME Activities overview (Super key) and type "kettle" to
launch it. If the entry doesn't appear immediately, log out and back
in once so the desktop database refresh takes effect.

Make sure ${BIN_DIR} is on your PATH:
    export PATH="${BIN_DIR}:\$PATH"

Three optional one-liners to finish setting things up:

    kettle --print-default-config > ~/.config/kettle/config
    kettle --shell-integration bash >> ~/.bashrc      # or zsh / fish
    kettle --print-completions bash >> ~/.bashrc      # or zsh / fish

To uninstall: ${PREFIX}/share/kettle/install.sh --uninstall
MSG
