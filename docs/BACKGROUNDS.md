# Background images & animated wallpapers

kettle can paint a still image **or an animated loop** behind your terminal —
the native, GPU-friendly equivalent of the "video background" people set up in
other terminals. No terminal decodes actual video files; kettle plays an
**animated GIF / APNG / animated WebP** instead, advancing frames on the media's
own timestamps (capped by the ~30 fps render tick) and — by default — freezing
when the window loses focus, so it costs nothing in the background.

> **Off by default.** `background-type` is `solid` out of the box and the binary
> embeds nothing. A wallpaper only appears once *you* point kettle at a file —
> keeping the binary lean and startup fast for everyone who doesn't want one.

## The included sample — a subtle starfield

kettle ships one ready-to-use sample: **[`docs/examples/space-starfield.gif`](examples/space-starfield.gif)**
— a slow **forward-flight** starfield: stars emerge near the center and drift
gently outward as if you're moving through space, then fade as they pass (the
"warp at low speed" look). It's the recommended starting point because it does
what a terminal background *should*: it recedes behind your text instead of
fighting it (see [What makes a good one](#what-makes-a-good-terminal-background)),
and because it's a uniform radial field of tiny dots it looks right at **every
aspect ratio and resolution** — 4:3, 16:9, 16:10, 21:9, even square — with
nothing to distort when stretched.

Point your config at it (or copy it somewhere first):

```ini
background-type  = image
background-image = ~/path/to/space-starfield.gif
background-animation = when-focused        # when-focused (default) | always | off
background-image-mode = stretch_and_fill   # safe for the starfield at any aspect
chrome-background = auto                    # tab/status bar tint from the wallpaper
```

Regenerate or customize it with the bundled generator (needs `pip install pillow`):

```sh
python scripts/gen-starfield.py ~/kettle-backgrounds/space-starfield.gif
```

`background-type` and `background-animation` are also in **Settings → Appearance**
(`Ctrl+,`). The image *path* stays a config line — it needs a file, not a cycle.

## What makes a good terminal background

A terminal already has text in every corner, so the background must stay out of
the way. The look people actually keep (and the reason the starfield was chosen):

- **Dark + low-contrast.** A near-black image keeps light text readable on any
  theme. Bright, high-detail loops (nebulae, photos, accretion disks) wash out
  text — avoid them, or pair them with `background-blur = true` and a low
  `background-darkness`.
- **Slow + subtle.** Gentle motion reads as "alive," not distracting (WezTerm
  users routinely slow GIFs to 0.2×). A calm twinkle beats a fast clip.
- **Aspect-agnostic.** A uniform/abstract field (stars, particles, soft
  gradients) survives stretching to any screen; a recognizable scene distorts.

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

Start with the [included starfield](#the-included-sample--a-subtle-starfield).
If you want something else, keep the [principles](#what-makes-a-good-terminal-background)
in mind (dark, slow, abstract) and use sources whose license allows use:

### Pixel-art space / particle loops (the popular choice)

The dark, subtle, pixel-art "space" look is what most people run, and it's
aspect-agnostic. These let you make your own and are free / CC0:

- **Deep-Fold — Pixel Space Background Generator:** <https://deep-fold.itch.io/space-background-generator>
  — generate a seamless, looping pixel starfield/nebula; tile-able, export to GIF.
- **ansimuz — space backgrounds (CC0):** seamless looped space art, highly rated.
- **itch.io CC0 packs:** <https://itch.io/game-assets/free/tag-cc0/tag-pixel-art>
  → search "space", "starfield", "rain", "night". Confirm each pack is **CC0**.

### OpenGameArt — CC0 (public-domain-equivalent)

- <https://opengameart.org/> → Art search → License: **CC0** → e.g. "space",
  "starfield", "particles", "parallax". No attribution required.

### NASA / Hubble (public domain — dramatic, but dim it)

NASA imagery is public domain (see NASA's media-usage guidelines) — nebulae, the
Sun, galaxies. It's **bright and busy**, so it fights text by default: pair it
with `background-blur = true` + a low `background-darkness`, and `chrome-background
= auto`, or it'll wash out your terminal.

- **NASA SVS:** <https://svs.gsfc.nasa.gov/> · **Library:** <https://images.nasa.gov/>
  · **Hubble:** <https://esahubble.org/images/> (confirm each item's terms).

### Make your own loop

- **Bundled generator** (recommended): `python scripts/gen-starfield.py out.gif`
  — tweak the constants for density, speed, color.
- **From a clearly-licensed clip** with `ffmpeg` — keep it short, low-fps, dark:

  ```sh
  ffmpeg -t 8 -i clip.mp4 -vf "fps=12,scale=1600:-1:flags=lanczos" \
    -loop 0 ~/kettle-backgrounds/my-bg.gif
  ```

## Troubleshooting

- **Nothing shows?** `background-type` must be `image` *and* `background-image`
  must point at a file that exists. Run with `RUST_LOG=warn kettle` to see decode
  warnings (not found / unsupported / too large).
- **Animation won't move while unfocused?** That's `background-animation =
  when-focused` (the default). Set `always` to keep it moving in the background.
- **Tabs still look busy?** Try `chrome-background = auto` (or `black`/`white`)
  and a calmer loop.
