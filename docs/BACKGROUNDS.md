# Background images & animated wallpapers

kettle can paint a **procedural starfield**, a still image, **or an animated
loop** behind your terminal — the native, GPU-friendly equivalent of the "video
background" people set up in other terminals. For a file you supply, no terminal
decodes actual video; kettle plays an **animated GIF / APNG / animated WebP**,
advancing frames on the media's own timestamps. By default an animated
background **plays even when unfocused**, but it **freezes when the window is
minimized or fully covered** (it can't be seen), so a hidden window costs nothing.

> **Off by default.** `background-type` is `solid` out of the box and the binary
> embeds nothing. A background only appears once *you* choose one — keeping the
> binary lean and startup fast for everyone who doesn't want one.

## The included starfield — a fixed built-in example (recommended)

kettle has a built-in **procedural starfield**: a slow **forward-flight** field
where stars emerge from the dark near the center, **fade in dramatically as they
get closer**, and bloom as they drift outward (the "warp at low speed" look).
It's rendered live by a tiny GPU shader, so unlike a GIF it's **true-color** (soft
star glows don't band), loops perfectly, stays crisp at **every resolution and
aspect ratio** (4:3 → 21:9 → square), and uses **~zero memory** (no decoded
frames). It's the recommended background because it does what a terminal
background *should*: recede behind your text instead of fighting it.

It's a **fixed example** — its look (speed, star count, glow, fade-in) is baked
into the shader and isn't config-tunable, so there's just one setting to turn it
on (no file needed):

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

> A pure-black starfield pairs well with `chrome-background = theme` (a distinct
> bar) or `auto` (a seamless black bar). It animates at a low ~10 fps cap, so
> idle CPU stays near a static background's. Want a *different* animated look?
> Use a file-based background (below) — the starfield itself is intentionally
> one curated example, not a tweakable engine.

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
| `background-darkness` | `0.0` fully see-through (backdrop at full strength) … `1.0` fully covered (backdrop hidden); default `0.5` |
| `background-animation` | `always` (default), `when-focused` (battery-friendly), `off` (freeze on frame 1) — applies to the starfield and animated images alike |

**Performance.** Frames decode once at load (bounded to 128 MiB / 128 frames; a
larger file degrades gracefully to a shorter loop, never an OOM). Playback just
swaps an already-uploaded GPU texture, and `when-focused` parks the animation
clock entirely when you tab away — so an animated wallpaper adds **zero idle
cost** when unfocused and a single texture swap per frame when focused. Prefer a
gentle, slow loop (a drifting starfield, not a fast action clip) for the least
distraction and the lowest wake rate.

## Where to get good, clearly-licensed wallpapers

Start with the [included starfield](#the-included-starfield--a-fixed-built-in-example-recommended).
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

- **Nothing shows?** For an image, `background-type` must be `image` *and*
  `background-image` must point at a file that exists (run `RUST_LOG=warn kettle`
  to see decode warnings — not found / unsupported / too large). For the
  starfield, just `background-type = starfield` (no file). And `background-type`
  defaults to `solid`, so a background only appears once you pick one.
- **Animation freezes when I switch windows?** Only if it's *hidden* (minimized
  or fully covered) — that's intentional (zero idle for an invisible window). An
  unfocused-but-visible window keeps animating under the `always` default; set
  `background-animation = when-focused` if you'd rather it pause on blur.
- **Tabs still look busy?** Try `chrome-background = auto` (or `black`/`white`)
  and a calmer loop.
