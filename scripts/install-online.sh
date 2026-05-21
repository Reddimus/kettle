#!/usr/bin/env sh
# kettle — one-line online installer for Linux
#
# Downloads the latest GitHub release tarball, extracts the prebuilt
# binary + XDG launcher + icons to a temp directory, and runs the
# bundled `install.sh --skip-build` to drop everything into the
# standard XDG user paths under `~/.local/`. No `sudo` required, no
# Rust toolchain required.
#
#   curl -fsSL https://raw.githubusercontent.com/Reddimus/kettle/main/scripts/install-online.sh | sh
#
# Or with a pinned version (recommended for reproducible installs):
#
#   curl -fsSL https://raw.githubusercontent.com/Reddimus/kettle/main/scripts/install-online.sh | KETTLE_VERSION=v1.3.2 sh
#
# Notes
# - Linux x86_64 only today. macOS users: grab the `.app` bundle from
#   https://github.com/Reddimus/kettle/releases/latest and drag it to
#   /Applications. Windows users: extract the zip and add to PATH.
# - The script uses `curl`, `tar`, and `gzip` — all standard on every
#   Linux distro we ship for. `gh` (GitHub CLI) is NOT required.
# - Verifies the downloaded tarball is non-empty and has a recognizable
#   gzip header before extracting — guards against partial / hijacked
#   downloads.
# - All work happens in a temp directory that's removed on exit (via
#   `trap`) regardless of success/failure.
# - To uninstall later: re-run `~/.local/share/kettle/install.sh
#   --uninstall` (the script copies a reference under share/ so the
#   uninstall path doesn't depend on the original temp dir).

set -eu

REPO="Reddimus/kettle"
VERSION="${KETTLE_VERSION:-latest}"
ASSET="kettle-linux-x86_64.tar.gz"

# --- Platform check ------------------------------------------------
case "$(uname -s)" in
  Linux) ;;
  Darwin)
    echo "kettle install-online.sh: macOS detected." >&2
    echo "Please download the .app bundle from:" >&2
    echo "  https://github.com/${REPO}/releases/latest" >&2
    exit 1
    ;;
  *)
    echo "kettle install-online.sh: unsupported OS '$(uname -s)'." >&2
    echo "Prebuilt binaries are available for Linux (x86_64), macOS (universal), and Windows." >&2
    echo "See https://github.com/${REPO}/releases/latest" >&2
    exit 1
    ;;
esac

case "$(uname -m)" in
  x86_64 | amd64) ;;
  *)
    echo "kettle install-online.sh: unsupported arch '$(uname -m)'." >&2
    echo "Prebuilt Linux binary is x86_64 only. Build from source via:" >&2
    echo "  git clone https://github.com/${REPO} && cd kettle && ./scripts/install.sh" >&2
    exit 1
    ;;
esac

# --- Required tools ------------------------------------------------
for cmd in curl tar uname mktemp; do
  if ! command -v "$cmd" >/dev/null 2>&1; then
    echo "kettle install-online.sh: missing required tool '$cmd'." >&2
    echo "Install it via your distro's package manager and re-run." >&2
    exit 1
  fi
done

# --- Resolve target version + URL ----------------------------------
if [ "$VERSION" = "latest" ]; then
  # The /releases/latest endpoint redirects to /releases/tag/<tag>.
  # `curl -sLI` follows redirects and dumps headers; grep the final
  # `location:` line for the tag. Bare-bones (no jq) so the script
  # has zero non-coreutils deps.
  RESOLVED=$(curl -fsSLI "https://github.com/${REPO}/releases/latest" 2>/dev/null \
    | awk 'tolower($1) == "location:" { print $2 }' \
    | tail -n1 \
    | sed -e 's|.*/tag/||' -e 's|[[:space:]]*$||')
  if [ -z "$RESOLVED" ]; then
    echo "kettle install-online.sh: could not resolve latest release tag." >&2
    echo "Tried https://github.com/${REPO}/releases/latest — got no Location header." >&2
    echo "Set KETTLE_VERSION=vX.Y.Z explicitly and re-run." >&2
    exit 1
  fi
  VERSION="$RESOLVED"
fi

URL="https://github.com/${REPO}/releases/download/${VERSION}/${ASSET}"
echo "kettle: installing ${VERSION} from ${URL}"

# --- Download to a temp dir ----------------------------------------
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT INT TERM

TAR="${TMP}/${ASSET}"
if ! curl -fL --progress-bar -o "$TAR" "$URL"; then
  echo "kettle install-online.sh: download failed." >&2
  echo "Check the version string '$VERSION' and try again." >&2
  echo "See available releases: https://github.com/${REPO}/releases" >&2
  exit 1
fi

# --- Sanity-check the downloaded file ------------------------------
# Non-empty + first two bytes are gzip's `1f 8b` magic. A redirect to
# an HTML error page would land here as ~1 KB of HTML, not gzip-
# magic'd binary — catch that before we try to tar -xz.
SIZE=$(wc -c < "$TAR")
if [ "$SIZE" -lt 1024 ]; then
  echo "kettle install-online.sh: downloaded file is only ${SIZE} bytes — likely an error response, not the tarball." >&2
  exit 1
fi
MAGIC=$(od -An -N2 -tx1 "$TAR" | tr -d ' \n')
if [ "$MAGIC" != "1f8b" ]; then
  echo "kettle install-online.sh: downloaded file is not a gzip archive (magic bytes: $MAGIC)." >&2
  echo "Got ${SIZE} bytes of something else — likely an HTML error page." >&2
  exit 1
fi

# --- Extract + run the bundled install.sh --------------------------
tar -C "$TMP" -xzf "$TAR"
if [ ! -x "$TMP/kettle/install.sh" ]; then
  echo "kettle install-online.sh: extracted tarball doesn't contain install.sh." >&2
  echo "This is likely a bug in the upstream release pipeline; please report at" >&2
  echo "  https://github.com/${REPO}/issues" >&2
  exit 1
fi

# `--skip-build` because the tarball already ships a release binary.
# Invoke via the script's own shebang (`#!/usr/bin/env bash`) — the
# bundled install.sh uses `set -euo pipefail`, a Bash-ism that fails
# under dash (Debian/Ubuntu `sh` is dash). The release.yml ships
# install.sh with mode 755 so the bare exec path works.
"$TMP/kettle/install.sh" --skip-build

# Stash a copy of install.sh under share/ so the user can later run
# `~/.local/share/kettle/install.sh --uninstall` without re-downloading.
mkdir -p "${HOME}/.local/share/kettle"
cp "$TMP/kettle/install.sh" "${HOME}/.local/share/kettle/install.sh"
chmod +x "${HOME}/.local/share/kettle/install.sh"

echo ""
echo "kettle ${VERSION} installed."
echo "Search 'kettle' in your app launcher (GNOME Activities / KDE Krunner /"
echo "Ubuntu Super-key), or run \`kettle\` from a shell on \$PATH."
echo ""
echo "Uninstall later via:"
echo "  ~/.local/share/kettle/install.sh --uninstall"
