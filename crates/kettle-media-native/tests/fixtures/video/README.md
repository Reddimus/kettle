# Video fixtures

Four clips of the same picture: 64x36, 10 frames a second for 4 s (40
frames). Frame N is a flat gray whose luma is `N*5+20`, so a test can tell
from one pixel which frame a decoder returned. They were made on 2026-10-10
with Homebrew's ffmpeg 9.0.2 (libx264, libvpx) from the lavfi source
`color=c=black:s=64x36:r=10:d=4,format=yuv420p,geq=lum='N*5+20':cb=128:cr=128`:

- `index.mp4`: H.264 High in MP4, keyframes every 15 frames, two B-frames:
  `-c:v libx264 -preset veryslow -qp 1 -g 15 -bf 2 -pix_fmt yuv420p -tag:v avc1
  -movflags +faststart -map_metadata -1 -fflags +bitexact -flags:v +bitexact`.
- `index.webm`: VP8 in WebM, the format Playwright records:
  `-c:v libvpx -crf 4 -b:v 1M -g 15 -auto-alt-ref 0 -map_metadata -1
  -fflags +bitexact -flags:v +bitexact`.
- `rotated.mp4`: `index.mp4` with a display matrix turning it 90 degrees
  counterclockwise to show, written by `-display_rotation 90 -i index.mp4
  -c copy -map_metadata -1 -fflags +bitexact`.
- `unsized.webm`: `index.webm`'s encoding written to a pipe (`-f webm
  pipe:1`), so it has no duration and no cues, as a browser's recorder
  leaves a file.

| File | Bytes | SHA-256 |
| --- | --- | --- |
| index.mp4 | 2608 | `3781d0ef705adb66a0fbec071130a0c798d878e0c265596e0f8139c350be8b8f` |
| index.webm | 1901 | `552167ead9ec04762f13fc27c4a805c81f487e88d9e7203fddd8a9fc8d4b72f5` |
| rotated.mp4 | 2608 | `231033eff1d60b79fec78c86301b9ef596696e3905b6717f484492d96a0b3331` |
| unsized.webm | 1694 | `6d8912bb3b11f3f97c0a81a0df22faaa68fbeff9ae18429c7cd998f6e59165a1` |
