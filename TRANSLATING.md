# Translating Kettle

Kettle's own text lives in [`crates/kettle-i18n`](crates/kettle-i18n): the
labels, menus, prompts, notifications and screen-reader names a person sees.
Translators edit three TOML files. The build turns them into typed Rust, so a
mistake in a catalogue fails the build instead of showing a broken label.

| File | What it holds |
|---|---|
| `locales/schema.toml` | One entry per message: translator context, arguments, plural selector |
| `locales/en.toml` | English, the reference text |
| `locales/es.toml` | Spanish, neutral Latin American |
| `locales/es.review.toml` | Review status of each Spanish message |

Never edit the generated Rust in `target/`; it is rebuilt from the TOML.

## What is translated

All Kettle-owned GUI text: Settings, the command palette and pickers, menus,
the Dock menu, prompts and confirmations, the search bar, paste receipts,
banners, desktop notifications and accessibility names.

Not translated, in any language:

- terminal content, shell output and anything a program writes;
- user and host names, paths, theme and font names, and other config values;
- config keys and values, keybind action names, flags and subcommands;
- logs, `kettle ctl` JSON, `ui_geometry` and other control-protocol output;
- the CLI's help and error text, the man page and these docs (English in 5.0).

## Messages

Each message has a snake_case key. Its schema entry says where it appears and
what a translator needs to know:

```toml
[messages.confirm_close_tab]
context = "crates/kettle-ui/src/app.rs: confirmation before closing a tab. count is the number of panes in that tab."
args = [{ name = "count", type = "u64" }]
plural = "count"
```

- `context` names the code that shows the message and anything a translator
  needs: what a word refers to, the grammatical gender of a noun it agrees
  with, what each argument holds, a column budget.
- `args` lists named arguments in caller order. Types are `str` and `u64`.
- `plural` names a `u64` argument that selects a `one` or `other` branch.

Keep keys stable, and never reuse a config value as a label.

### Placeholders

Write arguments as `{name}`. Every language must use the same set of
placeholders, in any order, so Spanish can reorder a sentence. `{{` and `}}`
are literal braces. Format specifiers, positional placeholders and expressions
fail the build.

```toml
picker_a11y_results = "{label} results"     # en.toml
picker_a11y_results = "{label}: resultados" # es.toml
```

Arguments are data: whatever a caller passes, including braces, quotes and
newlines, is shown verbatim.

### Plurals

A plural message has a `one` and an `other` table. Only a count of exactly 1
selects `one`; 0 and every larger count use `other`.

```toml
[confirm_close_window]
one = "Close {count} pane?"
other = "Close {count} panes?"
```

English and Spanish need only these two forms. A language with more plural
forms (Polish, Arabic, Russian) needs an implementation decision first: the
generator and every plural message would gain branches.

### Escaping and punctuation

Catalogue text is a TOML string: escape `"` and `\`, and write a line break
as `\n`. Text is emitted as a Rust string literal, so it can never become code.
Use the typographic characters the English uses (`…`, `—`, `·`, `✓`), keep a
trailing ellipsis only where the action opens something that asks for more,
and keep messages free of leading or trailing spaces.

## Spanish

One neutral Latin American catalogue serves every Spanish locale: no
*vosotros*, no regional slang. Use infinitives for actions ("Cerrar panel",
"Reiniciar Kettle para cambiar el idioma").

### Glossary

| English | Spanish | Notes |
|---|---|---|
| pane | panel | Never `ventana`, which is an OS window |
| tab | pestaña | Never `tabulador` |
| window | ventana | |
| split (action) | dividir | `Dividir a la derecha`, `Dividir hacia abajo` |
| scrollback | historial de la terminal | Short labels: `Líneas del historial` |
| keybind | atajo de teclado | |
| command palette | paleta de comandos | |
| shell prompt | indicador de la shell | |
| dialog prompt | mensaje, pregunta | Never the shell term |
| shell | shell | Keep Bash, Zsh, Fish and PowerShell names |
| terminal | la terminal | Feminine, consistently |
| settings | configuración | |
| key chord | combinación de teclas | Keep Ctrl, Alt, Shift, Esc, Enter as they are |
| broadcast input | enviar la entrada a varios paneles | |
| read-only | solo lectura | |
| clipboard | portapapeles | |
| scroll bar | barra de desplazamiento | |

### Widths

Most surfaces size themselves from the text: Settings, pickers, menus and the
search bar. A paste receipt does not: at the standard size its title has room
for about 19 columns and its detail lines 22, noted in the schema `context`,
and a longer line is cut with an ellipsis. The compact receipt widens for its
title, and the remote warning is always kept whole where the card fits.

## Fallback

Every shipping language holds every key; the build enforces it. If a message
were ever missing, it would fall back to its English text, never to its key.
An unsupported or malformed OS locale selects English.

## Choosing the language

The `language` config key (`auto`, `en`, `es`; Settings → Behavior →
Language) is read at startup. `auto` follows the OS locale: a Spanish locale of
any region selects Spanish. A change applies when Kettle restarts.

## Checking a translation

```sh
cargo test -p kettle-i18n
cargo test -p kettle-i18n --features dev-pseudo
cargo test -p kettle-ui -p kettle-render
```

The tests check every message in both languages, plural boundaries,
placeholders, and that no Spanish message, formatted and plural ones included,
is left in English except listed cognates. Layout checks run the text-sized
surfaces against English, Spanish and a pseudo-locale about 38% longer than
English. A drift guard fails if UI code passes literal prose straight to a
screen-reader name, a notification, a window title, a menu or picker row, a
prompt or painted chrome; text reaching those through a variable is a review
matter.

To see Spanish in context, set `language = es` in a scratch config and run
`kettle --config <that file>`. Review every surface the change touches:
Settings in each category, the palette, the pickers, the right-click and Dock
menus, confirmations, the search bar, paste receipts and notifications.

## Review

Model-drafted Spanish is a first draft. A fluent reviewer, designated by the
project owner, accepts each message in context on rendered UI. Record the
result per key in `locales/es.review.toml`, changing `draft` to `accepted`.

Confirmations and screen-reader text, listed under `[second-pass]`, need a
second pass before they count as accepted. Destructive prompts must say
exactly what closes or is lost, and screen-reader names must make sense
without the screen. Until every message is accepted, the Spanish catalogue
awaits the owner's language acceptance.

## Adding a language

1. Add `locales/xx.toml` with every key, and `locales/xx.review.toml`.
2. Extend `build.rs` and `build_support.rs` to read and validate it.
3. Add a `Language` variant and its catalogue selection in `kettle-i18n`.
4. Map its locale tags in `language_for_locale`.
5. Add a `LanguagePreference` value and config token in `kettle-config`, and a
   Settings choice named in that language.
6. Check its plural forms against the two Kettle supports.
7. Run the tests above and review it in context.
