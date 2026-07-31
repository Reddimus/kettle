#!/usr/bin/env bash
# Smoke the Linux installers without touching the user's real ~/.local install.

set -euo pipefail
export PATH="$HOME/.cargo/bin:$PATH"

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
normal_binary="${tmp_root}/kettle-release"
cp target/release/kettle "${normal_binary}"
cargo_hardlink_alias="target/release/.kettle-installer-hardlink-smoke.$$"
cleanup() {
  rm -f -- "${cargo_hardlink_alias}"
  cp "${normal_binary}" target/release/kettle 2>/dev/null || true
  rm -rf "${tmp_root}"
}
trap cleanup EXIT INT TERM

# Keep the alias beside the binary: WSL may place /tmp and the checkout on
# different filesystems, while hardlinks necessarily stay within one device.
ln -- target/release/kettle "${cargo_hardlink_alias}"
[ "$(stat -c '%h' target/release/kettle)" -gt 1 ] \
  || fail "could not reproduce Cargo's hardlinked release artifact"

if ./scripts/install.sh --skip-build --record-dir= > /dev/null 2>&1; then
  fail "installer accepted an empty development recording directory"
fi

assert_abs_desktop_paths() {
  local prefix=$1
  local record_dir=${2:-}
  local desktop="${prefix}/share/applications/kettle.desktop"
  local binary="${prefix}/bin/kettle"
  local icon="${prefix}/share/icons/hicolor/256x256/apps/kettle.png"
  desktop_string_escape() {
    printf '%s' "$1" | sed 's/\\/\\\\/g'
  }
  desktop_exec_quote() {
    local escaped
    # shellcheck disable=SC2016
    escaped=$(printf '%s' "$1" | sed \
      -e 's/\\/\\\\/g' \
      -e 's/"/\\"/g' \
      -e 's/`/\\`/g' \
      -e 's/\$/\\$/g' \
      -e 's/%/%%/g')
    printf '"%s"' "$(desktop_string_escape "${escaped}")"
  }
  grep -qx "Name=Kettle" "$desktop" \
    || fail "desktop Name is not the user-facing app name Kettle"
  if [ -n "$record_dir" ]; then
    grep -Fqx "Exec=/usr/bin/env $(desktop_exec_quote "KETTLE_RECORD_DIR=${record_dir}") $(desktop_exec_quote "${binary}")" "$desktop" \
      || fail "desktop recording Exec does not preserve its argument boundaries"
  else
    grep -Fqx "Exec=$(desktop_exec_quote "${binary}")" "$desktop" \
      || fail "desktop Exec does not point at ${prefix}/bin/kettle"
  fi
  grep -Fqx "TryExec=$(desktop_string_escape "${binary}")" "$desktop" \
    || fail "desktop TryExec does not point at ${prefix}/bin/kettle"
  grep -Fqx "Icon=$(desktop_string_escape "${icon}")" "$desktop" \
    || fail "desktop Icon does not point at the prefix-local PNG"
  if command -v desktop-file-validate >/dev/null 2>&1; then
    desktop-file-validate "$desktop" || fail "generated desktop entry is invalid"
  fi
}

assert_desktop_launch_argv() {
  local prefix=$1
  local expected_record_dir=$2
  command -v gio >/dev/null 2>&1 || fail "gio is required to verify desktop Exec argument parsing"
  local desktop="${prefix}/share/applications/kettle.desktop"
  local binary="${prefix}/bin/kettle"
  local original="${prefix}/bin/kettle.desktop-test-original"
  local probe="${tmp_root}/desktop-launch.probe"
  mv -- "${binary}" "${original}"
  # shellcheck disable=SC2016 # The second line is the generated probe script.
  printf '%s\n' \
    '#!/usr/bin/env bash' \
    'printf '\''%s\n%s\n%s\n%s\n'\'' "$0" "$#" "${1-}" "${KETTLE_RECORD_DIR-}" > "${KETTLE_DESKTOP_PROBE:?}"' \
    > "${binary}"
  chmod 755 -- "${binary}"
  KETTLE_DESKTOP_PROBE="${probe}" gio launch "${desktop}"
  local deadline=$((SECONDS + 10))
  while [[ ! -f "${probe}" && ${SECONDS} -lt ${deadline} ]]; do
    sleep 0.05
  done
  [[ -f "${probe}" ]] || fail "desktop launch did not execute the probe binary"
  mapfile -t desktop_probe < "${probe}"
  [[ "${desktop_probe[0]-}" == "${binary}" ]] \
    || fail "desktop Exec decoded the binary path incorrectly"
  [[ "${desktop_probe[1]-}" == "0" && -z "${desktop_probe[2]-}" ]] \
    || fail "desktop Exec introduced unexpected arguments"
  [[ "${desktop_probe[3]-}" == "${expected_record_dir}" ]] \
    || fail "desktop Exec decoded the recording directory incorrectly"
  mv -- "${original}" "${binary}"
}

assert_installed_prefix() {
  local prefix=$1
  local expected_channel=$2
  local record_dir=${3:-}
  [ -x "${prefix}/bin/kettle" ] || fail "missing installed binary in ${prefix}"
  [ -x "${prefix}/share/kettle/install.sh" ] \
    || fail "missing saved uninstall helper in ${prefix}"
  [ -x "${prefix}/share/kettle/install-unix.py" ] \
    || fail "missing saved no-follow installer helper in ${prefix}"
  [ -f "${prefix}/share/kettle/install-files.json" ] \
    || fail "missing install provenance manifest in ${prefix}"
  [ -f "${prefix}/share/kettle/install.json" ] \
    || fail "missing self-update ownership marker in ${prefix}"
  grep -q '"managed_by": "kettle-installer"' "${prefix}/share/kettle/install.json" \
    || fail "invalid self-update ownership marker in ${prefix}"
  grep -q "\"channel\": \"${expected_channel}\"" "${prefix}/share/kettle/install.json" \
    || fail "expected ${expected_channel} install marker in ${prefix}"
  [ -f "${prefix}/share/kettle/shell-integration/kettle.bash" ] \
    || fail "missing installed shell integration in ${prefix}"
  [ -f "${prefix}/share/man/man1/kettle.1" ] || fail "missing installed man page"
  [ -f "${prefix}/share/icons/hicolor/scalable/apps/kettle.svg" ] \
    || fail "missing installed SVG icon"
  [ -f "${prefix}/share/icons/hicolor/256x256/apps/kettle.png" ] \
    || fail "missing installed PNG icon"
  assert_abs_desktop_paths "$prefix" "$record_dir"
  "${prefix}/bin/kettle" --version | grep -qE '^kettle [0-9]+\.[0-9]+\.[0-9]+' \
    || fail "installed binary did not print a kettle version"
}

assert_uninstalled_prefix() {
  local prefix=$1
  [ ! -e "${prefix}/bin/kettle" ] || fail "binary survived uninstall"
  [ ! -e "${prefix}/share/applications/kettle.desktop" ] \
    || fail "desktop entry survived uninstall"
  [ ! -e "${prefix}/share/kettle/install.sh" ] || fail "helper survived uninstall"
  [ ! -e "${prefix}/share/kettle/install-unix.py" ] \
    || fail "no-follow helper survived uninstall"
  [ ! -e "${prefix}/share/kettle/install-files.json" ] \
    || fail "install provenance survived uninstall"
  [ ! -e "${prefix}/share/kettle/install-real.sh" ] \
    || fail "online real helper survived uninstall"
  [ ! -e "${prefix}/share/kettle/install.json" ] \
    || fail "self-update ownership marker survived uninstall"
}

direct_prefix="${tmp_root}/direct"
mkdir -p "${direct_prefix}/unrelated"
printf '%s' 'unrelated prefix content' > "${direct_prefix}/unrelated/must-survive.txt"
./scripts/install.sh --skip-build "--prefix=${direct_prefix}" > "${tmp_root}/direct-install.out"
grep -q "To uninstall: ${direct_prefix}/share/kettle/install.sh --uninstall" \
  "${tmp_root}/direct-install.out" \
  || fail "direct installer did not print prefix-aware uninstall hint"
assert_installed_prefix "$direct_prefix" "local-dev"
"${direct_prefix}/share/kettle/install.sh" --uninstall > "${tmp_root}/direct-uninstall.out"
assert_uninstalled_prefix "$direct_prefix"
[ "$(cat "${direct_prefix}/unrelated/must-survive.txt")" = 'unrelated prefix content' ] \
  || fail "installer removed unrelated shared-prefix content"
echo "linux-installer check: hardlinked Cargo source accepted"
echo "linux-installer check: direct custom-prefix install/uninstall OK"

# The source may be hardlinked because it is only read. A managed destination
# is different: reject an alias before upgrade or uninstall can replace/remove
# any recorded path.
hardlink_prefix="${tmp_root}/destination-hardlink"
managed_hardlink_alias="${tmp_root}/managed-kettle-hardlink"
./scripts/install.sh --skip-build "--prefix=${hardlink_prefix}" >/dev/null
ln -- "${hardlink_prefix}/bin/kettle" "${managed_hardlink_alias}"
if ./scripts/install.sh --skip-build "--prefix=${hardlink_prefix}" \
    >"${tmp_root}/hardlink-upgrade.out" 2>"${tmp_root}/hardlink-upgrade.err"; then
  fail "installer accepted a hardlinked managed destination"
fi
grep -Fq 'recorded install file changed identity: bin/kettle' \
  "${tmp_root}/hardlink-upgrade.err" \
  || fail "hardlinked managed destination failed for the wrong reason"
[ -f "${managed_hardlink_alias}" ] \
  || fail "installer removed the managed destination hardlink alias"
rm -- "${managed_hardlink_alias}"
"${hardlink_prefix}/share/kettle/install.sh" --uninstall >/dev/null
assert_uninstalled_prefix "$hardlink_prefix"
echo "linux-installer check: hardlinked managed destination refused"

# Reproduce the audited traversal: replace the recorded share/kettle directory
# with a symlink to an unrelated tree. The hardened uninstaller must refuse
# before removing any recorded path, and the victim must remain byte-for-byte.
attack_prefix="${tmp_root}/symlink-attack"
victim="${tmp_root}/victim"
mkdir -p "${victim}/shell-integration"
printf '%s' 'must survive' > "${victim}/shell-integration/sentinel.txt"
./scripts/install.sh --skip-build "--prefix=${attack_prefix}" >/dev/null
mv -- "${attack_prefix}/share/kettle" "${attack_prefix}/share/kettle-recorded"
ln -s "${victim}" "${attack_prefix}/share/kettle"
if ./scripts/install.sh --uninstall "--prefix=${attack_prefix}" \
    >"${tmp_root}/symlink-uninstall.out" 2>"${tmp_root}/symlink-uninstall.err"; then
  fail "uninstaller followed or accepted a replacement share/kettle symlink"
fi
[ -f "${victim}/shell-integration/sentinel.txt" ] \
  || fail "uninstaller deleted through the replacement share/kettle symlink"
[ "$(cat "${victim}/shell-integration/sentinel.txt")" = 'must survive' ] \
  || fail "uninstaller changed the symlink victim sentinel"
[ -x "${attack_prefix}/bin/kettle" ] \
  || fail "uninstaller mutated recorded files before symlink preflight failed"
echo "linux-installer check: symlink-traversal victim survived (uninstall refused)"

symlink_target="${tmp_root}/symlink target"
symlink_record="${tmp_root}/symlink records"
mkdir -p "${symlink_target}"
ln -s "${symlink_target}" "${symlink_record}"
if ./scripts/install.sh --skip-build "--prefix=${tmp_root}/must-refuse-symlink" \
    "--record-dir=${symlink_record}" >/dev/null 2>&1; then
  fail "installer accepted a symlink as the recording directory"
fi

# A source-checkout install with --record-dir now needs no special build
# (recording ships in every binary); it stays `local-dev` (self-update refused,
# rebuild to update) and wires KETTLE_RECORD_DIR into the launcher.
dev_prefix="${tmp_root}/dev back\\slash % dollar\$ quote\" tick\`"
record_dir="${tmp_root}/record back\\slash % dollar\$ quote\" tick\`"
./scripts/install.sh --skip-build "--prefix=${dev_prefix}" "--record-dir=${record_dir}" \
  > "${tmp_root}/dev-install.out"
assert_installed_prefix "$dev_prefix" "local-dev" "$record_dir"
[ "$(stat -c '%a' "$record_dir")" = "700" ] \
  || fail "recording directory is not mode 0700"
assert_desktop_launch_argv "$dev_prefix" "$record_dir"
"${dev_prefix}/share/kettle/install.sh" --uninstall > "${tmp_root}/dev-uninstall.out"
assert_uninstalled_prefix "$dev_prefix"
echo "linux-installer check: spaced recording prefix/install/uninstall OK"

# Restore the exact release binary for the simulated public bundle and for any
# CI smoke that follows this script.
cp "${normal_binary}" target/release/kettle

# Exercise the release-tarball layout without requiring a published tag. Only
# this layout (and install-online.sh) may emit a stable self-update marker.
bundle="${tmp_root}/bundle"
mkdir -p "${bundle}/packaging"
cp "${normal_binary}" "${bundle}/kettle"
cp scripts/install.sh "${bundle}/install.sh"
cp scripts/install-unix.py "${bundle}/install-unix.py"
cp -R packaging/linux "${bundle}/packaging/linux"
cp -R shell-integration "${bundle}/shell-integration"

# A release-tarball (`stable`) install must refuse --record-dir: the launcher
# env wiring would be dropped on the next self-update, so users are steered to
# the update-surviving `record = on` config key instead.
if "${bundle}/install.sh" --skip-build --prefix="${tmp_root}/tarball-refuse-record" \
    --record-dir="${tmp_root}/tarball-records" > /dev/null 2>&1; then
  fail "release-tarball installer accepted --record-dir (would be lost on self-update)"
fi

tarball_prefix="${tmp_root}/tarball"
"${bundle}/install.sh" --skip-build "--prefix=${tarball_prefix}" \
  > "${tmp_root}/tarball-install.out"
assert_installed_prefix "$tarball_prefix" "stable"
"${tarball_prefix}/share/kettle/install.sh" --uninstall \
  > "${tmp_root}/tarball-uninstall.out"
assert_uninstalled_prefix "$tarball_prefix"
echo "linux-installer check: simulated release-tarball channel OK"

repo=${KETTLE_GITHUB_REPO:-Reddimus/kettle}
tag="v${version}"
remote="https://github.com/${repo}.git"
if ! git ls-remote --exit-code --tags "$remote" "refs/tags/${tag}" >/dev/null 2>&1; then
  echo "linux-installer check: ${tag} is not published yet; skipping online installer smoke"
  exit 0
fi

# A signed tag can become visible before the release finalizer publishes its
# assets. Treat that normal publication window like an unpublished tag rather
# than deadlocking every PR on a guaranteed 404. Other HTTP failures remain
# errors so genuine GitHub/network outages do not silently reduce coverage.
case "$(uname -m)" in
  x86_64 | amd64) online_asset="kettle-linux-x86_64.tar.gz" ;;
  aarch64 | arm64) online_asset="kettle-linux-aarch64.tar.gz" ;;
  *) fail "online installer smoke has no release asset for $(uname -m)" ;;
esac
asset_url="https://github.com/${repo}/releases/download/${tag}/${online_asset}"
asset_status=$(curl -sSLI -o /dev/null -w '%{http_code}' "$asset_url") \
  || fail "could not check published asset ${asset_url}"
if [ "$asset_status" = "404" ]; then
  echo "linux-installer check: ${tag} asset is not published yet; skipping online installer smoke"
  exit 0
fi
[ "$asset_status" = "200" ] \
  || fail "published asset probe returned HTTP ${asset_status} for ${asset_url}"

online_prefix="${tmp_root}/online"
if ! KETTLE_VERSION="$tag" KETTLE_PREFIX="$online_prefix" \
    sh scripts/install-online.sh \
      > "${tmp_root}/online-install.out" 2> "${tmp_root}/online-install.err"; then
  if grep -Fq \
      'release lacks the hardened install-unix.py helper; refusing a legacy path-based install' \
      "${tmp_root}/online-install.err"; then
    echo "linux-installer check: online published-release install/uninstall NOT RUN: ${tag} predates hardened provenance; legacy refusal verified"
    exit 0
  fi
  cat "${tmp_root}/online-install.out"
  cat "${tmp_root}/online-install.err" >&2
  fail "online installer failed"
fi
grep -Fq \
  'kettle: SHA-256 verified. (Ed25519-signed manifest' \
  "${tmp_root}/online-install.out" \
  || {
    cat "${tmp_root}/online-install.err" >&2
    fail "online installer did not use the mandatory Ed25519-signed manifest"
  }
if grep -Fq 'falling back to the weaker same-origin SHA-256 sidecar' \
    "${tmp_root}/online-install.err"; then
  fail "online installer downgraded a modern release to the checksum sidecar"
fi
grep -q "Uninstall later via:" "${tmp_root}/online-install.out" \
  || fail "online installer did not print uninstall section"
grep -q "${online_prefix}/share/kettle/install.sh --uninstall" \
  "${tmp_root}/online-install.out" \
  || fail "online installer did not print prefix-aware uninstall helper"
if grep -q '^To uninstall: ./scripts/install.sh --uninstall$' \
    "${tmp_root}/online-install.out"; then
  fail "online installer leaked the old bundled uninstall hint"
fi
assert_installed_prefix "$online_prefix" "stable"
[ ! -e "${online_prefix}/share/kettle/install-real.sh" ] \
  || fail "online installer rewrote the provenance-managed helper"
"${online_prefix}/share/kettle/install.sh" --uninstall > "${tmp_root}/online-uninstall.out"
assert_uninstalled_prefix "$online_prefix"
echo "linux-installer check: online published-release install/uninstall OK"
