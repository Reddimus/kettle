# Background images & animated wallpapers

Kettle can paint a procedural starfield, a still image, or a GIF/APNG/WebP
animation behind the terminal. Video files are not wallpaper inputs.
Animation continues while unfocused by default; hidden or minimized windows
stop animation redraws when the platform reports that state. Full occlusion
notifications are unavailable on Wayland.

Backgrounds are off by default (`background-type = solid`). The starfield
needs no image file.

## The included starfield — a fixed built-in example (recommended)

The built-in starfield draws a slow forward-flight effect with a GPU shader,
without decoded image frames. Its speed, density and glow are fixed; the
general background controls below still apply. Enable it with:

```ini
background-type = starfield
```

…or pick **starfield** in **Settings → Background** (`Ctrl+,`). The general
background controls still apply to it:

```ini
# always (default) | when-focused | off (frozen)
background-animation = always
# tab/status bar color over the field
chrome-background = theme
```

The starfield requests up to 10 animation redraws per second.
`chrome-background` controls the bar color as described below. Use an image
file for a different animation.

## Using your own image (file-based)

Point kettle at any PNG / JPEG / WebP / BMP, or an animated GIF / APNG / WebP:

```ini
background-type  = image
# editable in Settings → Background
background-image = ~/path/to/wallpaper.png
# always | when-focused | off
background-animation = always
background-image-mode = stretch_and_fill
# tab/status bar tint from the image
chrome-background = auto
```

The image path is editable **inline in Settings → Background** (no config edit
needed). Prefer a dark, slow, abstract loop — see
[What makes a good one](#what-makes-a-good-terminal-background).

Want a *file* starfield instead of the procedural one? The bundled generator
still ships (needs `pip install pillow`):

```sh
python scripts/gen-starfield.py ~/kettle-backgrounds/space-starfield.gif
```

## What makes a good terminal background

A terminal already has text in every corner, so the background must stay out of
the way. The look people actually keep (and the reason the starfield was chosen):

- **Dark + low-contrast.** A near-black image keeps light text readable on any
  theme. Bright, high-detail loops (nebulae, photos, accretion disks) wash out
  text — avoid them, or pair them with `background-blur = true` and a high
  `background-darkness`.
- **Slow + subtle.** Gentle motion reads as alive rather than distracting. A
  calm twinkle beats a fast clip.
- **Aspect-agnostic.** A uniform/abstract field (stars, particles, soft
  gradients) survives stretching to any screen; a recognizable scene distorts.

## How it composites (v2.23.0)

The wallpaper is drawn at the very back, and **everything else paints opaquely on
top of it**:

```
window clear → wallpaper → cell backgrounds → chrome (tabs/status/titlebars) → text
```

So the tab bar, status bar, and any colored cell backgrounds (selections, syntax
highlight panels, TUI app panels) stay crisp and readable; the animation does not
bleed through them. Cells with the *default* background let the wallpaper show
through, tinted by `background-darkness`.

### Chrome color over a wallpaper — `chrome-background`

The chrome strips are opaque over the wallpaper; `chrome-background` picks what
color:

| Value | Result |
|---|---|
| `theme` *(default)* | the theme's chrome color — matches the no-wallpaper look |
| `auto` | the wallpaper's average color, automatically nudged dark/light enough to keep the tab text readable — "inspired by" the background |
| `black` / `white` | a fixed neutral panel |

```ini
chrome-background = auto
```

## Tuning

| Key | What it does |
|---|---|
| `background-image-mode` | `stretch_and_fill` (default), `tile`, `center`, `scale` (aspect-preserving fit) |
| `background-image-align-horiz` / `-vert` | position for `center` / `scale` |
| `background-blur` | CPU 3-pass box blur at load (a soft, subtle backdrop) |
| `background-darkness` | `0.0` fully see-through (backdrop at full strength) … `1.0` fully covered (backdrop hidden); default `0.5` |
| `background-animation` | `always` (default), `when-focused` (battery-friendly), `off` (freeze on frame 1) — applies to the starfield and animated images alike |

**Performance.** Decoded animations are capped at 128 MiB and 128 frames;
longer animations retain only the frames that fit. Frames decode once per
load, and the renderer caches their textures. `when-focused` stops
animation-driven redraws while unfocused; other repaints can still advance
the displayed frame. Slower loops need fewer animation wakeups.

## Where to get good, clearly-licensed wallpapers

Start with the [included starfield](#the-included-starfield--a-fixed-built-in-example-recommended).
If you want something else, keep the [principles](#what-makes-a-good-terminal-background)
in mind (dark, slow, abstract) and use sources whose license allows use:

### Pixel art and generated images

- [Deep-Fold's generator](https://deep-fold.itch.io/space-background-generator)
  allows generated images in projects, but prohibits redistributing them alone
  or in asset packs. Its output is not CC0.
- [itch.io's CC0 pixel-art filter](https://itch.io/game-assets/free/tag-cc0/tag-pixel-art)
  and [OpenGameArt](https://opengameart.org/) offer asset searches. Check each
  item's license; on OpenGameArt, select the CC0 license filter.

### NASA and ESA/Hubble imagery

- [NASA SVS](https://svs.gsfc.nasa.gov/) and the
  [NASA image library](https://images.nasa.gov/): follow
  [NASA's usage guidelines](https://www.nasa.gov/nasa-brand-center/images-and-media/).
  Some hosted material has third-party rights; check its credit and terms.
- [ESA/Hubble images](https://esahubble.org/images/): follow the
  [attribution and reuse terms](https://esahubble.org/copyright/), generally
  CC BY 4.0. Preserve the full credit and check for exceptions.

Bright imagery can obscure terminal text. Use `background-blur` and a higher
`background-darkness` to dim it.

### Make your own loop

- **Bundled generator** (recommended): `python scripts/gen-starfield.py out.gif`
  — tweak the constants for density, speed, color.
- **From a clearly-licensed clip** with `ffmpeg`. Keep it short, low-fps and
  dark, and keep width × height × 4 bytes × frames under 128 MiB, or kettle
  loads only the first part of the loop. This one is 32 frames at 1280×720,
  about 118 MB:

  ```sh
  ffmpeg -t 4 -i clip.mp4 -vf "fps=8,scale=1280:-1:flags=lanczos" \
    -loop 0 ~/kettle-backgrounds/my-bg.gif
  ```

## Troubleshooting

- **Nothing shows?** For an image, `background-type` must be `image` *and*
  `background-image` must point at a file that exists (run `RUST_LOG=warn kettle`
  to see decode warnings — not found / unsupported / too large). For the
  starfield, just `background-type = starfield` (no file). And `background-type`
  defaults to `solid`, so a background only appears once you pick one.
- **Animation stops when I switch windows?** `when-focused` stops animation
  redraws on focus loss. The `always` default keeps visible, unfocused windows
  animating. Hidden/minimized windows stop those redraws when their state is
  available; Wayland does not report full occlusion.
- **Tabs still look busy?** Try `chrome-background = auto` (or `black`/`white`)
  and a calmer loop.
