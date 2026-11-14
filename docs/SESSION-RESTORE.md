# Session restore

Enable `restore-session = true` or launch once with `kettle --restore` to reopen
windows, tabs, splits and working directories. On macOS the default `auto`
follows the system's window-persistence setting, Linux restores, and Windows
opens fresh.
Named workspaces (`--layout NAME`) restore independently.

## When Kettle saves

Kettle writes the session while it runs, not only when it quits, so a crash,
`kill -9`, power loss or a logout that hangs loses little of it:

- Opening, closing, moving or rotating a tab, split or window, and any other
  key-bound action except scrolling, is saved at once.
- Any other change (a `cd`, a dragged divider, a renamed tab, a moved or
  resized window, new output) is saved within 2 seconds, even when nothing
  else happens afterwards.
- While output keeps arriving (a long build, an agent at work), the saved
  output is replaced at most every 30 seconds, and again within about 4
  seconds after the output stops. A layout change, or a change to the output
  settings below, saves the output with it at once.
- Quitting always saves, output included.
- An idle Kettle writes nothing and does not wake up to check. A save that
  would write the same bytes is skipped.

The tested bounds, not counting the time a write itself takes: a crash loses
at most the last 2 seconds of layout changes; of output that is still
arriving, at most the last 32 seconds (a write every 30 seconds, on the next
2-second sweep); of output that has stopped, at most the last 4 seconds.

One Kettle process owns the saved session: the first one started in restore
mode without `--profile`, `--new-process`, `-e`, `-d` or `--layout`. It holds
`session.owner.lock` beside `session.json` until it exits. Any other Kettle
process opens fresh and leaves both session files alone, whatever its settings
say; `kettle --restore` there opens fresh too, with a notification saying why.
A process started with restore mode off claims nothing, so a `kettle --restore`
beside it owns the session. A process that was already running does not take
the session over when the owner exits; the next launch owns it. After an
update, a newer build that started while the older one ran waits for that one
to exit and then saves.

`kettle --config FILE` restores and saves a session of that file's own,
never the default one: `session-<digest>.json`, with its `.content`,
`.json.lock` and `.owner.lock` beside it, in the same private directory as
`session.json` and with the same limits. The digest is of the file's
canonical path, so the names hold no part of the path, the same file reached
through a link or a relative path has one session, and two files never share
one. The same ownership rules apply to it: two processes with the same
`--config` behave as two default launches do. A `--config` naming the default
config file uses the default session. `--profile` and `--new-process` stay
separate, also when given with `--config`.

## Recent output

Recent output now survives too: the default is at most **1000 rows per pane**
from the primary screen and scrollback, with **8 MiB total** across the saved
session. Set `restore-scrollback-lines` between 0 and 10000 in config or Settings
→ Sessions. Zero disables output saving and removes the previous output file.
Rows are capped at 16 KiB including their SGR attributes; a longer row keeps its
first complete cells. The global budget favors recently focused windows, then
tabs and panes in layout order, retaining the newest rows within each pane.
The snapshot is saved as [When Kettle saves](#when-kettle-saves) describes.
It is a bounded recent snapshot, not a complete log or a running process.

Restored output keeps colors, bold, italic, underline and wide/combined text.
It appears faint, followed by **Restored · YYYY-MM-DD HH:MM:SS** in the UI language
and local time, then a fresh shell prompt. Search, selection and copy use the
ordinary terminal scrollback. Normal scrollback memory/line limits still apply,
so a smaller configured history may evict restored rows. Alternate-screen
applications contribute only their saved primary screen; their TUI contents,
images, hyperlinks and control sequences are not restored. Programs themselves
start again using the existing layout restore behavior. Saved output is never
sent as shell input.

A shell that clears the screen at startup, for example with `clear` in
`.bashrc` or `.zshrc`, no longer hides the restored rows on macOS and Linux.
Kettle treats the first full-screen clear (`ESC[2J`) sent from the top-left
corner as clearing only the rows below the divider, so the restored rows stay
on screen with the prompt under them. This covers `tput clear` and the
system `clear` on both platforms, whichever order it sends the clear-screen
and clear-scrollback sequences in. The clear-scrollback part (`ESC[3J`) still
removes restored rows that had already scrolled above the screen.

This applies once, before anything else happens in the pane. A clear
afterwards works as usual, and so does the first one if the shell has
already drawn a prompt (with shell integration), printed enough to scroll,
switched to the alternate screen, reset the terminal, or received a key
press, paste or mouse report, or if the pane was resized after the shell had
printed something. Windows is unchanged: there a startup clear still pushes
the restored rows into scrollback.

One case can lose restored text. A startup program that clears and then
draws at absolute screen positions without using the alternate screen draws
over restored rows that would otherwise have gone to scrollback. Set `restore-scrollback-lines = 0` to
turn restored output off entirely.

Remote/SSH/container output is **excluded by default**: server output may be
sensitive and should not silently be copied onto local disk. Enable
`restore-scrollback-remote = true` to include panes Kettle detects as remote.
Detection uses the current remote context, launch command and foreground process;
unrecognized remote connections cannot be excluded automatically. A pane stays
excluded after `ssh` exits: what the remote session wrote, and anything on
screen when it ended, stays out of the saved output until all of it has left
the pane, scrolled out of the scrollback limit, cleared with Clear Scrollback
once it is off screen, or reset. Output written after that is saved as usual.
Turning the option off also prevents replaying output previously marked remote.

Output lives next to `session.json` in `session.content` (or `NAME.content` next
to a named layout). Writes use owner-only directories, mode 0600 on Unix and
Kettle's private-file protection on Windows, with durable atomic replacement.
The sidecar is deleted when output/session restore is disabled or after a
successful restore; subsequent session saves can write a new snapshot. Corrupt,
oversized, unsafe or layout-mismatched files are ignored with a content-free log
message. A partial restore uses the existing layout recovery flow; output remains a
best-effort recent snapshot.

Private files still contain readable terminal text. Kettle currently tracks
neither secure keyboard entry nor password-prompt state, so it cannot exclude
those panes automatically. Use `restore-scrollback-lines = 0` before handling
sensitive output. Disabling this feature does not change separately configured
recordings or terminal logs.
