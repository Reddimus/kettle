#!/usr/bin/env sh
# kettle — one-line online installer for Linux
#
# Downloads the latest GitHub release tarball, extracts the prebuilt
# binary + XDG launcher + icons to a temp directory, and runs the
# bundled `install.sh --skip-build` to drop everything into the
# standard XDG user paths under `~/.local/` by default. No `sudo`
# required for the default path, no Rust toolchain required.
#
#   curl -fsSL https://raw.githubusercontent.com/Reddimus/kettle/main/scripts/install-online.sh | sh
#
# Or with a pinned version (recommended for reproducible installs):
#
#   curl -fsSL https://raw.githubusercontent.com/Reddimus/kettle/main/scripts/install-online.sh | KETTLE_VERSION=v1.44.0 sh
#
# Or with a custom install prefix (e.g. system-wide):
#
#   curl -fsSL https://raw.githubusercontent.com/Reddimus/kettle/main/scripts/install-online.sh | KETTLE_PREFIX=/usr/local sh
#
# Notes
# - Linux x86_64 + aarch64. macOS users: grab the `.app` bundle from
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

# NOTE: this script is `#!/usr/bin/env sh` and is run via `curl … | sh`,
# so it must stay POSIX. `set -o pipefail` is a bashism — under dash
# (Ubuntu's /bin/sh) it errors and, with `set -e`, would abort the whole
# installer. Pipeline robustness is handled explicitly below instead
# (the download is staged to a temp file and its gzip magic bytes are
# validated before extraction, so a truncated `curl` can't masquerade as
# a clean extract).
set -eu

REPO="Reddimus/kettle"
VERSION="${KETTLE_VERSION:-latest}"
# ASSET is selected from `uname -m` in the arch check below (x86_64 / aarch64).
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

# Cycle 767: pick the artifact for this CPU. x86_64 and aarch64 (ARM64:
# Raspberry Pi 4/5, ARM servers/VPS, ARM laptops on Linux) both ship a
# prebuilt tarball; anything else builds from source.
case "$(uname -m)" in
  x86_64 | amd64) ASSET="kettle-linux-x86_64.tar.gz" ;;
  aarch64 | arm64) ASSET="kettle-linux-aarch64.tar.gz" ;;
  *)
    # Cycle 793 (audit F1): name the supported arches and give 32-bit users a
    # real path instead of a dead end. wgpu/glyphon have no tier-1 support on
    # armv7l/i686, so a source build there is experimental — say so, and point
    # at the support-tier matrix + a zero-build Nix sandbox to try first.
    echo "kettle install-online.sh: no prebuilt binary for arch '$(uname -m)'." >&2
    echo "Prebuilt Linux binaries are x86_64 (amd64) and aarch64 (arm64) only." >&2
    echo "32-bit targets (armv7l / i686) are source-only and EXPERIMENTAL —" >&2
    echo "wgpu/glyphon have no tier-1 support there; see the support-tier matrix" >&2
    echo "in docs/INSTALL.md (section 'Supported platforms')." >&2
    echo "Build from source:" >&2
    echo "  git clone https://github.com/${REPO} && cd kettle && ./scripts/install.sh" >&2
    echo "Or try it sandboxed without building (needs Nix):" >&2
    echo "  nix run github:${REPO}" >&2
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

# Cycle 725: detect the SHA-256 verifier UP FRONT, not after the
# download. On a minimal container image (e.g. `docker run -it ubuntu`)
# `sha256sum` lives in `coreutils` which may be missing; pre-cycle-725
# the script would download the ~5 MB tarball, hit the verify step,
# print "SHA-256 verification FAILED" and exit — making it look like
# a corrupted download. Bail BEFORE download so the user fixes the
# right problem (`apt-get install coreutils` etc).
if ! command -v sha256sum >/dev/null 2>&1 && ! command -v shasum >/dev/null 2>&1; then
  echo "kettle install-online.sh: missing 'sha256sum' (Linux) or 'shasum -a 256' (macOS)." >&2
  echo "Install it via your distro's package manager and re-run:" >&2
  echo "  Debian/Ubuntu: sudo apt-get install -y coreutils" >&2
  echo "  Fedora/RHEL:   sudo dnf install -y coreutils" >&2
  echo "  Alpine:        sudo apk add coreutils" >&2
  echo "  macOS:         already included with the OS (no install needed)" >&2
  exit 1
fi

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

# --- SHA-256 verification ------------------------------------------
# Releases since v1.3.4 ship a `<artifact>.sha256` sidecar generated
# on the same CI runner as the artifact. Download it and check —
# guards against a tampered tarball from a compromised mirror or a
# MITM. Older releases (≤ v1.3.3) didn't publish a sidecar, so a
# missing .sha256 is a soft failure (warn + continue) rather than a
# hard error. Once the older releases age out of the supported set,
# tighten this to a hard requirement.
SHA_URL="${URL}.sha256"
SHA_FILE="${TMP}/${ASSET}.sha256"
if curl -fL -o "$SHA_FILE" "$SHA_URL" 2>/dev/null; then
  # sha256sum reads `<hex>  <filename>` and looks for the file relative
  # to the cwd. Run it in $TMP so the bare filename matches.
  # Cycle 590: split tool-availability and verification-result so the
  # error diagnostic is accurate. Pre-fix, a system without sha256sum
  # AND without shasum would print "SHA-256 verification FAILED"
  # implying tampering, when actually the verification couldn't run.
  # Now: detect "no hashing tool" explicitly and emit the correct
  # diagnostic.
  if command -v sha256sum >/dev/null 2>&1; then
    HASH_CMD="sha256sum -c"
  elif command -v shasum >/dev/null 2>&1; then
    # Some BusyBox / Alpine environments ship shasum but not sha256sum.
    HASH_CMD="shasum -a 256 -c"
  else
    echo "kettle install-online.sh: neither sha256sum nor shasum is installed." >&2
    echo "Install one of them (coreutils / perl-base / busybox-utils) to verify" >&2
    echo "the release SHA-256, then re-run. Refusing to extract an unverified" >&2
    echo "archive." >&2
    exit 1
  fi
  if (cd "$TMP" && $HASH_CMD "$(basename "$SHA_FILE")" >/dev/null 2>&1); then
    echo "kettle: SHA-256 verified."
  else
    echo "kettle install-online.sh: SHA-256 verification FAILED for ${ASSET}." >&2
    echo "The downloaded tarball does not match the hash published on the release." >&2
    echo "Refusing to extract a potentially-tampered archive. Aborting." >&2
    exit 1
  fi
else
  # No sidecar — older release predating the cycle-254 sha256 publish.
  # Warn but continue so the one-liner still installs v1.3.0..v1.3.3.
  echo "kettle install-online.sh: no .sha256 sidecar found for ${VERSION} — skipping verification." >&2
  echo "Releases from v1.3.4 onward publish checksums; pin to a newer version with KETTLE_VERSION." >&2
fi

# --- Extract + run the bundled install.sh --------------------------
tar -C "$TMP" -xzf "$TAR"
if [ ! -x "$TMP/kettle/install.sh" ]; then
  echo "kettle install-online.sh: extracted tarball doesn't contain install.sh." >&2
  echo "This is likely a bug in the upstream release pipeline; please report at" >&2
  echo "  https://github.com/${REPO}/issues" >&2
  exit 1
fi

# `--skip-build` because the tarball already ships a release binary. Invoke via
# the script's own shebang (`#!/usr/bin/env bash`) — the bundled install.sh uses
# `set -euo pipefail`, a Bash-ism that fails under dash (Debian/Ubuntu `sh` is
# dash). The release.yml ships install.sh with mode 755 so the bare exec path
# works.
#
# `KETTLE_PREFIX` env var (optional) plumbs through to install.sh's
# `--prefix=<DIR>` so a power user can do, e.g.,
#   KETTLE_PREFIX=/usr/local sh install-online.sh
# for a system-wide install (with appropriate write perms). Default is
# `~/.local/` — matches the standalone `install.sh` default.
#
# Capture stdout so we can suppress old tarball helpers' repo-oriented uninstall
# hint (`./scripts/install.sh --uninstall`) and print the prefix-aware online
# uninstall helper below instead. Keep stderr live so real failures are visible.
INSTALL_LOG="${TMP}/install.log"
if [ -n "${KETTLE_PREFIX:-}" ]; then
  if ! "$TMP/kettle/install.sh" --skip-build "--prefix=$KETTLE_PREFIX" > "$INSTALL_LOG"; then
    cat "$INSTALL_LOG"
    exit 1
  fi
else
  if ! "$TMP/kettle/install.sh" --skip-build > "$INSTALL_LOG"; then
    cat "$INSTALL_LOG"
    exit 1
  fi
fi
sed '/^To uninstall: \.\/scripts\/install\.sh --uninstall$/d' "$INSTALL_LOG"

# Stash an uninstall helper under the same prefix so the user can later
# uninstall without re-downloading. Keep this in lockstep with KETTLE_PREFIX:
# pre-fix, a custom-prefix install still wrote ~/.local/share/kettle/install.sh,
# leaving the uninstall helper in the wrong tree and mutating the user's default
# install area during an isolated/prefix install.
#
# Use a wrapper instead of copying install.sh directly. The tarball might come
# from an older release whose install.sh defaults to ~/.local even when it is
# saved under <prefix>/share/kettle; the wrapper infers <prefix> from its own
# location and passes it explicitly. Newer install.sh versions infer this too,
# but the wrapper keeps old release tarballs uninstallable from custom prefixes.
INSTALL_PREFIX="${KETTLE_PREFIX:-${HOME}/.local}"
INSTALL_HELPER="${INSTALL_PREFIX}/share/kettle/install.sh"
INSTALL_REAL="${INSTALL_PREFIX}/share/kettle/install-real.sh"
mkdir -p "$(dirname "$INSTALL_HELPER")"
cp "$TMP/kettle/install.sh" "$INSTALL_REAL"
cat > "$INSTALL_HELPER" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
PREFIX=$(cd -- "${SCRIPT_DIR}/../.." && pwd)
UNINSTALL=0
for arg in "$@"; do
  if [[ "${arg}" == "--uninstall" ]]; then
    UNINSTALL=1
  fi
done
if [[ "${UNINSTALL}" -eq 1 ]]; then
  "${SCRIPT_DIR}/install-real.sh" "--prefix=${PREFIX}" "$@"
  rm -f "${SCRIPT_DIR}/install-real.sh" "${SCRIPT_DIR}/install.sh"
  rmdir "${SCRIPT_DIR}" 2>/dev/null || true
else
  exec "${SCRIPT_DIR}/install-real.sh" "--prefix=${PREFIX}" "$@"
fi
EOF
chmod +x "$INSTALL_HELPER"
chmod +x "$INSTALL_REAL"

echo ""
echo "kettle ${VERSION} installed."
echo "Search 'kettle' in your app launcher (GNOME Activities / KDE Krunner /"
echo "Ubuntu Super-key), or run \`kettle\` from a shell on \$PATH."
echo ""
echo "Uninstall later via:"
echo "  ${INSTALL_HELPER} --uninstall"
