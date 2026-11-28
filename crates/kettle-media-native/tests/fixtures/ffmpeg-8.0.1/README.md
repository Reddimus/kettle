# FFmpeg 8.0.1 answers

What Ubuntu 26.04.1 LTS's `ffmpeg` and `ffprobe` 8.0.1 (package
`8.0.1-3ubuntu2`, x86_64) wrote for this crate's fixtures on 2026-11-26,
asked exactly as `src/ffmpeg.rs` asks, with the fixture as stdin
(`file:/proc/self/fd/0`). CI's Linux runners carry an FFmpeg too old to read
a still, so the unit tests read these instead.

- `version.txt`: the first line of `ffmpeg -hide_banner -version`.
- `solid.heic.txt`, `solid.avif.txt`, `grid.heic.txt`: `still_probe_args`
  for `../still/` (`-f mov -show_entries
  stream=codec_type,codec_name,width,height:stream_side_data=rotation:stream_group=type:stream_group_components
  -of flat`).
- `index.webm.txt`: `probe_args` for `../video/index.webm` (`-f matroska`,
  the description a stills job and a session start from).

Each exited 0 with nothing on stderr. The fixtures it read had the SHA-256
sums of the ones here:

```text
1423352516df93f51ec2fc4ec6b55daafb2f3cf3075506ec05338f46a3cd6d10  solid.heic
1c2abe921f8c15f894475c6c32fac2a4c2fbb4a8d5559e1407f1d1ceadcda817  solid.avif
f81c8b5dbe82c2ba60492ceba1a110b8dfe28b058d3b5ca233c7c7e439070acc  grid.heic
28015bb902d6f4bcc99ba3fb6819e9960965a3fe546ff1dfd9ca7f2a7d181e78  index.webm
```

The answers match Homebrew's FFmpeg 9.1 on macOS byte for byte.
