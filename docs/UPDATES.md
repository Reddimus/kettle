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
windows. A successful update is used the next time Kettle starts.

## Supported self-update layouts

| Running Kettle | Self-update behavior |
|---|---|
| Windows 11 x86_64 installed by the bundled `install.ps1` | Supported |
| The same Windows executable launched from WSL | Supported through Windows interop |
| Ubuntu/Linux x86_64 installed by the bundled `install.sh` or online installer | Supported |
| Ubuntu/Linux aarch64 installed by the bundled installer | Supported |
| Cargo, distro package, Homebrew, AUR, Nix, or manually copied binary | Refused; update with its owner |
| macOS app | Use the release page or Homebrew; in-app replacement is not yet supported |

Official installers write a small ownership marker beside the managed layout.
The updater derives the install prefix from the running executable and requires
that marker to match Kettle, the stable channel, and the current Rust target.
It never searches `PATH` for another copy and never elevates privileges.

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

Downloads and extracted archives have strict size and entry-count limits.
Absolute paths, traversal, duplicate paths, links, special files, and unsafe
Windows names are rejected. Updates acquire an install-prefix lock, keep a
transaction journal and backups, and atomically replace each destination on its
own filesystem. An interrupted transaction is rolled back before another update
is attempted.

Release CI accepts only a GitHub-verified annotated tag. All required platform
artifacts must finish before the signing job can access its environment secret.
Assets are uploaded to a draft and checked against local names and sizes before
the release becomes public, preventing a partially published update feed.
