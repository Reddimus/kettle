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
# - Authenticates the release before extracting it: fetches the same
#   Ed25519-signed `kettle-update-manifest.json` that kettle-update's
#   self-updater trusts (a signing key held only by the release pipeline,
#   independent of whatever serves the tarball) and checks the tarball's
#   SHA-256 against the signed entry for this asset. Falls back to a
#   same-origin `.sha256` sidecar — which only catches transport
#   corruption, not a compromised release channel — when `openssl` can't
#   do Ed25519 verification or the release predates manifest publishing.
# - All work happens in a temp directory that's removed on exit (via
#   `trap`) regardless of success/failure.
# - To uninstall later: run `<prefix>/share/kettle/install.sh --uninstall`
#   (the script writes a prefix-local helper so the uninstall path doesn't
#   depend on the original temp dir).

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

# --- Trust root for the signed release manifest ---------------------
# Every release publishes an Ed25519-signed `kettle-update-manifest.json`
# alongside the tarballs — the exact scheme `kettle-update` (crates/
# kettle-update) already uses to authenticate self-updates from a trust
# root that is independent of the download channel: the signing key lives
# only in the release pipeline's secrets, never on whatever serves the
# tarball. Verifying that signature here (see the "Cryptographic
# verification" section below) closes the gap a same-origin `.sha256`
# sidecar can't: an attacker able to substitute the tarball (a compromised
# CI/release step, a compromised CDN edge, or a MITM scoped to release-
# asset delivery) can regenerate a matching sidecar for their own payload,
# but can't forge a signature without the release key.
#
# This is the DER SubjectPublicKeyInfo (RFC 8410) encoding of the same 32
# raw bytes as `UPDATE_PUBLIC_KEY` in crates/kettle-update/src/lib.rs —
# fingerprint (SHA-256 of the raw 32 bytes, matching the comment there):
# e8e73619a959b34c24fa255714719a61c9cee810340bf041497c39475ab2dbb7
# Keep this byte-for-byte in sync with that constant if the key ever
# rotates; a mismatch here just means every install falls back to the
# weaker sidecar check below, not a build failure, so drift is easy to
# miss — double-check after any key rotation.
MANIFEST_PUBLIC_KEY_PEM='-----BEGIN PUBLIC KEY-----
MCowBQYDK2VwAyEApCcwEc0sux/uhXTzuO9E/RDsNZD/+QcIih2agK9LQQs=
-----END PUBLIC KEY-----'
# Domain-separation prefix (with its trailing NUL) that `kettle-update`
# signs ahead of the manifest bytes — must match `SIGNING_CONTEXT` in
# crates/kettle-update/src/lib.rs byte-for-byte.
MANIFEST_SIGNING_CONTEXT="kettle-update-manifest-v1"

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

# Pick the artifact for this CPU. x86_64 and aarch64 (ARM64:
# Raspberry Pi 4/5, ARM servers/VPS, ARM laptops on Linux) both ship a
# prebuilt tarball; anything else builds from source.
case "$(uname -m)" in
  x86_64 | amd64) ASSET="kettle-linux-x86_64.tar.gz" ;;
  aarch64 | arm64) ASSET="kettle-linux-aarch64.tar.gz" ;;
  *)
    # Name the supported arches and give 32-bit users a
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

# Detect the SHA-256 verifier UP FRONT, not after the
# download. On a minimal container image (e.g. `docker run -it ubuntu`)
# `sha256sum` lives in `coreutils` which may be missing; previously
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

# --- Cryptographic verification (signed release manifest) ----------
# Try the strong path first: fetch the Ed25519-signed
# `kettle-update-manifest.json` published next to the tarball and check
# its signature against the trust root declared above. This is an
# independent-of-the-download-channel guarantee — see the block comment
# on MANIFEST_PUBLIC_KEY_PEM — unlike the same-origin `.sha256` sidecar
# checked in the fallback below. `MANIFEST_VERIFIED` gates that fallback.
MANIFEST_URL="${URL%/*}/kettle-update-manifest.json"
MANIFEST_SIG_URL="${MANIFEST_URL}.sig"
MANIFEST_FILE="${TMP}/kettle-update-manifest.json"
MANIFEST_SIG_FILE="${TMP}/kettle-update-manifest.json.sig"
MANIFEST_VERIFIED=0

# The signed manifest first shipped in v2.35.0 (scripts/make-update-manifest.py).
# Every release from there on publishes `kettle-update-manifest.json[.sig]`, so
# for those a *capable* openssl that still can't fetch-and-verify the manifest
# means it is being suppressed or tampered with — fail closed rather than
# silently downgrading to the same-origin `.sha256` sidecar, which anyone who
# can swap the tarball can forge. Only genuinely older releases (no manifest was
# ever published) or a system whose openssl lacks Ed25519 entirely may take the
# weaker fallback, and neither of those is a condition an on-path attacker can
# induce. `sort -V` does the version compare (POSIX-adjacent, present on the
# GNU/BSD/BusyBox coreutils this Linux/macOS installer targets).
MANIFEST_MIN_VERSION="v2.35.0"
if [ "$VERSION" = "$MANIFEST_MIN_VERSION" ] || [ "$(printf '%s\n%s\n' "$MANIFEST_MIN_VERSION" "$VERSION" | sort -V 2>/dev/null | head -n1)" = "$MANIFEST_MIN_VERSION" ]; then
  MANIFEST_REQUIRED=1
else
  MANIFEST_REQUIRED=0
fi

# Feature-probe `openssl` up front, and keep it SEPARATE from the manifest
# download: `openssl` older than 1.1.1 has no Ed25519 support, and
# `pkeyutl -verify` won't accept `-rawin` — which Ed25519 needs, since it
# hashes the message itself instead of taking a pre-hashed digest.
# `-help` always exits 0 and lists supported flags, so grepping it is a
# cheap, reliable capability check that doesn't risk mistaking "feature
# unsupported" for "signature invalid". Separating capability from the fetch
# is what lets a suppressed-manifest attack (404/reset the manifest requests)
# be told apart from a genuinely too-old openssl and fail closed below.
OPENSSL_ED25519=0
if command -v openssl >/dev/null 2>&1 \
  && openssl pkeyutl -verify -help 2>&1 | grep -q -- '-rawin'; then
  OPENSSL_ED25519=1
fi

if [ "$OPENSSL_ED25519" -eq 1 ] \
  && curl -fsSL -o "$MANIFEST_FILE" "$MANIFEST_URL" 2>/dev/null \
  && curl -fsSL -o "$MANIFEST_SIG_FILE" "$MANIFEST_SIG_URL" 2>/dev/null; then
  PUBKEY_FILE="${TMP}/kettle-update-manifest.pub.pem"
  SIG_RAW_FILE="${TMP}/kettle-update-manifest.sig.bin"
  SIGNED_FILE="${TMP}/kettle-update-manifest.signed.bin"
  printf '%s\n' "$MANIFEST_PUBLIC_KEY_PEM" > "$PUBKEY_FILE"
  if ! openssl base64 -d -A -in "$MANIFEST_SIG_FILE" -out "$SIG_RAW_FILE" 2>/dev/null; then
    echo "kettle install-online.sh: signed manifest's .sig is not valid base64 for ${VERSION} — aborting." >&2
    echo "Refusing to trust an unauthenticated manifest; ${MANIFEST_SIG_URL} looks corrupt or tampered." >&2
    exit 1
  fi
  # The signed payload is the domain-separation prefix (with its
  # trailing NUL) followed by the exact manifest bytes, byte-for-byte —
  # must match what the release pipeline signs and `kettle-update`
  # verifies, or a genuine signature will correctly fail to verify below.
  printf '%s\000' "$MANIFEST_SIGNING_CONTEXT" > "$SIGNED_FILE"
  cat "$MANIFEST_FILE" >> "$SIGNED_FILE"
  if ! openssl pkeyutl -verify -rawin -pubin -inkey "$PUBKEY_FILE" \
      -sigfile "$SIG_RAW_FILE" -in "$SIGNED_FILE" >/dev/null 2>&1; then
    echo "kettle install-online.sh: signed release manifest FAILED Ed25519 verification for ${VERSION}." >&2
    echo "${MANIFEST_URL} does not carry a valid signature from kettle's release key." >&2
    echo "Refusing to trust a hash from an unauthenticated manifest. Aborting." >&2
    exit 1
  fi
  # A valid signature only proves kettle's release key signed *some*
  # manifest — cheaply confirm it is the one for this exact release
  # before trusting any hash out of it. Replaying an old (still validly
  # signed) manifest for a different tag can't forge a hash match for
  # different content (that would need a SHA-256 preimage), but failing
  # closed on a mismatch costs nothing and also catches release-pipeline
  # bugs early.
  if ! grep -q "\"tag\":\"${VERSION}\"" "$MANIFEST_FILE" \
    || ! grep -q '"product":"kettle"' "$MANIFEST_FILE" \
    || ! grep -q '"channel":"stable"' "$MANIFEST_FILE"; then
    echo "kettle install-online.sh: signed manifest does not describe ${VERSION} (kettle/stable) — aborting." >&2
    exit 1
  fi
  # The manifest is compact, sorted-key JSON generated by
  # scripts/make-update-manifest.py (one flat object per asset, no
  # nested braces), so bounding each asset entry at its own `}` and
  # pulling the sha256 out of that slice is exact, not a best-effort
  # scrape of arbitrary JSON. Done in `awk` with plain `index()`/`substr()`
  # (no `grep -o` or `sed -E`, both GNU/BSD extensions absent from some
  # minimal/BusyBox `grep`/`sed` builds) so this stays as portable as the
  # rest of the script; `-v` assignment and interval-free ERE matching are
  # both POSIX-mandated `awk` behavior.
  NAME_FIELD="\"name\":\"${ASSET}\""
  MANIFEST_SHA=$(awk -v want="$NAME_FIELD" '
    {
      start = index($0, want)
      if (start == 0) { next }
      obj = substr($0, start)
      close_brace = index(obj, "}")
      if (close_brace > 0) { obj = substr(obj, 1, close_brace) }
      key = "\"sha256\":\""
      key_at = index(obj, key)
      if (key_at == 0) { next }
      hash = substr(obj, key_at + length(key), 64)
      if (length(hash) == 64 && hash ~ /^[0-9a-f]+$/) { print hash }
    }
  ' "$MANIFEST_FILE") || true
  if [ -z "$MANIFEST_SHA" ]; then
    echo "kettle install-online.sh: signed manifest has no SHA-256 entry for ${ASSET} — aborting." >&2
    exit 1
  fi
  if command -v sha256sum >/dev/null 2>&1; then
    ACTUAL_SHA=$(sha256sum "$TAR" | awk '{print $1}')
  else
    ACTUAL_SHA=$(shasum -a 256 "$TAR" | awk '{print $1}')
  fi
  if [ "$ACTUAL_SHA" != "$MANIFEST_SHA" ]; then
    echo "kettle install-online.sh: SHA-256 mismatch against the SIGNED manifest for ${ASSET}." >&2
    echo "Expected ${MANIFEST_SHA}, got ${ACTUAL_SHA}." >&2
    echo "Refusing to extract a tampered archive. Aborting." >&2
    exit 1
  fi
  echo "kettle: SHA-256 verified. (Ed25519-signed manifest — independent trust root.)"
  MANIFEST_VERIFIED=1
elif [ "$OPENSSL_ED25519" -eq 1 ] && [ "$MANIFEST_REQUIRED" -eq 1 ]; then
  # openssl CAN verify Ed25519 and this release (>= v2.35.0) must ship a signed
  # manifest, yet the manifest or its signature could not be fetched. On a
  # reachable network that is suppression/tampering, not a legitimate absence —
  # do not fall through to the forgeable same-origin sidecar.
  echo "kettle install-online.sh: ${VERSION} must ship an Ed25519-signed manifest, but ${MANIFEST_URL}[.sig] could not be fetched." >&2
  echo "If you are online, a >= ${MANIFEST_MIN_VERSION} release that serves no signed manifest indicates suppression or tampering of the release channel." >&2
  echo "Refusing to downgrade to the weaker same-origin checksum. Aborting." >&2
  exit 1
elif [ "$OPENSSL_ED25519" -ne 1 ] && [ "$MANIFEST_REQUIRED" -eq 1 ]; then
  # A genuine capability gap (not attacker-inducible): openssl is missing or
  # predates Ed25519. Warn loudly and let the sidecar fallback run — the user
  # can install a modern openssl for an independent trust root.
  echo "kettle install-online.sh: cannot verify ${VERSION}'s Ed25519-signed manifest — openssl is missing or too old for Ed25519 (needs >= 1.1.1)." >&2
  echo "Proceeding with the weaker same-origin SHA-256 sidecar; install a modern openssl for an independent trust root." >&2
fi

# --- SHA-256 verification (fallback) --------------------------------
# Only runs when the signed-manifest check above couldn't complete:
# `openssl` lacks Ed25519 support, the manifest/signature didn't fetch
# (network hiccup, or this release predates manifest publishing), or
# there was no earlier hard failure to abort on. Releases since v1.3.4
# ship a `<artifact>.sha256` sidecar generated on the same CI runner as
# the artifact and served from the very same release-asset channel as
# the tarball. That still catches what it can: transport corruption, a
# truncated download, or a wrong/partial file landing at the expected
# URL. It is NOT an independent trust root the way the signed manifest
# above is — anyone able to substitute the tarball itself (a compromised
# CI/release step, a compromised CDN edge, or a MITM scoped to release-
# asset delivery) can regenerate a matching `.sha256` for their own
# payload just as easily as the real CI runner did. Treat a pass here as
# "not obviously corrupted or truncated", not as "verified authentic".
# Older releases (≤ v1.3.3) didn't publish a sidecar either, so a missing
# .sha256 is a soft failure (warn + continue) rather than a hard error.
if [ "$MANIFEST_VERIFIED" -ne 1 ]; then
  echo "kettle install-online.sh: signed-manifest verification unavailable for ${VERSION} — falling back to the weaker same-origin SHA-256 sidecar." >&2
  SHA_URL="${URL}.sha256"
  SHA_FILE="${TMP}/${ASSET}.sha256"
  if curl -fL -o "$SHA_FILE" "$SHA_URL" 2>/dev/null; then
    # sha256sum reads `<hex>  <filename>` and looks for the file relative
    # to the cwd. Run it in $TMP so the bare filename matches.
    # Split tool-availability and verification-result so the
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
      echo "kettle: SHA-256 verified. (same-origin checksum only — see install-online.sh for caveats.)"
    else
      echo "kettle install-online.sh: SHA-256 verification FAILED for ${ASSET}." >&2
      echo "The downloaded tarball does not match the hash published on the release." >&2
      echo "Refusing to extract a potentially-tampered archive. Aborting." >&2
      exit 1
    fi
  else
    # No sidecar — older release predating the v1.3.4 sha256 sidecar publish.
    # Warn but continue so the one-liner still installs v1.3.0..v1.3.3.
    echo "kettle install-online.sh: no .sha256 sidecar found for ${VERSION} — skipping verification." >&2
    echo "Releases from v1.3.4 onward publish checksums; pin to a newer version with KETTLE_VERSION." >&2
  fi
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
  rm -f "${SCRIPT_DIR}/install-real.sh" "${SCRIPT_DIR}/install.sh" "${SCRIPT_DIR}/install.json"
  rm -rf "${SCRIPT_DIR}/shell-integration"
  rmdir "${SCRIPT_DIR}" 2>/dev/null || true
else
  exec "${SCRIPT_DIR}/install-real.sh" "--prefix=${PREFIX}" "$@"
fi
EOF
chmod +x "$INSTALL_HELPER"
chmod +x "$INSTALL_REAL"
if [ -d "$TMP/kettle/shell-integration" ]; then
  mkdir -p "${INSTALL_PREFIX}/share/kettle/shell-integration"
  cp "$TMP/kettle/shell-integration/"* "${INSTALL_PREFIX}/share/kettle/shell-integration/"
  chmod 644 "${INSTALL_PREFIX}/share/kettle/shell-integration/"*
fi

# Current online installs become explicit self-update-owned layouts even when
# the selected release predates the marker-aware bundled install.sh.
case "$ASSET" in
  kettle-linux-x86_64.tar.gz) UPDATE_TARGET="x86_64-unknown-linux-gnu" ;;
  kettle-linux-aarch64.tar.gz) UPDATE_TARGET="aarch64-unknown-linux-gnu" ;;
esac
cat > "${INSTALL_PREFIX}/share/kettle/install.json" <<EOF
{
  "schema": 1,
  "product": "kettle",
  "managed_by": "kettle-installer",
  "channel": "stable",
  "target": "${UPDATE_TARGET}",
  "version": "${VERSION#v}"
}
EOF
chmod 644 "${INSTALL_PREFIX}/share/kettle/install.json"

echo ""
echo "kettle ${VERSION} installed."
echo "Search 'kettle' in your app launcher (GNOME Activities / KDE Krunner /"
echo "Ubuntu Super-key), or run \`kettle\` from a shell on \$PATH."
echo ""
echo "Uninstall later via:"
echo "  ${INSTALL_HELPER} --uninstall"
