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
# - The script uses `curl`, GNU `tar`, `gzip`, OpenSSL 3.0+, and standard
#   POSIX text tools. `gh` (GitHub CLI) is NOT required.
# - Verifies the downloaded tarball is non-empty and has a recognizable
#   gzip header before extracting — guards against partial / hijacked
#   downloads.
# - Authenticates the release before extracting it: fetches the same
#   Ed25519-signed `kettle-update-manifest.json` that kettle-update's
#   self-updater trusts (a signing key held only by the release pipeline,
#   independent of whatever serves the tarball) and checks the tarball's
#   SHA-256 and byte count against the signed entry for this asset. Modern
#   releases fail closed when Ed25519 verification is unavailable. Releases
#   predating signed manifests require their same-origin `.sha256` sidecar;
#   an archive is never extracted without at least that integrity check.
# - Bounds every download and preflights the authenticated tar stream before
#   extraction: at most 256 MiB compressed, 128 entries, and 512 MiB unpacked,
#   with only safe regular files/directories under one `kettle/` root.
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
LC_ALL=C
LANG=C
CDPATH=
export LC_ALL LANG CDPATH

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
# This is the canonical `packaging/update-public.pem` trust root, whose DER
# SubjectPublicKeyInfo (RFC 8410) contains the same 32 raw bytes as
# `UPDATE_PUBLIC_KEY` in crates/kettle-update/src/lib.rs —
# fingerprint (SHA-256 of the raw 32 bytes, matching the comment there):
# e8e73619a959b34c24fa255714719a61c9cee810340bf041497c39475ab2dbb7
# `scripts/test-update-manifest.py` enforces byte-for-byte lockstep across all
# three forms and the release workflow.
MANIFEST_PUBLIC_KEY_PEM='-----BEGIN PUBLIC KEY-----
MCowBQYDK2VwAyEApCcwEc0sux/uhXTzuO9E/RDsNZD/+QcIih2agK9LQQs=
-----END PUBLIC KEY-----'
# Domain-separation prefix (with its trailing NUL) that `kettle-update`
# signs ahead of the manifest bytes — must match `SIGNING_CONTEXT` in
# crates/kettle-update/src/lib.rs byte-for-byte.
MANIFEST_SIGNING_CONTEXT="kettle-update-manifest-v1"
MAX_ARCHIVE_BYTES=268435456
MAX_MANIFEST_BYTES=131072
MAX_SIGNATURE_BYTES=1024
MAX_SIDECAR_BYTES=1024
MAX_ARCHIVE_ENTRIES=128
MAX_UNPACKED_BYTES=536870912
MAX_LATEST_HEADERS_BYTES=131072
CURL_CONNECT_TIMEOUT_SECONDS=15
CURL_TOTAL_TIMEOUT_SECONDS=600
CURL_LOW_SPEED_SECONDS=30
CURL_LOW_SPEED_BYTES=1024
CURL_MAX_REDIRECTS=5
POSIX_FILE_LIMIT_BLOCK_BYTES=512

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
  x86_64 | amd64)
    ASSET="kettle-linux-x86_64.tar.gz"
    EXPECTED_TARGET="x86_64-unknown-linux-gnu"
    ;;
  aarch64 | arm64)
    ASSET="kettle-linux-aarch64.tar.gz"
    EXPECTED_TARGET="aarch64-unknown-linux-gnu"
    ;;
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
for cmd in awk cat chmod cp curl dirname find grep mkdir mktemp od rm sed tail tar tr uname wc; do
  if ! command -v "$cmd" >/dev/null 2>&1; then
    echo "kettle install-online.sh: missing required tool '$cmd'." >&2
    echo "Install it via your distro's package manager and re-run." >&2
    exit 1
  fi
done

if ! curl --help all 2>/dev/null | grep -q -- '--max-filesize'; then
  echo "kettle install-online.sh: curl lacks --max-filesize support." >&2
  echo "Install a current curl so downloads can be bounded before re-running." >&2
  exit 1
fi
if ! tar --version 2>/dev/null | grep -q 'GNU tar'; then
  echo "kettle install-online.sh: hardened extraction requires GNU tar." >&2
  echo "Install GNU tar with your distro package manager and re-run." >&2
  exit 1
fi

download_limited() {
  download_url=$1
  download_path=$2
  download_limit=$3
  download_label=$4
  download_progress=${5:-0}
  download_blocks=$((
    (download_limit + POSIX_FILE_LIMIT_BLOCK_BYTES - 1) /
      POSIX_FILE_LIMIT_BLOCK_BYTES
  ))

  rm -f "$download_path"
  if [ "$download_progress" -eq 1 ]; then
    curl_flags="-fL --proto =https --proto-redir =https --tlsv1.2 --max-redirs ${CURL_MAX_REDIRECTS} --connect-timeout ${CURL_CONNECT_TIMEOUT_SECONDS} --max-time ${CURL_TOTAL_TIMEOUT_SECONDS} --speed-limit ${CURL_LOW_SPEED_BYTES} --speed-time ${CURL_LOW_SPEED_SECONDS} --max-filesize ${download_limit} --progress-bar"
  else
    curl_flags="-fsSL --proto =https --proto-redir =https --tlsv1.2 --max-redirs ${CURL_MAX_REDIRECTS} --connect-timeout ${CURL_CONNECT_TIMEOUT_SECONDS} --max-time ${CURL_TOTAL_TIMEOUT_SECONDS} --speed-limit ${CURL_LOW_SPEED_BYTES} --speed-time ${CURL_LOW_SPEED_SECONDS} --max-filesize ${download_limit}"
  fi
  # Word splitting here is intentional: every flag is a fixed token assembled
  # above; URLs and paths remain separately quoted arguments.
  # shellcheck disable=SC2086
  if ! (
    # POSIX sh defines `ulimit -f` in 512-byte blocks. This kernel-enforced
    # ceiling covers chunked/unknown-length responses on curls older than 8.4,
    # where --max-filesize checked only a declared Content-Length.
    ulimit -f "$download_blocks"
    curl $curl_flags -o "$download_path" "$download_url"
  ); then
    rm -f "$download_path"
    return 1
  fi
  download_size=$(wc -c < "$download_path" | tr -d '[:space:]')
  case "$download_size" in
    '' | *[!0-9]*)
      rm -f "$download_path"
      return 1
      ;;
  esac
  if [ "$download_size" -le 0 ] || [ "$download_size" -gt "$download_limit" ]; then
    echo "kettle install-online.sh: ${download_label} is ${download_size} bytes; limit is ${download_limit}." >&2
    rm -f "$download_path"
    return 1
  fi
  return 0
}

download_headers_limited() {
  download_url=$1
  download_path=$2
  download_limit=$3
  download_blocks=$((
    (download_limit + POSIX_FILE_LIMIT_BLOCK_BYTES - 1) /
      POSIX_FILE_LIMIT_BLOCK_BYTES
  ))

  rm -f "$download_path"
  if ! (
    ulimit -f "$download_blocks"
    curl -fsSLI --proto '=https' --proto-redir '=https' --tlsv1.2 \
      --max-redirs "$CURL_MAX_REDIRECTS" \
      --connect-timeout "$CURL_CONNECT_TIMEOUT_SECONDS" \
      --max-time "$CURL_TOTAL_TIMEOUT_SECONDS" \
      --speed-limit "$CURL_LOW_SPEED_BYTES" \
      --speed-time "$CURL_LOW_SPEED_SECONDS" \
      -o "$download_path" "$download_url"
  ); then
    rm -f "$download_path"
    return 1
  fi
  download_size=$(wc -c < "$download_path" | tr -d '[:space:]')
  case "$download_size" in
    '' | *[!0-9]*)
      rm -f "$download_path"
      return 1
      ;;
  esac
  if [ "$download_size" -le 0 ] || [ "$download_size" -gt "$download_limit" ]; then
    rm -f "$download_path"
    return 1
  fi
  return 0
}

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

# Allocate the private workspace before resolving `latest` so redirect headers
# are also written under a kernel-enforced file-size limit.
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT INT TERM

# --- Resolve target version + URL ----------------------------------
if [ "$VERSION" = "latest" ]; then
  # The /releases/latest endpoint redirects to /releases/tag/<tag>.
  # `curl -sLI` follows redirects and dumps headers; grep the final
  # `location:` line for the tag. Bare-bones (no jq) so the script
  # has zero non-coreutils deps.
  LATEST_HEADERS="${TMP}/latest.headers"
  if ! download_headers_limited \
      "https://github.com/${REPO}/releases/latest" \
      "$LATEST_HEADERS" "$MAX_LATEST_HEADERS_BYTES"; then
    echo "kettle install-online.sh: could not fetch bounded latest-release headers." >&2
    exit 1
  fi
  RESOLVED=$(awk 'tolower($1) == "location:" { print $2 }' "$LATEST_HEADERS" \
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

if ! VERSION_NUMBER=$(awk -v value="$VERSION" '
  BEGIN {
    if (value !~ /^v[0-9]+\.[0-9]+\.[0-9]+$/) {
      exit 1
    }
    count = split(substr(value, 2), component, ".")
    if (count != 3) {
      exit 1
    }
    for (i = 1; i <= 3; i++) {
      if (length(component[i]) > 9 ||
          (length(component[i]) > 1 && substr(component[i], 1, 1) == "0")) {
        exit 1
      }
    }
    print substr(value, 2)
  }
'); then
  echo "kettle install-online.sh: invalid version '$VERSION'; expected exact vMAJOR.MINOR.PATCH." >&2
  exit 1
fi

URL="https://github.com/${REPO}/releases/download/${VERSION}/${ASSET}"
echo "kettle: installing ${VERSION} from ${URL}"

TAR="${TMP}/${ASSET}"
if ! download_limited "$URL" "$TAR" "$MAX_ARCHIVE_BYTES" "release archive" 1; then
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
# can swap the tarball can forge. Only genuinely older releases (no manifest
# was ever published) may take the weaker fallback. A modern release never
# downgrades merely because its verifier is missing: that would let a
# compromised release channel choose the weaker trust policy.
MANIFEST_MIN_VERSION="v2.35.0"
MANIFEST_REQUIRED=$(awk -v value="$VERSION_NUMBER" '
  BEGIN {
    split(value, component, ".")
    required = component[1] > 2 ||
      (component[1] == 2 && component[2] > 35) ||
      (component[1] == 2 && component[2] == 35 && component[3] >= 0)
    print required ? 1 : 0
  }
')
PACKAGE_MANIFEST_REQUIRED=$(awk -v value="$VERSION_NUMBER" '
  BEGIN {
    split(value, component, ".")
    required = component[1] > 2 ||
      (component[1] == 2 && component[2] > 36) ||
      (component[1] == 2 && component[2] == 36 && component[3] >= 0)
    print required ? 1 : 0
  }
')

# Feature-probe `openssl` up front, and keep it SEPARATE from the manifest
# download: `pkeyutl` before OpenSSL 3.0 cannot verify Ed25519, and
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
  && download_limited "$MANIFEST_URL" "$MANIFEST_FILE" "$MAX_MANIFEST_BYTES" \
    "signed manifest" 0 2>/dev/null \
  && download_limited "$MANIFEST_SIG_URL" "$MANIFEST_SIG_FILE" \
    "$MAX_SIGNATURE_BYTES" "manifest signature" 0 2>/dev/null; then
  PUBKEY_FILE="${TMP}/kettle-update-manifest.pub.pem"
  SIG_RAW_FILE="${TMP}/kettle-update-manifest.sig.bin"
  SIGNED_FILE="${TMP}/kettle-update-manifest.signed.bin"
  printf '%s\n' "$MANIFEST_PUBLIC_KEY_PEM" > "$PUBKEY_FILE"
  if ! openssl base64 -d -A -in "$MANIFEST_SIG_FILE" -out "$SIG_RAW_FILE" 2>/dev/null; then
    echo "kettle install-online.sh: signed manifest's .sig is not valid base64 for ${VERSION} — aborting." >&2
    echo "Refusing to trust an unauthenticated manifest; ${MANIFEST_SIG_URL} looks corrupt or tampered." >&2
    exit 1
  fi
  SIG_RAW_SIZE=$(wc -c < "$SIG_RAW_FILE" | tr -d '[:space:]')
  if [ "$SIG_RAW_SIZE" != 64 ]; then
    echo "kettle install-online.sh: signed manifest has a ${SIG_RAW_SIZE}-byte Ed25519 signature; expected 64." >&2
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
  # Parse the canonical generator format as one exact record, rather than
  # grepping independent fragments that could come from different objects.
  # The signed top-level identity, schema, tag/version, selected target/name,
  # byte count, and hash are bound together before any field is trusted.
  if ! MANIFEST_FIELDS=$(awk \
    -v version="$VERSION" \
    -v version_number="$VERSION_NUMBER" \
    -v wanted_name="$ASSET" \
    -v wanted_target="$EXPECTED_TARGET" '
    function reject() {
      bad = 1
      exit 1
    }
    NR == 1 {
      line = $0
      next
    }
    {
      reject()
    }
    END {
      if (bad || NR != 1) {
        exit 1
      }
      prefix = "{\"assets\":["
      if (substr(line, 1, length(prefix)) != prefix) {
        exit 1
      }
      top = "],\"channel\":\"stable\",\"product\":\"kettle\",\"published_at\":\""
      top_at = index(line, top)
      if (top_at == 0 || index(substr(line, top_at + 1), top) != 0) {
        exit 1
      }
      assets = substr(line, length(prefix) + 1, top_at - length(prefix) - 1)
      remainder = substr(line, top_at + length(top))
      quote_at = index(remainder, "\"")
      if (quote_at <= 1) {
        exit 1
      }
      published_at = substr(remainder, 1, quote_at - 1)
      if (length(published_at) > 64 || published_at !~ /^[0-9T:+.-]+$/) {
        exit 1
      }
      suffix = substr(remainder, quote_at)
      expected_suffix = "\",\"schema\":1,\"tag\":\"" version \
        "\",\"version\":\"" version_number "\"}"
      if (suffix != expected_suffix) {
        exit 1
      }

      needle = "{\"name\":\"" wanted_name "\",\"sha256\":\""
      asset_at = index(assets, needle)
      if (asset_at == 0 ||
          index(substr(assets, asset_at + length(needle)), needle) != 0) {
        exit 1
      }
      object = substr(assets, asset_at)
      close_at = index(object, "}")
      if (close_at == 0) {
        exit 1
      }
      object = substr(object, 1, close_at)
      hash = substr(object, length(needle) + 1, 64)
      if (length(hash) != 64 || hash !~ /^[0-9a-f]+$/) {
        exit 1
      }
      after_hash = substr(object, length(needle) + 65)
      size_prefix = "\",\"size\":"
      if (substr(after_hash, 1, length(size_prefix)) != size_prefix) {
        exit 1
      }
      size_tail = substr(after_hash, length(size_prefix) + 1)
      comma_at = index(size_tail, ",")
      if (comma_at <= 1) {
        exit 1
      }
      size = substr(size_tail, 1, comma_at - 1)
      if (size !~ /^[0-9]+$/ || length(size) > 9) {
        exit 1
      }
      expected_tail = ",\"target\":\"" wanted_target "\"}"
      if (substr(size_tail, comma_at) != expected_tail) {
        exit 1
      }
      print hash " " size
    }
  ' "$MANIFEST_FILE"); then
    echo "kettle install-online.sh: signed manifest is non-canonical or does not bind ${VERSION}/${EXPECTED_TARGET}/${ASSET} exactly." >&2
    exit 1
  fi
  # MANIFEST_FIELDS contains only a validated lowercase hex digest and decimal
  # size, so normal POSIX field splitting is safe and deterministic.
  set -- $MANIFEST_FIELDS
  if [ "$#" -ne 2 ]; then
    echo "kettle install-online.sh: signed manifest field extraction was ambiguous." >&2
    exit 1
  fi
  MANIFEST_SHA=$1
  MANIFEST_SIZE=$2
  if [ "$MANIFEST_SIZE" -le 0 ] || [ "$MANIFEST_SIZE" -gt "$MAX_ARCHIVE_BYTES" ]; then
    echo "kettle install-online.sh: signed archive size ${MANIFEST_SIZE} is outside the accepted range." >&2
    exit 1
  fi
  if [ "$SIZE" -ne "$MANIFEST_SIZE" ]; then
    echo "kettle install-online.sh: archive size mismatch against signed manifest (expected ${MANIFEST_SIZE}, got ${SIZE})." >&2
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
elif [ "$MANIFEST_REQUIRED" -eq 1 ]; then
  if [ "$OPENSSL_ED25519" -eq 1 ]; then
    echo "kettle install-online.sh: ${VERSION} must ship a bounded Ed25519-signed manifest, but ${MANIFEST_URL}[.sig] could not be fetched." >&2
    echo "A missing or oversized manifest on a release from ${MANIFEST_MIN_VERSION} onward can indicate suppression or tampering." >&2
  else
    echo "kettle install-online.sh: ${VERSION} requires Ed25519 verification, but OpenSSL is missing or lacks pkeyutl -rawin support." >&2
    echo "Install OpenSSL 3.0 or newer and re-run." >&2
  fi
  echo "Refusing to downgrade to the weaker same-origin checksum. Aborting." >&2
  exit 1
fi

# --- SHA-256 verification (fallback) --------------------------------
# Only runs for a release that predates signed manifests. Modern releases
# already failed closed above if the signature path was unavailable.
# Releases since v1.3.4
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
# Older releases (≤ v1.3.3) did not publish a sidecar; the one-line installer
# now refuses those rather than execute an unauthenticated archive.
if [ "$MANIFEST_VERIFIED" -ne 1 ]; then
  echo "kettle install-online.sh: signed-manifest verification unavailable for ${VERSION} — falling back to the weaker same-origin SHA-256 sidecar." >&2
  SHA_URL="${URL}.sha256"
  SHA_FILE="${TMP}/${ASSET}.sha256"
  if download_limited "$SHA_URL" "$SHA_FILE" "$MAX_SIDECAR_BYTES" \
    "SHA-256 sidecar" 0 2>/dev/null; then
    if ! SIDECAR_SHA=$(awk -v wanted="$ASSET" '
      NR == 1 && NF == 2 && length($1) == 64 &&
        $1 ~ /^[0-9a-f]+$/ && ($2 == wanted || $2 == "*" wanted) {
          hash = $1
          next
        }
      {
        bad = 1
      }
      END {
        if (bad || NR != 1 || hash == "") {
          exit 1
        }
        print hash
      }
    ' "$SHA_FILE"); then
      echo "kettle install-online.sh: ${SHA_URL} is not one exact lowercase SHA-256 record for ${ASSET}." >&2
      exit 1
    fi
    if command -v sha256sum >/dev/null 2>&1; then
      ACTUAL_SHA=$(sha256sum "$TAR" | awk '{print $1}')
    else
      ACTUAL_SHA=$(shasum -a 256 "$TAR" | awk '{print $1}')
    fi
    if [ "$ACTUAL_SHA" = "$SIDECAR_SHA" ]; then
      echo "kettle: SHA-256 verified. (same-origin checksum only — see install-online.sh for caveats.)"
    else
      echo "kettle install-online.sh: SHA-256 verification FAILED for ${ASSET}." >&2
      echo "The downloaded tarball does not match the hash published on the release." >&2
      echo "Refusing to extract a potentially-tampered archive. Aborting." >&2
      exit 1
    fi
  else
    echo "kettle install-online.sh: no bounded .sha256 sidecar found for legacy release ${VERSION}." >&2
    echo "Refusing to extract an unverified archive; pin to v1.3.4 or newer." >&2
    exit 1
  fi
fi

# --- Bounded archive preflight + extraction -------------------------
# The outer archive is authenticated above. This structural pass still
# prevents a release-pipeline mistake from consuming unbounded disk or asking
# tar to materialize links, devices, path aliases, or writable special modes.
# GNU tar's fixed listing has six fields for Kettle's ASCII-only paths;
# rejecting any other shape also rejects whitespace and control characters.
if ! ARCHIVE_PREFLIGHT=$(
  tar --numeric-owner --full-time --quoting-style=escape -tvzf "$TAR" |
    awk \
      -v max_entries="$MAX_ARCHIVE_ENTRIES" \
      -v max_bytes="$MAX_UNPACKED_BYTES" \
      -v require_manifest="$PACKAGE_MANIFEST_REQUIRED" '
      function reject() {
        bad = 1
        exit 1
      }
      function reserved_device(component, lowered) {
        lowered = tolower(component)
        sub(/\..*$/, "", lowered)
        return lowered == "con" || lowered == "prn" ||
          lowered == "aux" || lowered == "nul" ||
          lowered ~ /^com[1-9]$/ || lowered ~ /^lpt[1-9]$/
      }
      {
        if (NF != 6) {
          reject()
        }
        mode = $1
        size = $3
        path = $6
        type = substr(mode, 1, 1)
        if (length(mode) != 10 || (type != "-" && type != "d") ||
            mode ~ /[sStT]/ || substr(mode, 6, 1) == "w" ||
            substr(mode, 9, 1) == "w" || size !~ /^[0-9]+$/) {
          reject()
        }
        if (type == "d") {
          if (substr(path, length(path), 1) != "/") {
            reject()
          }
          clean = substr(path, 1, length(path) - 1)
        } else {
          if (substr(path, length(path), 1) == "/") {
            reject()
          }
          clean = path
        }
        if (clean == "kettle") {
          if (type != "d") {
            reject()
          }
          saw_root = 1
        } else if (substr(clean, 1, 7) != "kettle/") {
          reject()
        }
        if (clean !~ /^[A-Za-z0-9._+\/-]+$/) {
          reject()
        }

        component_count = split(clean, component, "/")
        prefix = ""
        for (i = 1; i <= component_count; i++) {
          current = component[i]
          if (current == "" || current == "." || current == ".." ||
              length(current) > 255 ||
              substr(current, length(current), 1) == "." ||
              reserved_device(current)) {
            reject()
          }
          prefix = prefix == "" ? current : prefix "/" current
          folded_prefix = tolower(prefix)
          if (i < component_count) {
            if (kind[folded_prefix] == "file") {
              reject()
            }
            needed_directory[folded_prefix] = 1
          }
        }

        folded = tolower(clean)
        if (seen[folded]) {
          reject()
        }
        seen[folded] = 1
        if (type == "-" && needed_directory[folded]) {
          reject()
        }
        kind[folded] = type == "d" ? "directory" : "file"
        entries++
        if (entries > max_entries) {
          reject()
        }
        if (type == "-") {
          total += size
          if (total > max_bytes) {
            reject()
          }
        }
        if (clean == "kettle/kettle-package-manifest.json") {
          if (type != "-" || size > 262144) {
            reject()
          }
          saw_manifest = 1
        }
      }
      END {
        minimum_entries = require_manifest ? 4 : 3
        if (bad || !saw_root ||
            kind["kettle/kettle"] != "file" ||
            kind["kettle/install.sh"] != "file" ||
            (require_manifest && !saw_manifest) ||
            entries < minimum_entries || total <= 0) {
          exit 1
        }
        print entries " " total
      }
    '
); then
  echo "kettle install-online.sh: authenticated archive failed the bounded structural preflight." >&2
  exit 1
fi
set -- $ARCHIVE_PREFLIGHT
if [ "$#" -ne 2 ]; then
  echo "kettle install-online.sh: archive preflight returned an ambiguous result." >&2
  exit 1
fi
PREFLIGHT_ENTRIES=$1
PREFLIGHT_BYTES=$2

EXTRACT_ROOT="${TMP}/extracted"
mkdir -m 700 "$EXTRACT_ROOT"
if ! tar --extract --gzip --file "$TAR" --directory "$EXTRACT_ROOT" \
  --no-same-owner --no-same-permissions --delay-directory-restore \
  --keep-old-files; then
  echo "kettle install-online.sh: bounded archive extraction failed." >&2
  exit 1
fi

SPECIAL_ENTRY=$(find "$EXTRACT_ROOT" -mindepth 1 ! -type f ! -type d -print -quit)
ACTUAL_ENTRIES=$(find "$EXTRACT_ROOT" -mindepth 1 -print | awk 'END { print NR + 0 }')
ACTUAL_BYTES=$(
  find "$EXTRACT_ROOT" -type f -exec wc -c {} \; |
    awk '{ total += $1 } END { print total + 0 }'
)
if [ -n "$SPECIAL_ENTRY" ] ||
  [ "$ACTUAL_ENTRIES" -gt "$MAX_ARCHIVE_ENTRIES" ] ||
  [ "$ACTUAL_BYTES" -ne "$PREFLIGHT_BYTES" ] ||
  [ "$ACTUAL_BYTES" -gt "$MAX_UNPACKED_BYTES" ]; then
  echo "kettle install-online.sh: extracted archive violated its preflight bounds." >&2
  echo "entries=${ACTUAL_ENTRIES}/${PREFLIGHT_ENTRIES}, bytes=${ACTUAL_BYTES}/${PREFLIGHT_BYTES}" >&2
  exit 1
fi

PACKAGE_ROOT="${EXTRACT_ROOT}/kettle"
if [ ! -x "$PACKAGE_ROOT/install.sh" ]; then
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
  if ! "$PACKAGE_ROOT/install.sh" --skip-build "--prefix=$KETTLE_PREFIX" > "$INSTALL_LOG"; then
    cat "$INSTALL_LOG"
    exit 1
  fi
else
  if ! "$PACKAGE_ROOT/install.sh" --skip-build > "$INSTALL_LOG"; then
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
cp "$PACKAGE_ROOT/install.sh" "$INSTALL_REAL"
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
if [ -d "$PACKAGE_ROOT/shell-integration" ]; then
  mkdir -p "${INSTALL_PREFIX}/share/kettle/shell-integration"
  cp "$PACKAGE_ROOT/shell-integration/"* "${INSTALL_PREFIX}/share/kettle/shell-integration/"
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
