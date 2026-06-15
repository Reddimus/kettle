#!/usr/bin/env bash
# Verify that source-tree package templates track the workspace version and,
# once a matching GitHub release tag exists, the published artifact hashes.

set -euo pipefail

MODE="auto"
case "${1:-}" in
  ""|--auto) MODE="auto" ;;
  --local) MODE="local" ;;
  --require-release) MODE="require-release" ;;
  -h|--help)
    cat <<'EOF'
usage: scripts/check-package-templates.sh [--auto|--local|--require-release]

  --auto             verify local template lockstep; if v<VERSION> exists on
                     GitHub, also verify hashes against release .sha256 files
  --local            only verify local template lockstep
  --require-release  require v<VERSION> and verify published hashes
EOF
    exit 0
    ;;
  *) echo "unknown arg: $1" >&2; exit 2 ;;
esac

ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$ROOT"

fail() {
  echo "package-template check: $*" >&2
  exit 1
}

need_file() {
  [ -f "$1" ] || fail "missing $1"
}

need_file Cargo.toml
need_file packaging/homebrew/kettle.rb
need_file packaging/arch/PKGBUILD

version=$(awk -F\" '/^version = "/ { print $2; exit }' Cargo.toml)
[ -n "$version" ] || fail "could not read workspace version from Cargo.toml"

formula_version=$(sed -n 's/^  version "\([^"]\+\)"/\1/p' packaging/homebrew/kettle.rb)
arch_version=$(sed -n 's/^pkgver=\([^[:space:]]\+\)$/\1/p' packaging/arch/PKGBUILD)

[ "$formula_version" = "$version" ] \
  || fail "Homebrew version $formula_version does not match workspace $version"
[ "$arch_version" = "$version" ] \
  || fail "Arch pkgver $arch_version does not match workspace $version"

mapfile -t formula_hashes < <(
  sed -n 's/^[[:space:]]*sha256 "\([0-9a-f]\{64\}\)".*/\1/p' \
    packaging/homebrew/kettle.rb
)
[ "${#formula_hashes[@]}" -eq 2 ] \
  || fail "expected exactly 2 Homebrew sha256 entries, found ${#formula_hashes[@]}"

mac_hash=${formula_hashes[0]}
linux_hash=${formula_hashes[1]}
arch_hash=$(sed -n "s/^sha256sums=('\([0-9a-f]\{64\}\)')$/\1/p" \
  packaging/arch/PKGBUILD)
[ -n "$arch_hash" ] || fail "could not read Arch sha256sums entry"
[ "$arch_hash" = "$linux_hash" ] \
  || fail "Arch Linux hash does not match Homebrew Linux hash"

echo "package-template check: local versions/hashes are internally consistent for $version"

if [ "$MODE" = "local" ]; then
  exit 0
fi

repo=${KETTLE_GITHUB_REPO:-Reddimus/kettle}
tag="v${version}"
remote="https://github.com/${repo}.git"

if ! git ls-remote --exit-code --tags "$remote" "refs/tags/${tag}" >/dev/null 2>&1; then
  if [ "$MODE" = "require-release" ]; then
    fail "release tag ${tag} does not exist on ${repo}"
  fi
  echo "package-template check: ${tag} is not published yet; skipping remote hash check"
  exit 0
fi

command -v curl >/dev/null 2>&1 || fail "curl is required for remote hash check"

fetch_hash() {
  local asset=$1
  local url="https://github.com/${repo}/releases/download/${tag}/${asset}.sha256"
  local line
  local hash
  line=$(curl -fsSL "$url") || fail "could not fetch $url"
  read -r hash _ <<< "$line"
  [ "${#hash}" -eq 64 ] || fail "bad sha256 sidecar for ${asset}: ${line}"
  printf '%s\n' "$hash"
}

published_mac=$(fetch_hash kettle-macos-universal.zip)
published_linux=$(fetch_hash kettle-linux-x86_64.tar.gz)

[ "$mac_hash" = "$published_mac" ] \
  || fail "Homebrew macOS hash $mac_hash does not match published $published_mac"
[ "$linux_hash" = "$published_linux" ] \
  || fail "Linux x86_64 hash $linux_hash does not match published $published_linux"
[ "$arch_hash" = "$published_linux" ] \
  || fail "Arch hash $arch_hash does not match published $published_linux"

echo "package-template check: published ${tag} hashes match Homebrew and Arch templates"
