# kettle-i18n

Kettle-owned UI text in English and Spanish, as typed Rust generated at build
time. The build script reads three files in `locales/`, validates them, and
writes the catalogue into `OUT_DIR`:

- `schema.toml`: one entry per message, with translator context, and the
  argument names and types of a message that takes arguments;
- `en.toml`: the English reference text;
- `es.toml`: neutral Latin American Spanish.

`es.review.toml` records each Spanish message's review status. Translators
start with [`TRANSLATING.md`](../../TRANSLATING.md).

```rust
use kettle_i18n::{Language, Text, Translator};
let tr = Translator::new(Language::Es);
assert_eq!(tr.text(Text::SettingsCategoryTabs), "Pestañas");
assert_eq!(tr.settings_heading("Pestañas"), "Configuración — Pestañas");
```

A message without arguments is a `Text` variant, and `text` returns a
`&'static str`. A message with arguments is a method that takes them in schema
order and returns a `String`. A misspelled key, or a missing, extra or wrongly
typed argument, does not compile. There is no string-key lookup, runtime parser,
map or file access.

Pick one language per process and pass the `Translator` to the code that shows
text. Terminal content, user and shell names, config values and protocol text
never go through it. Unit symbols (`px`, `pt`, `%`, `MB`) and proper names
(themes, GPUs, graphics APIs) are not messages.

## Add a message

1. Add a snake_case entry to `locales/schema.toml`. Its `context` names the code
   that shows it and anything a translator needs: what a word refers to, its
   grammatical gender in Spanish, what an argument holds. Keep identifiers
   stable, and never reuse a config value as a label.
2. Add the same key to `locales/en.toml` and `locales/es.toml`.
3. For arguments, declare `args` in caller order. Types are `str` (`&str`) and
   `u64`. Use `{name}` in every language; Spanish may reorder them. For an
   integer plural, set `plural` to a `u64` argument and give `one` and `other`
   tables. Only 1 selects `one`.
4. Test the message in both languages where it is shown.

Only named placeholders exist. `{{` and `}}` are literal braces. Format
specifiers, positional placeholders and expressions fail the build, as do a
key missing from or extra in one language, a missing plural branch, a
placeholder set that differs between languages, a duplicate TOML key and an
unknown schema field. Catalogue text is escaped as Rust string literals; it can
never become code.

Every shipping language holds every key, which the build enforces. A missing
Spanish entry would still fall back to that message's English text, never to
its key; a unit test exercises the fallback for every message.

## Spanish

Use neutral Latin American Spanish and the working glossary in the 5.0 plan:
`panel` for pane, `pestaña` for tab, `configuración` for settings, infinitives
for actions. Model-drafted Spanish is a first draft until a fluent reviewer
accepts it.

## Choosing the language

The `language` config key (`auto`, `en`, `es`) is read once at startup.
`auto` asks the operating system for its locale with `os_locale()`, which
queries it at most once per process, and maps it with
`language_for_locale()`: a well-formed Spanish tag of any region in Latin
script (`es`, `es-MX`, `es_ES.UTF-8`, `es-419`) is Spanish; English, other
languages, `C`, `POSIX` and malformed tags are English. Parsing is bounded and
allocation-free. An explicit choice never queries the OS.

## Pseudo-locale

`Translator::pseudo()` accents English vowels, adds visible delimiters and
lengthens each message by about 38%, so a layout check can find text that does
not fit. Arguments stay verbatim. kettle-ui and kettle-render enable it in
their test builds (`dev-pseudo` as a dev-dependency feature) and run the
text-sized layouts against English, Spanish and the pseudo-locale: the
Settings panel and its hints fit a default window, the search bar keeps every
label whole and its controls apart, a paste receipt's remote warning shows
whole wherever the card has room (the compact card widens for it, up to the
expanded card it shares a lane with and the pane), and Dock titles promise no
dialog. It exists only in debug builds with the
`dev-pseudo` feature, and in this crate's unit tests; a release build has no
pseudo constructor or state, even with every feature enabled.

## Validation

```sh
cargo test -p kettle-i18n
cargo test -p kettle-i18n --release
cargo test -p kettle-i18n --features dev-pseudo
```

`tests/build_gates.rs` runs the generator against broken catalogues. The
`compile_fail` doctests in `src/lib.rs` prove that unknown keys and wrong
arguments do not compile.
