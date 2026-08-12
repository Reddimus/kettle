# macOS distribution signing

Kettle's ordinary pull-request builds remain unsigned. The `package` job uses
the protected `macos-signing` environment for signed manual and tag builds.
Official tags fail closed when its signing material is absent.

## Required environment secrets

| Secret | Value |
| --- | --- |
| `APPLE_CERT_P12` | Base64 of a Developer ID Application certificate and its private key exported as password-protected PKCS#12 |
| `APPLE_CERT_PASSWORD` | PKCS#12 export password |
| `APPLE_SIGNING_IDENTITY` | Exact `Developer ID Application: Name (TEAMID)` identity reported by `security find-identity -v -p codesigning` |
| `APPLE_TEAM_ID` | Apple Developer Team ID |
| `APPLE_API_KEY_ID` | App Store Connect API key identifier |
| `APPLE_API_ISSUER_ID` | App Store Connect API issuer identifier |
| `APPLE_API_PRIVATE_KEY` | Complete contents of the downloaded `AuthKey_*.p8` file |

Use a Kettle-specific Developer ID certificate and App Store Connect API key.
The workflow reconstructs both only under `RUNNER_TEMP`, imports the identity
into an ephemeral keychain restricted to `codesign`, and deletes the material
in an unconditional cleanup step. Do not store Apple-ID passwords or app-
specific passwords; notarization uses the API key.

Before saving the secrets, prove the local export contains the private key:

```sh
security find-identity -v -p codesigning
```

## Signing order

The order is load-bearing:

1. Compile `AppIcon.icon` and assemble the complete bundle.
2. Sign every nested Mach-O, then sign the outer `.app` with hardened runtime
   and a secure timestamp.
3. Verify the seal and submit a `ditto --keepParent` archive to notarization.
4. Staple and validate the accepted ticket, then run Gatekeeper assessment.
5. Create the release ZIP and SHA-256 sidecar from the final stapled bytes.

`kettle.entitlements` is intentionally empty. A terminal needs `forkpty` and
Metal, neither of which requires a hardened-runtime exception. Add no
entitlement without a failing signed/notarized runtime test and a documented
reason.
