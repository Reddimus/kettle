# Video fixtures

Four clips of the same picture: 64x36, 10 frames a second for 4 s (40
frames). Frame N shows its index in six blocks, three across and two down,
read left to right and top to bottom as bits 0 to 5 of N: white (luma 235)
for a one, black (16) for a zero. A block's centre survives scaling, and its
white or black survives any decoder's tone curve, so a test can tell which
frame a decoder returned. They were made on 2026-10-10 with Homebrew's ffmpeg
9.0.2 (libx264, libvpx) from the lavfi source
`color=c=black:s=64x36:r=10:d=4,format=yuv420p,geq=lum='if(mod(floor(N/pow(2,floor(X*3/W)+3*floor(Y*2/H))),2),235,16)':cb=128:cr=128`:

- `index.mp4`: H.264 High in MP4, keyframes every 15 frames, two B-frames:
  `-c:v libx264 -preset veryslow -qp 1 -g 15 -bf 2 -pix_fmt yuv420p -tag:v avc1
  -movflags +faststart -map_metadata -1 -fflags +bitexact -flags:v +bitexact`.
- `index.webm`: VP8 in WebM, the format Playwright records:
  `-c:v libvpx -crf 4 -b:v 1M -g 15 -auto-alt-ref 0 -map_metadata -1
  -fflags +bitexact -flags:v +bitexact`.
- `rotated.mp4`: `index.mp4` with a display matrix turning it 90 degrees
  counterclockwise to show, written by `-display_rotation 90 -i index.mp4
  -c copy -map_metadata -1 -fflags +bitexact`.
- `segments.mp4`: nine one-second segments, segment N showing N in the same
  blocks, H.264 with keyframes only where every other pair of segments meets
  (0, 2, 4, 6 and 8 s), so the middle of an odd segment has a keyframe half a
  segment after it and none before: `color=c=black:s=64x36:r=10:d=9` with
  the blocks taken from `floor(T)`, `-c:v libx264 -preset veryslow -qp 1 -g
  250 -bf 2 -sc_threshold 0 -force_key_frames 'expr:gte(t,n_forced*2)'` and
  the same tags and flags as `index.mp4`.
- `unsized.webm`: `index.webm`'s encoding written to a pipe (`-f webm
  pipe:1`), so it has no duration and no cues, as a browser's recorder
  leaves a file.

| File | Bytes | SHA-256 |
| --- | --- | --- |
| index.mp4 | 4117 | `4ac5bc7783ee1e305f32272fbab83fbdc46c2a96bb72a992cf326f914c0d9b42` |
| index.webm | 2925 | `28015bb902d6f4bcc99ba3fb6819e9960965a3fe546ff1dfd9ca7f2a7d181e78` |
| rotated.mp4 | 4117 | `fe665abfb89c63b567879cb42ba80028a9ab10df007e8c21285ac37f7052eca7` |
| unsized.webm | 2795 | `4759f8fbd8447efcd1303c7ee101fff0737aa1a5bd4133eaf08d72f062ddd946` |
| segments.mp4 | 4224 | `78ebea18da005ef402680afbf333bac3db4aa5e9e8f73f5c43e86d3f613705ac` |
