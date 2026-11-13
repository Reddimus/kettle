# Linux UI baselines

These 80 synthetic window captures cover ten states, English and Spanish,
dark and light themes, and default (1280x820) and narrow (640x820) windows.
They contain fixture output and isolated application state.

The initial images come from the successful Linux CI job in
[run 42383598778](https://github.com/Reddimus/kettle/actions/runs/42383598778),
artifact `ui-states-42383598778-1`. Its actual checkout is
`679f3362ebc5d666e94ed4887e6b6c8798614991`, whose tree
`d9c5ff5f6c14bb52c8908bd856bcf5a20edaab6f` equals PR #820 head
`34cc7a8e7d4af8eed5c53c9aac42eaf2461a5466`. The tested binary SHA-256 is
`98417207e751aeb307193825416eefe2c40840d04964b99a52cfdd502dff37a7`.
The run's Windows job failed unrelated command-mark and teardown tests; the
Linux state capture and its owned cleanup completed successfully. This
provenance does not imply that the whole workflow passed.
The Linux job used Weston 13.0.0 headless/GL, scale 1, Mesa 25.2.8 llvmpipe,
and only the repository's bundled JetBrains Mono and Fira Sans fonts.
The artifact retains the environment, source/font hashes, all 80 fixture
records, and the completed process cleanup receipt.

The 24 `palette-empty`, `palette-query` and `theme-picker` captures were
replaced by PR #855 from the Linux CI job in
[run 43502436986](https://github.com/Reddimus/kettle/actions/runs/43502436986),
artifact `ui-states-43502436986-1`, in the same environment: pickers fill their
rows and show a scrollbar thumb, and the palette drops weak matches. The PR
description shows each before and after.

The states pin these visible arrangements:

| State | Expected content |
| --- | --- |
| `default` | Fixture text, prompt, and normal window |
| `palette-empty` | Initial command rows and shortcut hints |
| `palette-query` | Commands matching `split`: word-start and contiguous matches only, so scattered mid-word matches drop out while a stronger match exists |
| `palette-nomatch` | Empty-result message with the query retained |
| `settings` | Appearance controls and responsive navigation |
| `search-matches` | Match highlight, count and search controls |
| `search-nomatch` | Search query and no-match state |
| `tabs` | Three tabs with one active tab |
| `splits` | Three nonoverlapping panes and active-pane border |
| `theme-picker` | Theme rows filling the panel with their colour swatches, light/dark labels, a scrollbar thumb, and active selection (the narrow window drops the swatches) |

CI regenerates these files through the private Wayland runner and compares
each name against this directory. The existing thresholds remain 24 per
channel and at most 0.5 percent changed pixels; a size mismatch or missing
baseline fails. A baseline update must explain the intended visual change
and show its before/after images. Do not copy a failing capture over a baseline
to make the comparison pass. The fixture comparison does not establish native
GNOME/KDE behavior, macOS pixel parity, IME or accessibility acceptance.
