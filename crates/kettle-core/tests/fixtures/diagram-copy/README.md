# Copied and selected diagram corpus

These fixtures come from the S6 spike (2026-09-30), which ran Claude Code
2.1.285 and Codex CLI 0.159.0 in Kettle at 60, 70 and 80 columns, asked each
for the same Mermaid flowchart, and captured what each put on screen and on
the clipboard. The diagrams and the paths they name are synthetic; no user
data is in them.

Each run's directory (`cc*` for Claude Code, `cx*` for Codex; `multi` runs
asked for two diagrams, `classic` and `full` are Claude's screen modes,
`inline` is Codex without the alternate screen) holds:

- `NN-name.message.txt`: the whole reply, as a selection would take it;
- `NN-name.tight.txt`: just the diagram rows, from its label, notice or
  header;
- `NN-name.boxart.txt`: a reply Codex drew as box art, which no source can
  be taken from;
- `NN-clip.clipboard.txt`: what `/copy` put on the clipboard;
- `source-N.mmd`: the diagram the model wrote, from its own transcript.

`cases.json` lists every case with the pane width it was wrapped at (none
for a clipboard) and what the prototype normalizer that settled the rules
gave it: the source files it matched, or `null` for a refusal. That
prototype matched the source in all 11 clipboard cases and 82 selections at
their width, and refused the 8 selections whose wraps could not be told from
statements and the 7 box-art replies. `tests/diagram_copy.rs` holds
`kettle_core::diagram_sources` to the same outcomes.
