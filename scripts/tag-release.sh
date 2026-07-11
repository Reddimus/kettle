#!/usr/bin/env bash
# Create and push a signed release tag only from synchronized protected main.

set -euo pipefail

if [[ $# -ne 1 || ! $1 =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "usage: $0 X.Y.Z" >&2
  exit 2
fi
version=$1
tag="v${version}"

[[ $(git branch --show-current) == main ]] || {
  echo "::error::release tags may only be cut from main" >&2
  exit 1
}
[[ -z $(git status --porcelain=v1 --untracked-files=normal) ]] || {
  echo "::error::working tree is not clean" >&2
  exit 1
}
git fetch origin main --tags
[[ $(git rev-parse HEAD) == $(git rev-parse origin/main) ]] || {
  echo "::error::local main is not synchronized with origin/main" >&2
  exit 1
}
[[ $(awk -F\" '/^version = / { print $2; exit }' Cargo.toml) == "$version" ]] || {
  echo "::error::Cargo.toml does not declare ${version}" >&2
  exit 1
}
grep -qE "^## \[${version}\] — [0-9]{4}-[0-9]{2}-[0-9]{2}$" CHANGELOG.md || {
  echo "::error::CHANGELOG.md has no dated ${version} section" >&2
  exit 1
}
if git ls-remote --exit-code --tags origin "refs/tags/${tag}" >/dev/null 2>&1; then
  echo "::error::${tag} already exists" >&2
  exit 1
fi

if git show-ref --verify --quiet "refs/tags/${tag}"; then
  [[ $(git cat-file -t "$tag") == tag ]] || {
    echo "::error::existing local ${tag} is not annotated" >&2
    exit 1
  }
  [[ $(git rev-parse "${tag}^{commit}") == $(git rev-parse HEAD) ]] || {
    echo "::error::existing local ${tag} does not point to HEAD" >&2
    exit 1
  }
  git verify-tag "$tag"
  echo "Reusing verified local ${tag} from an earlier failed push."
else
  git tag -s "$tag" -m "kettle ${tag}

See CHANGELOG.md [${version}]."
  git verify-tag "$tag"
fi
git push origin "$tag"

echo "Pushed verified release tag ${tag}."
