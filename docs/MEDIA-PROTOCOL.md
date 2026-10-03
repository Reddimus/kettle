# Bounded media protocol, P1

## Ownership

`kettle-media` is a leaf crate whose only dependency is `sha2`. It opens no files and starts no processes.
The public model uses six job kinds. Input limits apply when validating or encoding a job
and when decoding its frame. For paths, the worker later checks the opened object's size and
permissions using the job kind's input cap. External attestations are declarations from the
caller, not proof established by this crate.

`ExternalRequest` and the private-channel `Job` use different source types and frame kinds.
External paths carry only dev/ino attestations. GUI user pulls require an explicit action
witness. Decoding a private-channel UserPull never creates a GUI witness, and changing its
frame kind to an external request makes decoding fail.

The worker answers Ready only after setup. `check_ready` compares version, source hash and
protocol version, requiring the local protocol too. It returns RestartRequired on mismatch.
`check_peer_failure(UnknownMethod)` returns ReverseSkew with the older-GUI wording. Neither
helper retries. State sequencing and verification of which spawned worker supplied a reply
belong to P2.

## Wire version 1

The header is 11 bytes: `KMED`, u16 LE protocol version 1, a frame kind, then u32 LE payload
length. No padding or compression is permitted. Binary data and UTF-8 strings have u32 LE
byte lengths. Lists have u32 LE counts. Integers and f64 bit patterns use LE. Boolean and
optional-value discriminants are exactly 0 or 1. Floats must satisfy the field validators.
Native paths carry tag 0 for Unix bytes or tag 1 for Windows UTF-16LE, and the receiving
platform refuses the other encoding.

| Kind | Direction | Payload order |
| --- | --- | --- |
| 1 Hello | Parent to worker | Version text, source hash hex text, u16 build protocol |
| 2 Ready | Worker to parent | Same BuildId representation |
| 3 ExternalRequest | External to parent | Kind, restricted source, theme, canvas, target, fonts |
| 4 Job | Parent to worker | Kind, worker source, theme, canvas, target, fonts |
| 5 Rendered | Worker to parent | Width, height, RGBA blob, 32 digest bytes, optional path identity, display lines, fence count, optional index, fence sources, script names, warning codes |
| 6 Failure | Worker to parent | One fixed error-code byte |

Job-kind tags 0 through 5 are Mermaid, Svg, Raster, MarkdownDiagrams, VideoProbe and
VideoStills. Markdown adds one index byte. Stills adds count u8, edge u32, start f64, optional
end f64 and optional single-frame time f64. Single-frame time requires count 1, start 0 and
no end time. Interval times must be finite, nonnegative and ordered. The still-count cap is
16 and the requested edge cap is 2560, as in the video-frame contract.

Source tag 0 carries a bounded byte blob. Mermaid, SVG and Markdown bytes require UTF-8.
Tag 1 carries a native path followed by authorization tag 0 and dev/ino u64 values, or
private-channel authorization tag 1 with no payload. ExternalRequest refuses tag 1.
Theme stores background, foreground, all 16 palette colors and accent as four-byte RGBA
colors, then its dark flag. Canvas tags 0/1/2 mean theme/white/checker. Target stores width
u32, height u32, scale f64, optional crop of x/y/width/height u32. Crop lies within the target
box. Each font stores a native path and u32 face index.

Path identity stores dev, ino and size u64, signed seconds i64, then nanoseconds u32 below
one billion. Rendered rows use straight RGBA, without premultiplication. There is no stride
field. The byte count must equal checked width * height * 4. Digests refer to source content,
so the codec cannot verify them from returned pixels alone.

Encoding validates first, measures with checked lengths, then requests one exact frame
capacity. Decoding validates the header and the entire borrowed payload before requesting
owned storage. That pass checks trailing bytes too. The second pass measures each requested
byte copy and list capacity. The per-direction budgets include list element storage, rather
than treating serialized length as the only allocation bound. Allocation statistics do not
measure allocator rounding, caller-owned input or simultaneous stream buffering. Stream
readers buffer at most one capped frame and then invoke the same decoder. P2 must include
both that buffer and the owned result in footprint accounting.

## Digest

SHA-256 comes from the `sha2` crate the workspace already uses; a value computed
independently pins the framing. The input is exactly:

```text
"kettle-media-content-v1\0"
u64 LE content length
content bytes
u8 identity-present
if present: dev, ino, size, mtime seconds, mtime nanoseconds
```

The four first identity fields are eight bytes each, nanoseconds four bytes. This prevents
identical bytes read from different file versions from sharing a path cache key. The caller
supplies metadata from the held source descriptor. P2 must prove unchanged identity and
content while reading. SHA-256 here does not establish file authorization or worker identity.

## Fixed failures and unspecified bounds

Failure bytes 0 through 26 are pinned in `FailureCode` and round-trip tests. `code`, `reason`
and `model_message` map applicable cases to the model-facing error contract. Failures carry
no source, path, parser diagnostic or other input. WorkerUnavailable is the added fixed
reason for a missing or unverifiable worker. Codec/backend identities and platform package
hints are future bounded provider data, not free-form strings accepted in P1 failures.

The plan leaves some small metadata bounds unspecified. P1 chooses 64 bytes each for version
and source hash hex, 32 uncovered script names of 64 UTF-8 bytes, and at most 16 warnings from four
fixed variants. Native path length is a byte limit on both supported encodings. Empty build
names and hashes are refused; hex letter case is preserved, and exact build equality is
intentional. Changing tags, layouts or acceptance rules requires a protocol version decision
and updated golden fixtures.

## P2 boundary and separate worker decision

Production uses a separate O3 `kettle-media-worker` executable. The parent resolves it only
from its installation, verifies its identity, and validates build equality after startup.
No search through PATH or cwd, and no re-exec of the terminal for the new worker.

P2 adds the spawn/deadline/footprint/reap client, the heavy safe renderer and the actual worker
binary. It owns setup sequencing, one startup-only retry, no retry after Ready, fd sweep and
non-dumpable setup, minimal environment, neutral cwd, null stderr, process groups, rlimits,
self-sandbox, bounded pipe progress, aggregate child/decoder footprint monitoring and bounded
reaping/abandonment. It must enforce decoded-raster caps before codec allocation, device and
preview-account caps before upload, SVG amplification caps, source identity and no-write
policy. It also measures the separate worker's size, spawn-ready time and hostile footprint,
and verifies atomic two-binary packaging/update behavior. Renderer/platform acceptance,
installation identity and those resource budgets are not proved by P1's fixture.
