#!/usr/bin/env bash
# Exercise the macOS bundle updater against a genuinely notarized archive.
#
# No synthesized fixture can be notarized, and the published archive is 27 MiB,
# so the one path that cannot be unit tested is the one that matters most:
# whether a real release archive survives plain zip extraction with its seal
# intact, and whether the bundle that lands after an atomic swap is still one
# Gatekeeper accepts.
#
# Downloads the archive for a tag (default: the latest release) and runs the
# live test with KETTLE_MACOS_ARCHIVE_REQUIRED set, so a missing archive fails
# instead of quietly skipping.
#
# Usage: scripts/check-macos-update-smoke.sh [TAG]

set -euo pipefail

if [ "$(uname -s)" != "Darwin" ]; then
    echo "check-macos-update-smoke.sh: macOS only; skipping on $(uname -s)."
    exit 0
fi

REPO=${KETTLE_GITHUB_REPO:-Reddimus/kettle}
TAG=${1:-}
ASSET=kettle-macos-universal.zip
CACHE=${KETTLE_MACOS_ARCHIVE_CACHE:-target/diagnostics/macos-update-smoke}

if ! command -v gh >/dev/null 2>&1; then
    echo "::error::gh is required to download the release archive" >&2
    exit 1
fi

mkdir -p "$CACHE"
if [ -z "$TAG" ]; then
    TAG=$(gh release view --repo "$REPO" --json tagName --jq .tagName)
fi
archive="$CACHE/$TAG-$ASSET"

if [ ! -f "$archive" ]; then
    echo "downloading $ASSET from $TAG"
    gh release download "$TAG" --repo "$REPO" --pattern "$ASSET" \
        --output "$archive" --clobber
fi

# The sidecar binds these bytes to the release, so a corrupted or substituted
# download fails here rather than inside the test as a confusing seal error.
sidecar="$CACHE/$TAG-$ASSET.sha256"
if [ ! -f "$sidecar" ]; then
    gh release download "$TAG" --repo "$REPO" --pattern "$ASSET.sha256" \
        --output "$sidecar" --clobber
fi
expected=$(awk '{print $1}' "$sidecar" | head -1)
actual=$(shasum -a 256 "$archive" | awk '{print $1}')
if [ "$expected" != "$actual" ]; then
    echo "::error::$ASSET does not match its published SHA-256 sidecar" >&2
    exit 1
fi

echo "running the live bundle update check against $TAG"
KETTLE_MACOS_ARCHIVE="$PWD/$archive" \
KETTLE_MACOS_ARCHIVE_REQUIRED=1 \
    cargo test -p kettle-update --lib a_published_archive -- --nocapture
