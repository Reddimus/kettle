# Session restore

Enable `restore-session = true` or launch once with `kettle --restore` to reopen
windows, tabs, splits and working directories. On macOS the default `auto`
follows the system's window-persistence setting, Linux restores, and Windows
opens fresh.
Named workspaces (`--layout NAME`) restore independently.

Recent output now survives too: the default is at most **1000 rows per pane**
from the primary screen and scrollback, with **8 MiB total** across the saved
session. Set `restore-scrollback-lines` between 0 and 10000 in config or Settings
→ Sessions. Zero disables output saving and removes the previous output file.
Rows are capped at 16 KiB including their SGR attributes; a longer row keeps its
first complete cells. The global budget favors recently focused windows, then
tabs and panes in layout order, retaining the newest rows within each pane.
The snapshot is taken whenever restore mode saves the session, including on
quit. It is a bounded recent snapshot, not a complete log or a running process.

Restored output keeps colors, bold, italic, underline and wide/combined text.
It appears faint, followed by **Restored · YYYY-MM-DD HH:MM:SS** in the UI language
and local time, then a fresh shell prompt. Search, selection and copy use the
ordinary terminal scrollback. Normal scrollback memory/line limits still apply,
so a smaller configured history may evict restored rows. Alternate-screen
applications contribute only their saved primary screen; their TUI contents,
images, hyperlinks and control sequences are not restored. Programs themselves
start again using the existing layout restore behavior. Saved output is never
sent as shell input.

Remote/SSH/container output is **excluded by default**: server output may be
sensitive and should not silently be copied onto local disk. Enable
`restore-scrollback-remote = true` to include panes Kettle detects as remote.
Detection uses the current remote context, launch command and foreground process;
unrecognized remote connections cannot be excluded automatically. Turning the
option off also prevents replaying output previously marked remote.

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
