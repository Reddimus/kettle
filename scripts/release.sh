#!/usr/bin/env bash
# scripts/release.sh — atomic bump + tag for kettle releases.
#
# Solves the cycle-307 race condition: tagging BEFORE the
# CHANGELOG.md `[X.Y.Z] — YYYY-MM-DD` section was committed.
# The cycle-286 tag↔Cargo↔CHANGELOG consistency guard catches
# that race at CI time, but by then the release workflow has
# already been triggered and one platform job has failed at
# pre-flight (the Linux job ran the guard; macOS + Windows
# uploaded without it, leaving the GitHub release partial).
#
# This script does the four release ops atomically + with pre-
# flight checks, so the race can't happen:
#
#   1. Asserts working tree is clean.
#   2. Asserts CHANGELOG.md has the target [VERSION] section.
#   3. Bumps Cargo.toml's workspace `version`.
#   4. Builds once to refresh Cargo.lock.
#   5. Commits the bump + lock + (presumably already-staged)
#      CHANGELOG changes.
#   6. Annotated-tags the release commit.
#
# Push is left to the caller (so a sanity check can happen
# between local tag creation and remote push).
#
# Usage:
#
#   1. Add the new [VERSION] — YYYY-MM-DD section to CHANGELOG.md.
#      Leave it staged or committed — script accepts either.
#   2. Run: scripts/release.sh 1.7.4
#   3. Verify: git log -1, git tag -l v1.7.4
#   4. Push:   git push origin main && git push origin v1.7.4

set -euo pipefail

if [ $# -ne 1 ]; then
    cat >&2 <<EOF
usage: $0 <VERSION>

  VERSION:  semver without leading 'v', e.g. 1.7.4

Prerequisites:
  - Working tree clean (changes already committed or staged).
  - CHANGELOG.md has a '## [<VERSION>] — YYYY-MM-DD' section.
EOF
    exit 2
fi

VERSION=$1

# Strict semver match — refuse 'v1.7.4', 'release-1.7.4', etc.
if ! [[ $VERSION =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[a-zA-Z0-9.-]+)?$ ]]; then
    echo "::error::not a valid semver: $VERSION" >&2
    echo "  expected: X.Y.Z (e.g. 1.7.4), got: $VERSION" >&2
    exit 1
fi

# Pre-flight: must be in repo root (Cargo.toml + CHANGELOG.md visible).
[ -f Cargo.toml ] || { echo "::error::run from repo root (no Cargo.toml here)" >&2; exit 1; }
[ -f CHANGELOG.md ] || { echo "::error::run from repo root (no CHANGELOG.md here)" >&2; exit 1; }

# Pre-flight: working tree must be clean. The script will create
# its own commit; any uncommitted changes belong in either that
# commit's scope OR a separate prior commit — the script refuses
# to silently bundle them.
if ! git diff-index --quiet HEAD --; then
    echo "::error::working tree has uncommitted changes" >&2
    echo "  commit (or stash) your changes first, then re-run" >&2
    git status --short >&2
    exit 1
fi

# Pre-flight: CHANGELOG.md must have a section for this version.
# Mirrors the cycle-286 CI-time guard so a release.sh user gets
# the same diagnostic the CI would, but at developer-time.
if ! grep -qE "^## \[${VERSION}\] — [0-9]{4}-[0-9]{2}-[0-9]{2}\$" CHANGELOG.md; then
    echo "::error::CHANGELOG.md has no '## [${VERSION}] — YYYY-MM-DD' section" >&2
    echo "  add it before re-running. The cycle-286 CI guard would" >&2
    echo "  reject this release anyway." >&2
    exit 1
fi

# Pre-flight: refuse to re-create an existing tag locally.
if git tag -l "v${VERSION}" | grep -q "^v${VERSION}\$"; then
    echo "::error::local tag v${VERSION} already exists" >&2
    echo "  delete it first (git tag -d v${VERSION}) or pick a different version" >&2
    exit 1
fi
# Cycle 323: also check the REMOTE. Pre-fix, `git tag -l` only
# listed local tags — if a previous run pushed v1.7.X but the
# local clone hadn't fetched it, this script would silently
# proceed and the eventual `git push origin v1.7.X` would fail
# with a "remote tag already exists" error AFTER the local
# commit + tag were already made. The user'd then have to delete
# the local commit + tag manually to recover. Now: query the
# remote up-front. If `git ls-remote` fails (no network / no
# remote), warn but proceed (offline workflow is still valid).
if remote_tag=$(git ls-remote --tags origin "refs/tags/v${VERSION}" 2>/dev/null) \
    && [ -n "$remote_tag" ]; then
    echo "::error::remote tag v${VERSION} already exists on origin" >&2
    # Cycle 516: backticks inside double-quoted echo run as command
    # substitution. The original (cycle 51) line ran `git fetch && git
    # tag -d` AT ERROR TIME, mutating state and printing garbled help.
    # Use single quotes around the suggestion so the backticks are
    # literal text the user can copy-paste.
    echo '  pick a different version, or `git fetch && git tag -d v'"${VERSION}"'`' >&2
    echo "  if you need to overwrite (rarely the right move; cuts a fresh" >&2
    echo "  patch version is usually safer than retagging a published v)" >&2
    exit 1
fi

# Bump Cargo.toml workspace version. The workspace's leading
# `[workspace.package]` block has the single `version = "X.Y.Z"`
# line; per-crate Cargo.tomls inherit via `version.workspace = true`.
PREV=$(awk -F\" '/^version = "/ { print $2; exit }' Cargo.toml)
echo "bumping Cargo.toml: ${PREV} → ${VERSION}"
sed -i.bak "0,/^version = \"${PREV}\"\$/s//version = \"${VERSION}\"/" Cargo.toml
rm -f Cargo.toml.bak

# Refresh Cargo.lock so the workspace + lockfile agree. Failing
# here means a real build error — release shouldn't proceed.
#
# Cycle 311: tolerate `cargo` not being on PATH (e.g. running from
# a CI runner, cron, or a context that didn't source ~/.cargo/env).
# rustup's default install puts cargo at ~/.cargo/bin/cargo;
# Homebrew puts it at /opt/homebrew/bin/cargo or
# /usr/local/bin/cargo. Search those in order; bail with a clear
# error if none resolve. First catch: ran release.sh from a script
# context where PATH was sanitized and the cycle-307 fallout
# happened — version bumped but Cargo.lock not refreshed.
CARGO=cargo
if ! command -v "$CARGO" >/dev/null 2>&1; then
    for candidate in "$HOME/.cargo/bin/cargo" /opt/homebrew/bin/cargo /usr/local/bin/cargo; do
        if [ -x "$candidate" ]; then
            CARGO=$candidate
            break
        fi
    done
fi
if ! command -v "$CARGO" >/dev/null 2>&1 && [ ! -x "$CARGO" ]; then
    echo "::error::cargo not found on PATH or in standard rustup / Homebrew locations" >&2
    echo "  install rustup (https://rustup.rs) or set PATH to include cargo" >&2
    echo "  (Cargo.toml was already bumped to ${VERSION} — restore with: git checkout Cargo.toml)" >&2
    exit 1
fi
echo "refreshing Cargo.lock"
"$CARGO" build --workspace --quiet

# Commit.
git add Cargo.toml Cargo.lock CHANGELOG.md
git commit -m "release: v${VERSION}

See CHANGELOG.md [${VERSION}]."

# Tag.
git tag -a "v${VERSION}" -m "kettle v${VERSION}

See CHANGELOG.md [${VERSION}]."

cat <<EOF

✓ Tagged v${VERSION} locally.

Next steps:
  1. Verify the commit + tag look right:
       git log -1
       git tag -l "v${VERSION}"
       git show "v${VERSION}" | head -20

  2. Push when ready:
       git push origin main
       git push origin "v${VERSION}"

  3. Watch the release workflow (resolve the run AFTER the push
     lands — the \`run list\` is racy if you copy this command
     before pushing because it returns the previous run, not the
     one you just triggered):
       sleep 5  # let GitHub register the push-triggered run
       gh run watch \$(gh run list --workflow=release.yml --branch "v${VERSION}" --limit 1 --json databaseId --jq '.[0].databaseId') --exit-status
EOF
