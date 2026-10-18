# Still-image fixtures

Two stills of one picture: 640x480, every pixel sRGB (230, 40, 20). They
were made on 2026-10-17 on macOS 27 with `sips` from a PNG Python wrote
with nothing but `zlib` and `struct` (no colour profile, 8 bits a channel):

```python
import struct, zlib
w, h, rgb = 640, 480, (230, 40, 20)
raw = b"".join(b"\0" + bytes(rgb) * w for _ in range(h))
def chunk(kind, data):
    return (struct.pack(">I", len(data)) + kind + data
            + struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF))
open("solid.png", "wb").write(
    b"\x89PNG\r\n\x1a\n"
    + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
    + chunk(b"IDAT", zlib.compress(raw, 9))
    + chunk(b"IEND", b""))
```

- `solid.heic`: `sips -s format heic solid.png --out solid.heic` (HEVC,
  brands `heic mif1`).
- `solid.avif`: `sips -s format avif solid.png --out solid.avif` (AV1,
  brands `avif mif1`).
- `grid.heic`: the same colour at 1024x768, from the same script with
  `w, h = 1024, 768`, by `sips -s format heic`, which stores a still that size
  as a grid of tiles (`ffprobe -show_entries stream_group=type` says
  `Tile Grid`), as cameras store their photos.

The colour is far from grey on every channel, so a decoder that left it in
video range, or swapped its channels, would move it well past the few
levels lossy coding does.
