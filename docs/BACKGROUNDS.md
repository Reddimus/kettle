# Background images & animated wallpapers

kettle can paint a still image **or an animated loop** behind your terminal —
the native, GPU-friendly equivalent of the "video background" people set up in
other terminals. No terminal decodes actual video files; kettle plays an
**animated GIF / APNG / animated WebP** instead, advancing frames on the media's
own timestamps (capped by the ~30 fps render tick) and — by default — freezing
when the window loses focus, so it costs nothing in the background.

> **Off by default.** Nothing is bundled in kettle and `background-type` is
> `solid` out of the box. A wallpaper only appears once *you* point kettle at a
> file. This keeps the binary lean and startup fast for everyone who doesn't
> want one.

## Quick start

Add to your config (`~/.config/kettle/config`, or `%APPDATA%\kettle\config` on
Windows — find yours with `kettle --config-path`):

```ini
background-type  = image
background-image = ~/kettle-backgrounds/space.gif
background-animation = when-focused        # when-focused (default) | always | off
background-image-mode = stretch_and_fill   # stretch_and_fill | tile | center | scale
```

`background-type` and `background-animation` are also in **Settings → Appearance**
(open with `Ctrl+,`). The image *path* stays a config line — it needs a file, not
a cycle-through.

## How it composites (v2.23.0)

The wallpaper is drawn at the very back, and **everything else paints opaquely on
top of it** — the standard kitty / WezTerm / Alacritty layering:

```
window clear → wallpaper → cell backgrounds → chrome (tabs/status/titlebars) → text
```

So the tab bar, status bar, and any colored cell backgrounds (selections, syntax
highlight panels, TUI app panels) stay crisp and readable — the animation no
longer bleeds through them. Cells with the *default* background are transparent,
so the wallpaper shows through your terminal text exactly as you'd want.

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
| `background-darkness` | `0.0` fully dark … `1.0` no tint (default `0.5`) |
| `background-animation` | `when-focused` (battery-friendly default), `always`, `off` (freeze on frame 1) |

**Performance.** Frames decode once at load (bounded to 256 MB / 512 frames; a
larger file degrades gracefully to a shorter loop, never an OOM). Playback just
swaps an already-uploaded GPU texture, and `when-focused` parks the animation
clock entirely when you tab away — so an animated wallpaper adds **zero idle
cost** when unfocused and a single texture swap per frame when focused. Prefer a
gentle, slow loop (a drifting starfield, not a fast action clip) for the least
distraction and the lowest wake rate.

## Where to get good, clearly-licensed wallpapers

Use sources whose license actually allows redistribution/use. Two reliable,
popular ones:

### NASA — Scientific Visualization Studio (public domain)

NASA's imagery is generally **not copyrighted** and free to use (see NASA's
media-usage guidelines). The SVS publishes ready-to-use animated GIFs and loops —
nebulae, the Sun, Earth from orbit, galaxies — that make excellent slow,
ambient backgrounds.

- **NASA SVS:** <https://svs.gsfc.nasa.gov/> (filter for GIF/loop products)
- **NASA Image and Video Library:** <https://images.nasa.gov/>
- **Hubble / ESA-Hubble:** <https://esahubble.org/images/> (check each image's
  license; most Hubble imagery is public domain or CC BY)

> Always confirm the specific item's usage terms — a few NASA assets include
> third-party or partner content with separate restrictions.

### OpenGameArt — CC0 (public-domain-equivalent)

Filter OpenGameArt by the **CC0** license for seamless space / starfield /
parallax sets you can use with no attribution required:

- <https://opengameart.org/> → Art search → License: **CC0** → e.g. "space",
  "starfield", "nebula", "parallax"

Other good CC0 pools: **Wikimedia Commons** (filter for public domain / CC0) and
**Pexels/Pixabay** (their own license; check before redistributing).

### Make your own loop from a clip

If you have a clearly-licensed short video, convert a few seconds to a looping
GIF with `ffmpeg`:

```sh
ffmpeg -t 8 -i clip.mp4 \
  -vf "fps=12,scale=1920:-1:flags=lanczos" \
  -loop 0 ~/kettle-backgrounds/space.gif
```

Keep it short and low-fps — a terminal backdrop wants calm, not bandwidth.

## Troubleshooting

- **Nothing shows?** `background-type` must be `image` *and* `background-image`
  must point at a file that exists. Run with `RUST_LOG=warn kettle` to see decode
  warnings (not found / unsupported / too large).
- **Animation won't move while unfocused?** That's `background-animation =
  when-focused` (the default). Set `always` to keep it moving in the background.
- **Tabs still look busy?** Try `chrome-background = auto` (or `black`/`white`)
  and a calmer loop.
