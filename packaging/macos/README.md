# macOS packaging

`release.yml` builds a universal binary with `lipo`, assembles `kettle.app`
around it, then signs, notarizes and staples the bundle before zipping it.

## Why this matters

An unsigned `.app` is not merely warned about on current macOS — it is
effectively unopenable. macOS 15 removed the right-click → Open bypass for
unsigned applications, so the only route is the `xattr -dr
com.apple.quarantine` incantation in `docs/INSTALL.md`, which no ordinary user
should be asked to run.

Verify a release with:

```sh
codesign -dvvv --verbose=4 /Applications/kettle.app   # Developer ID authority
spctl --assess --type execute --verbose=4 /Applications/kettle.app
xcrun stapler validate /Applications/kettle.app
```

An ad-hoc/linker-signed build reports `rejected: no usable signature` from
`spctl`.

## Required repository secrets

Signing is skipped when `APPLE_CERT_P12` is empty, so forks and pull requests
still build a working unsigned bundle. A tag on `Reddimus/kettle` **fails
closed** instead — an official release must never publish unsigned under the
official filename.

| Secret | What it is |
| --- | --- |
| `APPLE_CERT_P12` | Base64 of the **Developer ID Application** certificate + private key, exported as `.p12` |
| `APPLE_CERT_PASSWORD` | The password set when exporting that `.p12` |
| `APPLE_SIGNING_IDENTITY` | The identity string, e.g. `Developer ID Application: Your Name (TEAMID)` |
| `APPLE_ID` | Apple ID of the Developer Program account |
| `APPLE_TEAM_ID` | 10-character team identifier |
| `APPLE_APP_PASSWORD` | An **app-specific** password from appleid.apple.com — not the account password |

Export the certificate from Keychain Access (My Certificates → the *Developer
ID Application* entry → Export), then:

```sh
base64 -i DeveloperID.p12 | pbcopy   # paste into APPLE_CERT_P12
security find-identity -v -p codesigning   # copy the identity string verbatim
```

Note the `package` job has no `environment:`, so these must be **repository**
secrets rather than environment secrets (the signed-update key lives in the
`release-signing` environment, which only `finalize` declares).

## Entitlements

`kettle.entitlements` is deliberately an empty dictionary. Entitlements are
exceptions to the hardened runtime, and a non-sandboxed terminal that spawns
processes through `forkpty(3)` and renders through Apple's own Metal frameworks
needs none of them. The file documents each one that is *not* claimed and why;
read it before adding any, because the notarization service scrutinises them.

## Ordering

Stapling rewrites the bundle, so the distribution zip is created **after**
stapling and the SHA-256 sidecar is generated from that final zip. Moving
either earlier publishes a hash that does not match what users download.

## Icons

`kettle.iconset/` holds the 10 source PNGs (16→512 at 1x and 2x). CI converts
them with `iconutil` at release time; `just icns-smoke` runs the same
conversion as a gate. `Info.plist` carries `1.0.0` placeholders that PlistBuddy
patches from `Cargo.toml` during packaging, so a locally built `.app` will show
the placeholder version.
