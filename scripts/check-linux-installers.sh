#!/usr/bin/env bash
# Smoke the Linux installers without touching the user's real ~/.local install.

set -euo pipefail

ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$ROOT"

fail() {
  echo "linux-installer check: $*" >&2
  exit 1
}

version=$(awk -F\" '/^version = "/ { print $2; exit }' Cargo.toml)
[ -n "$version" ] || fail "could not read workspace version from Cargo.toml"

if [ ! -x target/release/kettle ]; then
  fail "target/release/kettle missing; run cargo build --release -p kettle first"
fi

tmp_root=$(mktemp -d /tmp/kettle-install-smoke.XXXXXX)
trap 'rm -rf "$tmp_root"' EXIT INT TERM

assert_abs_desktop_paths() {
  local prefix=$1
  local desktop="${prefix}/share/applications/kettle.desktop"
  grep -qx "Name=Kettle" "$desktop" \
    || fail "desktop Name is not the user-facing app name Kettle"
  grep -qx "Exec=${prefix}/bin/kettle" "$desktop" \
    || fail "desktop Exec does not point at ${prefix}/bin/kettle"
  grep -qx "TryExec=${prefix}/bin/kettle" "$desktop" \
    || fail "desktop TryExec does not point at ${prefix}/bin/kettle"
  grep -qx "Icon=${prefix}/share/icons/hicolor/256x256/apps/kettle.png" "$desktop" \
    || fail "desktop Icon does not point at the prefix-local PNG"
}

assert_installed_prefix() {
  local prefix=$1
  [ -x "${prefix}/bin/kettle" ] || fail "missing installed binary in ${prefix}"
  [ -x "${prefix}/share/kettle/install.sh" ] \
    || fail "missing saved uninstall helper in ${prefix}"
  [ -f "${prefix}/share/kettle/install.json" ] \
    || fail "missing self-update ownership marker in ${prefix}"
  grep -q '"managed_by": "kettle-installer"' "${prefix}/share/kettle/install.json" \
    || fail "invalid self-update ownership marker in ${prefix}"
  [ -f "${prefix}/share/kettle/shell-integration/kettle.bash" ] \
    || fail "missing installed shell integration in ${prefix}"
  [ -f "${prefix}/share/man/man1/kettle.1" ] || fail "missing installed man page"
  [ -f "${prefix}/share/icons/hicolor/scalable/apps/kettle.svg" ] \
    || fail "missing installed SVG icon"
  [ -f "${prefix}/share/icons/hicolor/256x256/apps/kettle.png" ] \
    || fail "missing installed PNG icon"
  assert_abs_desktop_paths "$prefix"
  "${prefix}/bin/kettle" --version | grep -qE '^kettle [0-9]+\.[0-9]+\.[0-9]+' \
    || fail "installed binary did not print a kettle version"
}

assert_uninstalled_prefix() {
  local prefix=$1
  [ ! -e "${prefix}/bin/kettle" ] || fail "binary survived uninstall"
  [ ! -e "${prefix}/share/applications/kettle.desktop" ] \
    || fail "desktop entry survived uninstall"
  [ ! -e "${prefix}/share/kettle/install.sh" ] || fail "helper survived uninstall"
  [ ! -e "${prefix}/share/kettle/install-real.sh" ] \
    || fail "online real helper survived uninstall"
  [ ! -e "${prefix}/share/kettle/install.json" ] \
    || fail "self-update ownership marker survived uninstall"
}

direct_prefix="${tmp_root}/direct"
./scripts/install.sh --skip-build "--prefix=${direct_prefix}" > "${tmp_root}/direct-install.out"
grep -q "To uninstall: ${direct_prefix}/share/kettle/install.sh --uninstall" \
  "${tmp_root}/direct-install.out" \
  || fail "direct installer did not print prefix-aware uninstall hint"
assert_installed_prefix "$direct_prefix"
"${direct_prefix}/share/kettle/install.sh" --uninstall > "${tmp_root}/direct-uninstall.out"
assert_uninstalled_prefix "$direct_prefix"
echo "linux-installer check: direct custom-prefix install/uninstall OK"

repo=${KETTLE_GITHUB_REPO:-Reddimus/kettle}
tag="v${version}"
remote="https://github.com/${repo}.git"
if ! git ls-remote --exit-code --tags "$remote" "refs/tags/${tag}" >/dev/null 2>&1; then
  echo "linux-installer check: ${tag} is not published yet; skipping online installer smoke"
  exit 0
fi

online_prefix="${tmp_root}/online"
KETTLE_VERSION="$tag" KETTLE_PREFIX="$online_prefix" sh scripts/install-online.sh \
  > "${tmp_root}/online-install.out"
grep -q 'kettle: SHA-256 verified\.' "${tmp_root}/online-install.out" \
  || fail "online installer did not verify SHA-256"
grep -q "Uninstall later via:" "${tmp_root}/online-install.out" \
  || fail "online installer did not print uninstall section"
grep -q "${online_prefix}/share/kettle/install.sh --uninstall" \
  "${tmp_root}/online-install.out" \
  || fail "online installer did not print prefix-aware uninstall helper"
if grep -q '^To uninstall: ./scripts/install.sh --uninstall$' \
    "${tmp_root}/online-install.out"; then
  fail "online installer leaked the old bundled uninstall hint"
fi
assert_installed_prefix "$online_prefix"
[ -x "${online_prefix}/share/kettle/install-real.sh" ] \
  || fail "online installer did not save real bundled helper"
"${online_prefix}/share/kettle/install.sh" --uninstall > "${tmp_root}/online-uninstall.out"
assert_uninstalled_prefix "$online_prefix"
echo "linux-installer check: online published-release install/uninstall OK"
