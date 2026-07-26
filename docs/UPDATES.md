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

On Windows, the updater writes a bounded pending record inside the
installer-owned prefix containing the target version and the size/SHA-256 of
every extracted file, copies the current binary to a uniquely named helper, and
starts that helper. Its ACL is inherited from the selected install prefix, so a
custom shared prefix must itself be access-controlled. Every managed Kettle process
holds a shared run lock; the helper takes it exclusively, also waits until the
installed `.exe` and `.com` are no longer mapped, re-verifies the staged files,
and then commits the transaction. A launch that sees pending state starts the
helper and exits rather than prolonging the old version. Failed attempts retain
the pending record, staged files, and a bounded error message for the next
launch to retry. Automatic retry stops after three failed helper attempts. An
invalid or exhausted pending record is atomically quarantined as
`.kettle-update-failed-*.json`; Kettle also writes a bounded `.txt` diagnostic
when the prefix permits it. Quarantine itself is best-effort so a read-only
prefix or antivirus sharing denial cannot block the intact old binary from
starting. Startup emits a stderr message and attempts a desktop recovery
notification; notification failure is logged. Evidence that could be preserved
remains in the install prefix for diagnosis instead of trapping every future
launch in a handoff loop.

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
integrity and checkpoints every restored entry, so a second interruption simply
resumes rollback. A durably committed journal is cleaned without reverting the
new files. The journal is deleted and its parent synced before backup cleanup,
so recovery never points at data it already removed. Schema-1 journals left by
v2.34 remain recoverable in the corrected order.

Release CI accepts only a GitHub-verified annotated tag. All required platform
artifacts must finish before the signing job can access its environment secret.
Assets are uploaded to a draft and checked against local names and sizes before
the release becomes public, preventing a partially published update feed.
