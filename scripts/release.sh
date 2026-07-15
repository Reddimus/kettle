#!/usr/bin/env bash
# scripts/release.sh — prepare a protected-main release pull request.
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
#   5. Signs and commits the bump + lock + already-committed CHANGELOG section.
#   6. Leaves the release commit on the current branch for a pull request.
#
# `main` is intentionally never pushed or tagged by this script. Merge the
# generated commit through required CI, then run `scripts/tag-release.sh` from
# the synchronized main branch.
#
# Usage:
#
#   1. Add and commit the new [VERSION] — YYYY-MM-DD section to CHANGELOG.md.
#   2. Run: scripts/release.sh 1.7.4
#   3. Push the branch and merge its pull request after CI.
#   4. On synchronized main: scripts/tag-release.sh 1.7.4
#
# Release commits and `tag-release.sh` require a configured GPG or SSH signing
# identity. The commit/tag email and public signing key must both be associated
# with the GitHub account that publishes the release, or GitHub reports an
# otherwise valid local signature as unverified:
#
#   git config gpg.format ssh
#   git config user.signingkey ~/.ssh/id_ed25519.pub

set -euo pipefail

if [ $# -ne 1 ]; then
    cat >&2 <<EOF
usage: $0 <VERSION>

  VERSION:  semver without leading 'v', e.g. 1.7.4

Prerequisites:
  - Working tree clean (all changes already committed).
  - CHANGELOG.md has a '## [<VERSION>] — YYYY-MM-DD' section.
EOF
    exit 2
fi

VERSION=$1

# Strict semver match — refuse 'v1.7.4', 'release-1.7.4', etc.
if ! [[ $VERSION =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
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
BRANCH=$(git branch --show-current)
if [ -z "$BRANCH" ] || [ "$BRANCH" = main ]; then
    echo "::error::prepare releases on a topic branch, not main or detached HEAD" >&2
    exit 1
fi
if [ -n "$(git status --porcelain=v1 --untracked-files=normal)" ]; then
    echo "::error::working tree has uncommitted changes" >&2
    echo "  commit (or stash) your changes first, then re-run" >&2
    git status --short >&2
    exit 1
fi

# Every path the script may mutate. The clean-tree precondition makes it safe
# to restore these files if any command fails before the release commit lands.
RESTORE_FILES=(
    Cargo.toml Cargo.lock CHANGELOG.md flake.nix
    README.md docs/INSTALL.md docs/VERSION-HISTORY.md
)
MUTATIONS_STARTED=0
cleanup_release_attempt() {
    status=$?
    trap - EXIT
    if [ "$status" -ne 0 ] && [ "$MUTATIONS_STARTED" -eq 1 ]; then
        existing=()
        for path in "${RESTORE_FILES[@]}"; do
            [ -e "$path" ] && existing+=("$path")
        done
        if [ "${#existing[@]}" -gt 0 ]; then
            git restore --source=HEAD --staged --worktree -- "${existing[@]}" 2>/dev/null || true
        fi
        echo "release preparation failed; script-owned changes were restored" >&2
    fi
    exit "$status"
}
trap cleanup_release_attempt EXIT

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
# Escape BRE metacharacters before splicing PREV into the sed *address*
# patterns below. A plain `X.Y.Z` semver works either way (the dots only
# ever match their literal selves in Cargo.toml), but a pre-release tag
# like `1.0.0-rc.1+build` carries chars BRE would misinterpret — escaping
# keeps the match exact regardless of the version shape.
PREV_RE=$(printf '%s' "${PREV}" | sed 's/[.[\*^$/]/\\&/g')
echo "bumping Cargo.toml: ${PREV} → ${VERSION}"
MUTATIONS_STARTED=1
sed -i.bak "0,/^version = \"${PREV_RE}\"\$/s//version = \"${VERSION}\"/" Cargo.toml
rm -f Cargo.toml.bak

# Cycle 746 — durable lockstep for the inter-crate path-dep version
# requirements in `[workspace.dependencies]`. They were pinned at a fixed
# 1.x floor (`version = "1.45.1"`), which `^`-excludes a 2.0.0 MAJOR bump
# and broke `release.sh 2.0.0` at the Cargo.lock refresh ("failed to select
# a version for `kettle-vt = ^1.45.1` … candidate 2.0.0 didn't match").
# Keeping each pin equal to the release version means every future bump —
# including majors — resolves cleanly. The crates are never published to
# crates.io (no `publish`/badge), so the version is only a resolver hint.
echo "bumping inter-crate version pins → ${VERSION}"
sed -i.bak -E "s|(path = \"crates/kettle-[a-z]+\", version = \")[^\"]*|\1${VERSION}|" Cargo.toml
rm -f Cargo.toml.bak

# Cycle 550 — durable lockstep with flake.nix. The Nix-side
# version had drifted 39 releases (v1.3.5 → v1.42.0 at cycle 549)
# because the file's "Keep in lockstep" comment was advisory-
# only. Now the release script bumps it in the same atomic step
# as Cargo.toml. The flake-nix-version-line shape:
#
#     version = "1.42.0";
#
# (4 leading spaces + version + ; + maybe a trailing comment).
# Use the same 0,/pattern/ form so we only touch the first match
# (the package version, not any cargo-vendor-deps version etc.).
if [ -f flake.nix ]; then
    echo "bumping flake.nix:  ${PREV} → ${VERSION}"
    sed -i.bak "0,/^          version = \"${PREV_RE}\";\$/s//          version = \"${VERSION}\";/" flake.nix
    rm -f flake.nix.bak
fi
# Cycle 790 — durable lockstep for the user-facing install docs. README.md's
# status banner and docs/INSTALL.md's "current latest" line + example
# `KETTLE_VERSION=` / download URLs spell the version as `vX.Y.Z`, and kept
# re-staling because nothing synced them to Cargo.toml (the manual cycle-783
# bump went stale within two days — the audit's finding E2). Bump every
# `vPREV` → `vVERSION` occurrence here, atomically with the Cargo/flake bumps.
# These files only ever write `vX.Y.Z` as a release reference, so a global
# replace is safe.
for doc in README.md docs/INSTALL.md docs/VERSION-HISTORY.md; do
    if [ -f "$doc" ] && grep -q "v${PREV}" "$doc"; then
        echo "bumping ${doc}: v${PREV} → v${VERSION}"
        sed -i.bak "s/v${PREV_RE}/v${VERSION}/g" "$doc"
        rm -f "${doc}.bak"
    fi
done

# v2.34.1 (audit) — durable fix for the exact drift the cycle-790 loop above
# cannot catch. That loop only rewrites `v${PREV}`; a doc whose release-reference
# version missed a bump is never healed. (docs/INSTALL.md's "current latest" +
# download URLs and README.md's `KETTLE_VERSION=` example had stranded at
# v2.31.0 while the workspace was already v2.34.0 — three releases back — because
# `grep -q v2.34.0` never matched them.) Rewrite the well-defined release-
# reference patterns keyed to the version being RELEASED, not PREV, so they land
# on the current release no matter how far they drifted. Bounded, unambiguous
# anchors — the "current latest" claim, GitHub release-download URLs, and the
# `KETTLE_VERSION=` pin example. Historical / feature-era refs (e.g. "Every
# release from v1.3.4 onward") never match these anchors, so they stay put.
for doc in README.md docs/INSTALL.md; do
    [ -f "$doc" ] || continue
    sed -i.bak -E \
        -e "s/current latest: v[0-9]+\.[0-9]+\.[0-9]+/current latest: v${VERSION}/g" \
        -e "s#releases/download/v[0-9]+\.[0-9]+\.[0-9]+/#releases/download/v${VERSION}/#g" \
        -e "s/KETTLE_VERSION=v[0-9]+\.[0-9]+\.[0-9]+/KETTLE_VERSION=v${VERSION}/g" \
        "$doc"
    rm -f "${doc}.bak"
    if ! git diff --quiet -- "$doc"; then
        echo "bumping ${doc}: release-reference version strings → v${VERSION}"
    fi
done
if [ -f docs/VERSION-HISTORY.md ] && grep -q "Current workspace version: \`${PREV}\`" docs/VERSION-HISTORY.md; then
    echo "bumping docs/VERSION-HISTORY.md workspace version: ${PREV} → ${VERSION}"
    sed -i.bak "s/Current workspace version: \`${PREV_RE}\`/Current workspace version: \`${VERSION}\`/" docs/VERSION-HISTORY.md
    rm -f docs/VERSION-HISTORY.md.bak
fi
if [ -f docs/VERSION-HISTORY.md ]; then
    RELEASE_DATE=$(awk -v ver="$VERSION" '$0 ~ "^## \\[" ver "\\] — " { print $4; exit }' CHANGELOG.md)
    TAG_COUNT=$(git tag -l 'v[0-9]*' | wc -l | tr -d '[:space:]')
    NEXT_TAG_COUNT=$((TAG_COUNT + 1))
    echo "refreshing docs/VERSION-HISTORY.md release count/date"
    sed -i.bak -E \
        "s/Release records inspected: [0-9]+ Git tags and [0-9]+ changelog headings/Release records inspected: ${NEXT_TAG_COUNT} Git tags and ${NEXT_TAG_COUNT} changelog headings/" \
        docs/VERSION-HISTORY.md
    rm -f docs/VERSION-HISTORY.md.bak
    if [ -n "$RELEASE_DATE" ]; then
        sed -i.bak -E \
            "s/(\`v2\.29\.0\` to \`v${VERSION}\` \(2026-06-19 to )[0-9]{4}-[0-9]{2}-[0-9]{2}\)/\1${RELEASE_DATE})/" \
            docs/VERSION-HISTORY.md
        rm -f docs/VERSION-HISTORY.md.bak
    fi
fi

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
# Cycle 765: roll back the version bumps if the build fails. The bump touched
# Cargo.toml (+ inter-crate pins), flake.nix, and possibly Cargo.lock BEFORE
# this build; under `set -e` a build failure would otherwise exit with those
# files dirty and no commit, leaving the maintainer to clean up by hand.
# Restore them so a failed release attempt leaves the tree exactly as it was.
if ! "$CARGO" build --workspace --quiet; then
    echo "::error::cargo build failed" >&2
    echo "  fix the build error and re-run" >&2
    exit 1
fi

# Commit.
# Cycle 550: include flake.nix in the release commit since the
# Nix-side version is now auto-bumped in lockstep above.
# Cycle 589: gate the `git add flake.nix` on the file's existence
# to match the cycle-550 sed-bump guard above. The previous
# comment claimed the add was a no-op when the file was absent,
# but `git add <missing>` exits with code 128 — under `set -e`
# the whole release would abort *after* the Cargo.toml + lock
# bump had already been applied to the working tree, leaving
# the user with a dirty state to clean up. Conditional add
# matches the conditional bump.
ADD_FILES=(Cargo.toml Cargo.lock CHANGELOG.md)
if [ -f flake.nix ]; then
    ADD_FILES+=(flake.nix)
fi
# Cycle 790: stage the install docs whose version strings were bumped above
# (only if the bump actually changed them, so a clean tree stays clean).
for doc in README.md docs/INSTALL.md docs/VERSION-HISTORY.md; do
    if [ -f "$doc" ] && ! git diff --quiet -- "$doc"; then
        ADD_FILES+=("$doc")
    fi
done
git add "${ADD_FILES[@]}"
git commit -S -m "release: v${VERSION}

See CHANGELOG.md [${VERSION}]."
MUTATIONS_STARTED=0
trap - EXIT

cat <<EOF

Prepared the v${VERSION} release commit on $(git branch --show-current).

Next steps:
  1. Verify the commit:
       git log -1

  2. Push this branch, open a pull request, and merge only after required CI.

  3. Synchronize local main, then create the verified release tag:
       scripts/tag-release.sh ${VERSION}
EOF
