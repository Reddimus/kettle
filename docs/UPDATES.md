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
| Local source or `just install-local-dev-record` build | Refused; rebuild and reinstall from that checkout |
| Cargo, distro package, Homebrew, AUR, Nix, or manually copied binary | Refused; update with its owner |
| macOS app | Use the release page or Homebrew; in-app replacement is not yet supported |

Official installers write a small ownership marker beside the managed layout.
The updater derives the install prefix from the running executable and requires
that marker to match Kettle, the stable channel, and the current Rust target.
It never searches `PATH` for another copy and never elevates privileges.
Repository installs deliberately use a `local-dev` marker, or
`local-dev-record` when the launcher enables development recording. Refusing
those channels prevents the stable updater from replacing a locally featured
binary or rewriting its launcher. Only an extracted release tarball or the
online installer writes a `stable` marker.

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

The default is a passive notification check at most once every 24 hours. The
first launch only creates the throttle state and performs no network request.

```ini
update-policy = off       # no automatic network request
update-policy = notify    # default: show a passive new-version banner
update-policy = auto      # install in the background, use after next restart
```

The legacy `update-check = true|false` setting remains compatible and maps to
`notify|off`. If both settings exist, `update-policy` wins regardless of order.
Builds produced with `KETTLE_PACKAGED` disable automatic checks so a downstream
distribution can own update policy. Explicit CLI checks still work.

## Verification and recovery

Each stable release publishes `kettle-update-manifest.json` and a detached
Ed25519 signature. Kettle embeds only the dedicated public key. Before parsing
or using metadata it verifies the signature over a domain-separated payload.
The signed manifest binds each supported target to an exact archive name, byte
length, and SHA-256 digest.

Downloads are capped at 256 MiB. Extraction accepts at most 128 entries and 512
MiB of actual output. Absolute paths, traversal, duplicates and case aliases,
file/directory prefix conflicts, encrypted entries, links, special/sparse files,
Windows device names, and declared/actual size mismatches are rejected. Updates
acquire an install-prefix lock and atomically replace each destination on its own
filesystem.

When an archive contains `kettle-package-manifest.json`, Kettle additionally
binds its product/version/target and every extracted regular file to an exact
relative path, size, SHA-256, and Unix mode where applicable before staging is
accepted.
The v2.35 verifier is backward-compatible with earlier release archives that do
not yet contain this inner package manifest; the signed feed's archive size and
SHA-256 remain mandatory in either case.

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
