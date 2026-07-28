# Updating Kettle

## Commands

```text
kettle --check-update     Check the authenticated stable feed without installing
kettle update             Check, prompt, download, verify, and install
kettle update --yes       Confirm non-interactively for scripts
kettle --update           Convenience alias for interactive `kettle update`
```

On Windows, use the bare `kettle` command after installation. The installed
`kettle.com` console launcher makes PowerShell/cmd wait for prompts and return
the updater's exit code; the Start Menu points directly at no-console
`kettle.exe`.

There is intentionally no `-u` shorthand: short flags are easy to trigger by
mistake and are difficult to reserve permanently. Updates never restart running
windows. Linux commits the replacement immediately and existing processes keep
their mapped image. Windows stages the verified release and applies it from a
helper only after every Kettle process has exited.

> **Windows bootstrap for v2.35:** releases before v2.35 did not include the
> out-of-process helper/run-lock protocol and could not reliably replace a
> mapped `kettle.exe`. Install v2.35 once with the bundled `install.ps1`; later
> releases can use `kettle update` without that manual bootstrap.

## Supported self-update layouts

| Running Kettle | Self-update behavior |
|---|---|
| Windows 11 x86_64 installed by the bundled `install.ps1` | Supported |
| The same Windows executable launched from WSL | Supported through Windows interop |
| Ubuntu/Linux x86_64 installed by the bundled `install.sh` or online installer | Supported |
| Ubuntu/Linux aarch64 installed by the bundled installer | Supported |
| Local source build (`local-dev` marker) | Refused; rebuild and reinstall from that checkout |
| Cargo, distro package, Nix, a future Homebrew/AUR package, or manually copied binary | Refused; update with its owner |
| macOS app | Use the release page; the Homebrew tap is not published and in-app replacement is not yet supported |

Official installers write a small ownership marker beside the managed layout.
The updater derives the install prefix from the running executable and requires
that marker to match Kettle, the stable channel, and the current Rust target.
It never searches `PATH` for another copy and never elevates privileges.
Repository installs deliberately use a `local-dev` marker (recording no longer
affects the channel — it is a runtime toggle in every build; the legacy
`local-dev-record` marker is still recognized and refused for older installs).
Refusing those channels prevents the stable updater from replacing a
source-built binary or rewriting its launcher. Only an extracted release
tarball or the online installer writes a `stable` marker.

On Windows, the updater writes a schema-2 bounded pending record inside the
installer-owned prefix containing the target version, the copied helper's
size/SHA-256, and the size/SHA-256 of every extracted file, then starts that
uniquely named helper. Verification retains root-down directory and file handles
for the helper, stage root, optional shell directory, and every staged leaf
through helper launch or transaction consumption, so a rename cannot substitute
an object after validation. The transaction id is exactly a canonical decimal
staging-process `u32` PID, a hyphen, and canonical decimal Unix-epoch
nanoseconds bounded to `u128`; a valid transaction's stage, helper, backup
marker, journal, pending record, and quarantine evidence must agree on that id
(unparseable pending evidence receives a fresh bounded quarantine id).
The saved PowerShell uninstaller understands this same schema-2 record and
validates its exact field set, scalar types, counters, hashes, managed relative
paths, and aggregate byte limit. It does not require a named helper or stage to
remain present after controlled removal or a crash; any extant object still
passes the independent managed-tree checks before it can be deleted.
The install prefix must be a dedicated directory named `kettle`; the installer
rejects shared or broad prefixes and validates the managed tree and transaction
ACL before recovery or mutation. Every managed Kettle process
holds a shared run lock; the helper takes it exclusively, also waits until the
installed `.exe` and `.com` are no longer mapped, re-verifies the staged files,
and then commits the transaction. The helper acquires the update lock before
the running lock, releases the running lock and then the update lock after the
commit and pending-record removal are durable, and only then launches the saved
installer's integration refresh. A launch that sees pending state starts the
helper and exits rather than prolonging the old version. Failed attempts retain
the pending record, staged files, and a bounded error message for the next
launch to retry. Automatic retry stops after three failed helper attempts. An
actual transaction attempt resets the independent handoff-timeout counter.
Timeouts waiting for still-running Kettle processes do not consume transaction
attempts during the first five minutes; after that grace period, three timed-out
handoffs stop automatic retry instead of allowing every future launch to hang.
An invalid or exhausted regular pending record is atomically quarantined as
`.kettle-update-failed-*.json`; Kettle also writes a bounded `.txt` diagnostic
when the prefix permits it. Quarantine itself is best-effort so a read-only
prefix or antivirus sharing denial cannot block the intact old binary from
starting. A nonregular pending path is never renamed into trusted evidence.
Startup emits a stderr message and attempts a desktop recovery notification;
notification failure is logged. Evidence that could be preserved remains in
the install prefix for diagnosis instead of trapping every future launch in a
handoff loop. At most eight failed transaction pairs are retained; older exact
records are pruned by transaction timestamp and then PID while the update lock
is held.

## Automatic policy

The default is `auto`: Kettle keeps itself current in the background,
oh-my-zsh style. It checks at most once per day (tunable), and when a newer
signed release is found it installs it — applied on Linux immediately (running
windows keep the old mapped image until they exit) and staged on Windows until
every Kettle window closes, then used on the next launch. The first launch only
creates the throttle state and performs no network request; the first automatic
install shows a one-time notification explaining how to opt out.

```ini
update-policy = auto      # default: install in the background, use after next restart
update-policy = notify    # only show a passive new-version banner
update-policy = off       # no automatic network request

# How often the background check may contact the feed, in hours (default 24 =
# daily; floored at 1). `update-policy = off` disables checking regardless.
update-check-interval-hours = 24
```

A window left open for many days is re-checked on an hourly in-session timer
(still bounded by the configured interval), so it stays current without a
restart. The legacy `update-check = true|false` setting remains compatible and
maps to `notify|off`. If both settings exist, `update-policy` wins regardless of
order. Builds produced with `KETTLE_PACKAGED` disable automatic checks so a
downstream distribution can own update policy. Explicit CLI checks still work.

## Verification and recovery

Each stable release publishes `kettle-update-manifest.json` and a detached
Ed25519 signature. Kettle embeds only the dedicated public key. Before parsing
or using metadata it verifies the signature over a domain-separated payload.
The signed manifest binds each supported target to an exact archive name, byte
length, and SHA-256 digest.

Downloads are capped at 256 MiB. On Linux, the downloader reserves one buffer
from the signed artifact size, rejects allocation failure explicitly, hashes
that exact buffer, and extracts through a `Cursor` over the same bytes. No
writable archive inode exists between verification and extraction. Windows
uses one exclusively range-locked temporary-file handle for download,
verification, and extraction. Extraction accepts at most 128 entries and 512
MiB of actual output. Absolute paths, traversal, duplicates and case aliases,
file/directory prefix conflicts, encrypted entries, links, special/sparse
files, Windows device names, and declared/actual size mismatches are rejected.
Updates acquire an install-prefix lock and atomically replace each destination
on its own filesystem.

Linux and Windows update archives from v2.36 onward contain
`kettle-package-manifest.json`. Kettle binds its product/version/target and
every extracted regular file to an exact relative path, size, SHA-256, and Unix
mode where applicable before staging is accepted. Release CI generates this
inner manifest before packaging, then extracts the final downloaded artifact
and verifies it again with `scripts/package-manifest.py` before signing or
publication. The macOS `.app` is not installed by Kettle's self-updater and does
not use this inner manifest.

The verifier remains backward-compatible with release archives before v2.36
that do not contain this inner package manifest; the signed feed's archive size
and SHA-256 remain mandatory in either case.

The schema-2 transaction journal records a transaction id, target version,
durable phase (`prepared`, `applying`, `rolling_back`, or `committed`), and each
destination's previous/replacement size and SHA-256. Recovery verifies backup
integrity, an id-bound marker, and the exact absence of unjournaled backup files
or directories, then checkpoints every restored entry, so a second interruption
simply resumes rollback. A durably committed journal is cleaned without
reverting the new files. The journal is deleted and its parent synced before
exact leaf-by-leaf backup cleanup, so recovery never points at data it already
removed. A crash after that durable boundary can leave only a bounded,
marker-bound orphan backup; startup removes it under the update lock and refuses
unknown names, reparse points, or payload paths. Schema-1 journals left by v2.34
remain recoverable in the corrected order, but cleanup still requires their
exact journaled tree and never removes an unjournaled sentinel. Before the first
destination mutation, both new transactions and recovery preflight the aggregate
backup set against the 512 MiB limit. Backup copies stream through a bounded
64 KiB buffer rather than allocating each destination in memory.
Replacement bytes, the final preserved security descriptor, and the final Unix
mode are applied and synced on the staged inode before its atomic publication.
An interrupted executable update therefore cannot expose the staging mode
`0600` at the installed path while its journal still says `prepared`.
Linux keeps the descriptor-relative parent alive until the destination snapshot
file is open, so an existing install leaf cannot be mistaken for a new file
through a dangling `/proc/self/fd` path. Exact same-destination atomic-write
temps from a definitively dead creator are reclaimed with owner, type,
single-link, and identity checks; bounded cleanup ignores live or malformed
lookalikes.

The Windows installer uses the same update-then-running lock order and retains a
no-follow handle chain from the drive root through the install prefix while it
validates or mutates managed state. Lock files use the sharing and whole-file
byte range expected by Kettle's Rust lock implementation. The accepted tree is
limited to 128 entries and 512 MiB, with exact release-root and
`shell-integration/kettle.{bash,fish,ps1,zsh}` names. For upgrade compatibility,
historical binary-backup forms are recognized narrowly:
`kettle.{exe,com}.bak-MAJOR.MINOR.PATCH-YYYYMMDD`,
`kettle.{exe,com}.bak-YYYY-MM-DD`, and
`kettle.{exe,com}.bak-N-N`. They are preserved by an upgrade and removed as
ordinary leaves by uninstall; arbitrary `.bak-*` names remain unmanaged and
fail closed.

Release CI accepts only a GitHub-verified annotated tag. All required platform
artifacts must finish before the signing job can access its environment secret.
The release jobs use pinned runner images and an exact Rust toolchain, and every
Cargo invocation is lock-file constrained. The protected signer has read-only
repository permission: it reopens each archive through the bounded
package-manifest extractor, signs the feed, and passes only the exact finalized
asset set across an Actions-artifact boundary. A separate publisher has
repository write permission but no signing secret. It re-verifies the
signature, canonical sidecars, archives, and package metadata before uploading
a draft. The GitHub API response must then identify the exact draft tag and
contain one uploaded asset per expected name with the exact local byte length
and streamed SHA-256 before the release becomes public. This prevents a
partial or substituted update feed without exposing the signing key and
publication credential to the same job.

The macOS universal archive is deliberately outside the signed self-update
manifest because Kettle does not replace `.app` bundles in place. Its release
assurance boundary is therefore distinct: the publisher requires the exact
staged archive and canonical SHA-256 sidecar, regenerates the Homebrew formula
from those bytes, and verifies the uploaded draft's reported size and streamed
digest before publication. This does not claim that the macOS archive is bound
by the updater's Ed25519 signature; adding macOS to that schema would be a
future defense-in-depth improvement.

## Signing-key rotation

The stable manifest currently has one active Ed25519 trust root and no key
identifier. That makes an unannounced key replacement intentionally fail closed
in every installed client. Do not rotate the release-environment secret by
itself: the finalizer compares its derived public key with
`packaging/update-public.pem` and will reject the release.

A planned rotation must use a bridge release:

1. Generate the successor key under the release key-custody process; never add
   its private key to the repository or a developer workstation.
2. Add multi-key verification and an explicit key identifier to the updater,
   online installer, manifest generator, checked-in public-key set, and
   cross-consumer tests. Continue signing the bridge release with the old key.
3. After the bridge release has been distributed through every supported
   installer and package channel, select the successor key in the protected
   release environment and publish a release signed by it.
4. Retain the old public key for a documented migration window. Remove it only
   after the supported upgrade floor can verify the successor key.

If the active private key is suspected compromised, disable the release
environment and updater publication immediately. Existing signatures can no
longer establish authenticity; recovery must use independently authenticated
OS/package-manager distribution and a reviewed bridge build, not a manifest
signed by either the suspected key or a silent replacement.
