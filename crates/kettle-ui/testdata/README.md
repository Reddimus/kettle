# Video preview fixture

`video-preview.mp4` is a one-second, 160 by 90 H.264 test pattern with no audio.
The [shipped-worker tests](../../kettle/tests/video_preview_native.rs) cover
Unix identity/content checks and Windows native poster extraction in CI.
Separate [macOS rendering tests](../../kettle-media-native/tests/avfoundation.rs)
exercise AVFoundation with the `avfoundation` feature.

Regenerate it with:

```sh
ffmpeg -f lavfi -i 'testsrc2=s=160x90:d=1:r=12' -c:v libx264 \
  -pix_fmt yuv420p -movflags +faststart video-preview.mp4
```
