#!/usr/bin/env bash
# Validate source templates and, once published, their rendered release assets.

set -euo pipefail

MODE="auto"
case "${1:-}" in
  ""|--auto) MODE="auto" ;;
  --local) MODE="local" ;;
  --require-release) MODE="require-release" ;;
  -h|--help)
    cat <<'EOF'
usage: scripts/check-package-templates.sh [--auto|--local|--require-release]

  --auto             validate source templates; when v<VERSION> exists,
                     also validate its generated package-metadata assets
  --local            validate only the source templates and renderer
  --require-release  require v<VERSION> and validate its published metadata
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

for file in \
  Cargo.toml \
  packaging/homebrew/kettle.rb.in \
  packaging/arch/PKGBUILD.in \
  scripts/render-package-templates.py \
  scripts/test-package-templates.py; do
  [ -f "$file" ] || fail "missing $file"
done

command -v python3 >/dev/null 2>&1 || fail "python3 is required"
python3 scripts/test-package-templates.py

version=$(awk -F\" '/^version = "/ { print $2; exit }' Cargo.toml)
[ -n "$version" ] || fail "could not read workspace version from Cargo.toml"
echo "package-template check: source templates render deterministically for $version"

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
  echo "package-template check: ${tag} is not published yet; skipping release-asset check"
  exit 0
fi

command -v curl >/dev/null 2>&1 || fail "curl is required for release-asset checks"
temporary=$(mktemp -d)
trap 'rm -rf "$temporary"' EXIT

fetch_asset() {
  local asset=$1
  local url="https://github.com/${repo}/releases/download/${tag}/${asset}"
  if ! curl -fsSL "$url" -o "$temporary/$asset"; then
    if [ "$MODE" = "require-release" ]; then
      fail "could not fetch $url"
    fi
    echo "package-template check: ${tag} exists but ${asset} is not reachable yet; skipping release-asset check" >&2
    exit 0
  fi
}

fetch_asset kettle.rb
fetch_asset PKGBUILD
fetch_asset kettle-macos-universal.zip.sha256
fetch_asset kettle-linux-x86_64.tar.gz.sha256

sidecar_hash() {
  local sidecar=$1
  local expected_name=$2
  local hash name extra
  read -r hash name extra < "$sidecar" || fail "could not parse $sidecar"
  [[ "$hash" =~ ^[0-9a-f]{64}$ ]] || fail "invalid SHA-256 in $sidecar"
  [ "$name" = "$expected_name" ] || fail "unexpected filename in $sidecar: $name"
  [ -z "${extra:-}" ] || fail "unexpected trailing fields in $sidecar"
  printf '%s\n' "$hash"
}

published_mac=$(sidecar_hash "$temporary/kettle-macos-universal.zip.sha256" kettle-macos-universal.zip)
published_linux=$(sidecar_hash "$temporary/kettle-linux-x86_64.tar.gz.sha256" kettle-linux-x86_64.tar.gz)
formula_version=$(sed -n 's/^  version "\([^"]\+\)"/\1/p' "$temporary/kettle.rb")
arch_version=$(sed -n 's/^pkgver=\([^[:space:]]\+\)$/\1/p' "$temporary/PKGBUILD")
mapfile -t formula_hashes < <(
  sed -n 's/^[[:space:]]*sha256 "\([0-9a-f]\{64\}\)".*/\1/p' "$temporary/kettle.rb"
)
arch_hash=$(sed -n "s/^sha256sums=('\([0-9a-f]\{64\}\)')$/\1/p" "$temporary/PKGBUILD")

[ "$formula_version" = "$version" ] || fail "Homebrew release asset version $formula_version does not match $version"
[ "$arch_version" = "$version" ] || fail "Arch release asset version $arch_version does not match $version"
[ "${#formula_hashes[@]}" -eq 2 ] || fail "expected two Homebrew hashes, found ${#formula_hashes[@]}"
[ "${formula_hashes[0]}" = "$published_mac" ] || fail "Homebrew macOS hash does not match its sidecar"
[ "${formula_hashes[1]}" = "$published_linux" ] || fail "Homebrew Linux hash does not match its sidecar"
[ "$arch_hash" = "$published_linux" ] || fail "Arch hash does not match the Linux sidecar"

echo "package-template check: published ${tag} metadata matches verified sidecars"
