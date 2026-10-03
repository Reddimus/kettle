#!/usr/bin/env bash
# scripts/release.sh — prepare a protected-main release pull request.
#
# A tag pushed before the CHANGELOG.md `[X.Y.Z] — YYYY-MM-DD` section is
# committed starts the release workflow, which then fails at the
# tag/Cargo.toml/CHANGELOG consistency guard. This script closes that race by
# running the release steps atomically, with pre-flight checks:
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
    docs/INSTALL.md docs/VERSION-HISTORY.md
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
# Mirrors the tag/Cargo.toml/CHANGELOG consistency guard so a release.sh user gets
# the same diagnostic the CI would, but at developer-time.
if ! grep -qE "^## \[${VERSION}\] — [0-9]{4}-[0-9]{2}-[0-9]{2}\$" CHANGELOG.md; then
    echo "::error::CHANGELOG.md has no '## [${VERSION}] — YYYY-MM-DD' section" >&2
    echo "  add it before re-running. The tag/Cargo.toml/CHANGELOG consistency guard would" >&2
    echo "  reject this release anyway." >&2
    exit 1
fi

# Pre-flight: refuse to re-create an existing tag locally.
if git tag -l "v${VERSION}" | grep -q "^v${VERSION}\$"; then
    echo "::error::local tag v${VERSION} already exists" >&2
    echo "  delete it first (git tag -d v${VERSION}) or pick a different version" >&2
    exit 1
fi
# Also check the remote, because `git tag -l` misses tags this clone has not
# fetched. If `git ls-remote` fails (no network or no remote), skip the check
# so offline preparation still works.
if remote_tag=$(git ls-remote --tags origin "refs/tags/v${VERSION}" 2>/dev/null) \
    && [ -n "$remote_tag" ]; then
    echo "::error::remote tag v${VERSION} already exists on origin" >&2
    # Single quotes keep the backticks literal so the user can copy the command.
    # Inside double quotes they would run `git fetch && git tag -d` as command
    # substitution at error time.
    echo '  pick a different version, or `git fetch && git tag -d v'"${VERSION}"'`' >&2
    echo "  if you need to overwrite (rarely the right move; cuts a fresh" >&2
    echo "  patch version is usually safer than retagging a published v)" >&2
    exit 1
fi

# Replace the one line equal to $2 in file $1 with $3. Missing and duplicate
# anchors are both errors: either means the owning file changed shape and a
# release must not guess which text is current.
#
# GNU sed's `0,/re/s//repl/` is not portable. BSD sed (macOS /usr/bin/sed)
# accepts the `0,/re/` range but silently matches nothing and exits 0, and it
# rejects `s//repl/` with "first RE may not be empty". A silent miss leaves
# the workspace version behind the inter-crate pins and fails the Cargo.lock
# refresh. awk with an exact string comparison is portable, needs no regex
# escaping, and can enforce the match count on both BSD and GNU userlands.
replace_exact_line() {
    local file="$1" want="$2" repl="$3"
    awk -v want="${want}" -v repl="${repl}" '
        $0 == want { print repl; matches += 1; next }
        { print }
        END { exit(matches == 1 ? 0 : 1) }
    ' "${file}" >"${file}.tmp" || {
        rm -f "${file}.tmp"
        echo "::error::${file} expected exactly one matching line: ${want}" >&2
        exit 1
    }
    # Remove the temporary file on a failed rename too. `release.sh` refuses to
    # start on a dirty tree, so a stranded `.tmp` would block the next run with
    # a confusing diagnostic about uncommitted changes rather than the real
    # cause.
    mv "${file}.tmp" "${file}" || {
        rm -f "${file}.tmp"
        echo "::error::could not replace ${file}" >&2
        exit 1
    }
}

# Replace exactly one complete line selected by a POSIX extended regular
# expression. This is reserved for generated history values whose previous
# counts are not known until release time. As above, drift fails closed.
replace_matching_line() {
    local file="$1" pattern="$2" repl="$3"
    awk -v pattern="${pattern}" -v repl="${repl}" '
        $0 ~ pattern { print repl; matches += 1; next }
        { print }
        END { exit(matches == 1 ? 0 : 1) }
    ' "${file}" >"${file}.tmp" || {
        rm -f "${file}.tmp"
        echo "::error::${file} expected exactly one matching line for: ${pattern}" >&2
        exit 1
    }
    mv "${file}.tmp" "${file}" || {
        rm -f "${file}.tmp"
        echo "::error::could not replace ${file}" >&2
        exit 1
    }
}

# Bump Cargo.toml workspace version. The workspace's leading
# `[workspace.package]` block has the single `version = "X.Y.Z"`
# line; per-crate Cargo.tomls inherit via `version.workspace = true`.
PREV=$(awk -F\" '/^version = "/ { print $2; exit }' Cargo.toml)
echo "bumping Cargo.toml: ${PREV} → ${VERSION}"
MUTATIONS_STARTED=1
replace_exact_line Cargo.toml "version = \"${PREV}\"" "version = \"${VERSION}\""

# Keep the inter-crate path-dep version requirements in
# `[workspace.dependencies]` equal to the release version. A fixed floor such
# as `1.45.1` `^`-excludes the next major version and fails the Cargo.lock
# refresh. The crates are never published to crates.io, so the version is only
# a resolver hint.
echo "bumping inter-crate version pins → ${VERSION}"
# The character class admits `-` for `kettle-test-support` and digits for
# `kettle-i18n`.
sed -i.bak -E "s|(path = \"crates/kettle-[a-z0-9-]+\", version = \")[^\"]*|\1${VERSION}|" Cargo.toml
rm -f Cargo.toml.bak

# Fail loudly if any inter-crate pin did not reach ${VERSION}. A silent miss
# here surfaces much later as an opaque cargo resolver error.
if grep -E 'path = "crates/kettle[^"]*", version = "' Cargo.toml \
    | grep -v "version = \"${VERSION}\"" >/dev/null; then
    echo "::error::inter-crate version pins were not all bumped to ${VERSION}:" >&2
    grep -E 'path = "crates/kettle[^"]*", version = "' Cargo.toml \
        | grep -v "version = \"${VERSION}\"" >&2
    exit 1
fi

# Bump flake.nix in the same step so the Nix package version cannot drift from
# Cargo.toml. The package line is `version = "X.Y.Z";` with 10 leading spaces,
# and `replace_exact_line` matches that whole line, so nothing else changes.
if [ -f flake.nix ]; then
    echo "bumping flake.nix:  ${PREV} → ${VERSION}"
    replace_exact_line flake.nix \
        "          version = \"${PREV}\";" \
        "          version = \"${VERSION}\";"
fi

# Keep only the live install examples in lockstep. README.md and the other
# versioned references in these documents are historical records, so every
# replacement is a complete, unique line keyed to the previous workspace
# version. A stale or duplicated anchor aborts instead of broadening the edit.
echo "bumping docs/INSTALL.md current release references: v${PREV} → v${VERSION}"
replace_exact_line docs/INSTALL.md \
    "  | KETTLE_VERSION=v${PREV} sh" \
    "  | KETTLE_VERSION=v${VERSION} sh"
replace_exact_line docs/INSTALL.md \
    "Every release from **v1.3.4** onward ships a \`.sha256\` sidecar (current latest: v${PREV})" \
    "Every release from **v1.3.4** onward ships a \`.sha256\` sidecar (current latest: v${VERSION})"
replace_exact_line docs/INSTALL.md \
    "curl -fLO https://github.com/Reddimus/kettle/releases/download/v${PREV}/kettle-linux-x86_64.tar.gz" \
    "curl -fLO https://github.com/Reddimus/kettle/releases/download/v${VERSION}/kettle-linux-x86_64.tar.gz"
replace_exact_line docs/INSTALL.md \
    "curl -fLO https://github.com/Reddimus/kettle/releases/download/v${PREV}/kettle-linux-x86_64.tar.gz.sha256" \
    "curl -fLO https://github.com/Reddimus/kettle/releases/download/v${VERSION}/kettle-linux-x86_64.tar.gz.sha256"

echo "refreshing docs/VERSION-HISTORY.md current baseline"
replace_exact_line docs/VERSION-HISTORY.md \
    "- Latest version in this tree: \`v${PREV}\`, with matching source version," \
    "- Latest version in this tree: \`v${VERSION}\`, with matching source version,"
replace_exact_line docs/VERSION-HISTORY.md \
    "- Current workspace version: \`${PREV}\`" \
    "- Current workspace version: \`${VERSION}\`"

# release.sh runs after the new changelog heading is committed but before its
# tag exists, so the heading count leads the real tag count by one.
TAG_COUNT=$(git tag -l 'v[0-9]*' | wc -l | tr -d '[:space:]')
DATED_HEADING_COUNT=$((TAG_COUNT + 1))
TOTAL_HEADING_COUNT=$((DATED_HEADING_COUNT + 1))
replace_matching_line docs/VERSION-HISTORY.md \
    '^- Release headings inspected: [0-9][0-9]* across the root `CHANGELOG.md` and$' \
    "- Release headings inspected: ${TOTAL_HEADING_COUNT} across the root \`CHANGELOG.md\` and"
replace_matching_line docs/VERSION-HISTORY.md \
    '^  `docs/changelog/` archives[.] That count comprises `[[]Unreleased[]]` and [0-9][0-9]* dated$' \
    "  \`docs/changelog/\` archives. That count comprises \`[Unreleased]\` and ${DATED_HEADING_COUNT} dated"
replace_matching_line docs/VERSION-HISTORY.md \
    '^  versions from `v0[.]1[.]0` through `v[0-9][0-9]*[.][0-9][0-9]*[.][0-9][0-9]*`[.]$' \
    "  versions from \`v0.1.0\` through \`v${VERSION}\`."

# Refresh Cargo.lock so the workspace + lockfile agree. Failing
# here means a real build error — release shouldn't proceed.
#
# Tolerate `cargo` not being on PATH (e.g. running from
# a CI runner, cron, or a context that didn't source ~/.cargo/env).
# rustup's default install puts cargo at ~/.cargo/bin/cargo;
# Homebrew puts it at /opt/homebrew/bin/cargo or
# /usr/local/bin/cargo. Search those in order; bail with a clear
# error if none resolve.
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
# If the build fails, the EXIT trap restores every file in RESTORE_FILES, so a
# failed release attempt leaves the tree exactly as it was.
if ! "$CARGO" build --workspace --quiet; then
    echo "::error::cargo build failed" >&2
    echo "  fix the build error and re-run" >&2
    exit 1
fi

# Commit. Add flake.nix only when it exists, matching the conditional bump
# above. `git add` of a missing path exits 128, which aborts the release under
# `set -e`.
ADD_FILES=(Cargo.toml Cargo.lock CHANGELOG.md)
if [ -f flake.nix ]; then
    ADD_FILES+=(flake.nix)
fi
# Stage the release docs whose current-version anchors were bumped above.
for doc in docs/INSTALL.md docs/VERSION-HISTORY.md; do
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
