# Releasing

Cutting a release takes **two pull requests and one tag**, in that order. The
scripts enforce most of it; this document exists for the parts they cannot.

## Why two pull requests

`scripts/release.sh` refuses to run until `CHANGELOG.md` already contains a
committed `## [X.Y.Z] — YYYY-MM-DD` section (note the **em dash**). `main` is
protected with `enforce_admins`, so that section cannot simply be committed
there. It needs its own pull request first.

1. **Prep** — branch `release/prep-vX.Y.Z`. Promote `## [Unreleased]` to
   `## [X.Y.Z] — YYYY-MM-DD`, leaving `[Unreleased]` in place as an empty
   placeholder. `CHANGELOG.md` only. Merge it.
2. **Cut** — branch `release/vX.Y.Z-cut` off synchronized `main`. Run
   `scripts/release.sh X.Y.Z`, which bumps the workspace version, refreshes
   `Cargo.lock`, and rewrites the version references in `README.md`,
   `docs/INSTALL.md`, `docs/VERSION-HISTORY.md` and `flake.nix`. Merge it.
3. **Tag** — from synchronized `main`, `scripts/tag-release.sh X.Y.Z`.

`release.sh` never pushes or tags `main` itself. That separation is deliberate:
its header records a tag-before-CHANGELOG race that once left a GitHub release
partially uploaded, with one platform job failing pre-flight while the others
uploaded anyway.

## The tag must be signed with a key GitHub verifies

This is the requirement most likely to stop a release, and neither script can
tell you in advance whether you satisfy it.

`.github/workflows/release.yml` gates on GitHub's own verdict:

```bash
verified=$(gh api ".../git/tags/${sha}" --jq '.verification.verified')
if [ "$verified" != true ]; then
  echo "::error::$GITHUB_REF_NAME does not have a GitHub-verified signature"
  exit 1
```

GitHub reports `verified: true` **only** for a key registered on the publishing
account as a *signing* key — not an authentication key, and not merely a key
that signs valid signatures locally. A key that is unknown to GitHub yields
`reason: unknown_key`, the gate fails, and you are left with a published tag and
no release behind it.

Two consequences worth knowing before you start:

- **Not every machine that can commit can cut a tag.** Release *commits* may be
  signed with any key, or none; branch protection sets `signatures = false`, so
  they show "Unverified" on GitHub without consequence. Only the **tag** is
  gated.
- **`tag-release.sh` runs `git verify-tag` before it pushes**, and that needs
  `gpg.ssh.allowedSignersFile` configured locally. Without it the script aborts
  under `set -e` after creating a local tag but before pushing — a safe failure,
  but it leaves a stray local tag that blocks a re-run until deleted.

### Check before you cut

```bash
# 1. Which key will sign?
git config --get user.signingkey
ssh-keygen -lf "$(git config --get user.signingkey)"

# 2. Can this machine verify a signature at all?
git config --get gpg.ssh.allowedSignersFile   # must be set and exist

# 3. Does GitHub trust that key? Ask it about the previous release tag,
#    which was signed with the key you intend to reuse.
gh api "repos/<owner>/<repo>/git/tags/$(git rev-parse vPREVIOUS)" \
  --jq '.verification | {verified, reason}'
```

If step 3 does not report `verified: true, reason: valid`, cut the tag from a
machine whose key is registered, or register the key first: GitHub → Settings →
SSH and GPG keys → New SSH key → **type: Signing Key**.

## After the tag

`release.yml` builds on Linux, macOS and Windows, signs the update manifest with
the `release-signing` environment secret, verifies the draft, and publishes. Do
not trust the green check alone — verify the published artifacts:

```bash
gh release download vX.Y.Z --dir /tmp/verify
cd /tmp/verify
for s in *.sha256; do shasum -a 256 -c "$s"; done

# The manifest signature covers a domain-separated payload, not the bare file:
printf 'kettle-update-manifest-v1\0' > /tmp/payload
cat kettle-update-manifest.json >> /tmp/payload
base64 -d < kettle-update-manifest.json.sig > /tmp/sig
openssl pkeyutl -verify -rawin -pubin -inkey packaging/update-public.pem \
  -in /tmp/payload -sigfile /tmp/sig
```

That `kettle-update-manifest-v1\0` prefix is deliberate domain separation — it
stops a valid signature being replayed against a different document type. Verify
against the bare manifest and it will fail, which looks alarming and is not.

## Known gaps

- **macOS artifacts are unsigned and unnotarized.** Users see Gatekeeper
  warnings. The work exists as a draft pull request and is blocked on Apple
  Developer Program credentials; see `docs/ROADMAP.md`.
- `scripts/verify-release-assets.py` only runs against a **draft** release, by
  design — it is part of `release.yml` before publication and cannot be re-run
  afterwards.
