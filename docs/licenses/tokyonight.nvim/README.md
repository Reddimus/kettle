# TokyoNight palette license

Kettle bundles TokyoNight themes from the iTerm2-Color-Schemes Ghostty theme
collection. That collection identifies individual themes as belonging to their
authors. The TokyoNight palette originates in
[folke/tokyonight.nvim](https://github.com/folke/tokyonight.nvim), licensed under
Apache-2.0. The adjacent LICENSE retains its full license text.

Kettle Day derives from TokyoNight Day and changes text colors for contrast.
Its file header describes the changes; the other bundled TokyoNight variants
retain their supplied palettes. The collection license remains in
assets/themes/LICENSE.

License source: [cdc07ac78467a233fd62c493de29a17e0cf2b2b6](https://raw.githubusercontent.com/folke/tokyonight.nvim/cdc07ac78467a233fd62c493de29a17e0cf2b2b6/LICENSE), retrieved 2026-11-07.
SHA-256: `c71d239df91726fc519c6eb72d318ec65820627232b2f796219e87dcf35d0ab4`. This pins the license text,
not the historical palette revision.

macOS app bundles retain this directory under Contents/Resources/docs/licenses;
Linux release archives retain it under docs/licenses. Homebrew and Arch carry
docs/licenses through their existing third-party license packaging. The Linux
installer, Linux self-updater, and Nix packages explicitly install this LICENSE
and README.md under share/doc/kettle/docs/licenses/tokyonight.nvim. The macOS
self-updater replaces the complete app bundle, including its license resources.
Linux release packaging also appends this full LICENSE to the shipped NOTICE,
which existing Linux updaters already install even when they omit docs/licenses.
