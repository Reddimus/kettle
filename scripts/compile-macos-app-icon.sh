#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 || -z $1 ]]; then
  echo "usage: $0 OUTPUT_DIR" >&2
  exit 2
fi

repo_root=$(cd "$(dirname "$0")/.." && pwd)
output_dir=$1
required_major=26

# Pin the release compiler to Xcode 26. Selecting "newest" without a major cap
# would silently move release assets to an Xcode 27 preview as soon as one
# appears on a hosted image. The current developer directory is included for
# developer machines whose Xcode app has a nonstandard name or location.
candidate_file=$(mktemp "${TMPDIR:-/tmp}/kettle-xcode-candidates.XXXXXX")
trap 'rm -f "$candidate_file"' EXIT

xcode-select -p 2>/dev/null >>"$candidate_file" || true
find /Applications -maxdepth 1 -type d -name 'Xcode*.app' -print 2>/dev/null \
  | while IFS= read -r app; do
      printf '%s/Contents/Developer\n' "$app"
    done >>"$candidate_file"

best_version=
best_developer_dir=
while IFS= read -r developer_dir; do
  [[ -x "$developer_dir/usr/bin/xcodebuild" ]] || continue
  version=$("$developer_dir/usr/bin/xcodebuild" -version 2>/dev/null \
    | awk 'NR == 1 && $1 == "Xcode" { print $2 }')
  [[ $version =~ ^[0-9]+([.][0-9]+)*$ ]] || continue
  major=${version%%.*}
  (( major == required_major )) || continue

  if [[ -z $best_version ]] \
    || [[ $(printf '%s\n%s\n' "$best_version" "$version" | sort -V | tail -n1) == "$version" ]]; then
    best_version=$version
    best_developer_dir=$developer_dir
  fi
done < <(sort -u "$candidate_file")

if [[ -z $best_developer_dir ]]; then
  echo "error: compiling AppIcon.icon requires Xcode ${required_major}.x" >&2
  echo "installed Xcode developer directories:" >&2
  sed 's/^/  /' "$candidate_file" >&2
  exit 1
fi

mkdir -p "$output_dir"
for artifact in Assets.car AppIcon.icns partial.plist; do
  if [[ -e "$output_dir/$artifact" ]]; then
    echo "error: refusing stale AppIcon output: $output_dir/$artifact" >&2
    exit 1
  fi
done
echo "Compiling AppIcon.icon with Xcode $best_version ($best_developer_dir)"
DEVELOPER_DIR=$best_developer_dir xcrun actool \
  "$repo_root/packaging/macos/AppIcon.icon" \
  --compile "$output_dir" \
  --platform macosx \
  --minimum-deployment-target 11.0 \
  --app-icon AppIcon \
  --output-partial-info-plist "$output_dir/partial.plist"

test -s "$output_dir/Assets.car"
test -s "$output_dir/AppIcon.icns"
test "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIconName' \
  "$output_dir/partial.plist")" = AppIcon
test "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIconFile' \
  "$output_dir/partial.plist")" = AppIcon
echo "actool AppIcon OK"
