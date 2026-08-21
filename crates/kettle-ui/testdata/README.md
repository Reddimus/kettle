# Video preview fixture

`video-preview.mp4` is a one-second, 160 by 90 H.264 test pattern with no audio.
It exercises the native macOS and Windows poster workers in CI.

Regenerate it with:

```sh
ffmpeg -f lavfi -i 'testsrc2=s=160x90:d=1:r=12' -c:v libx264 \
  -pix_fmt yuv420p -movflags +faststart video-preview.mp4
```
