# Lua plugins

Kettle runs one Lua script: `<config-dir>/init.lua` when it exists, or the
file `--lua-script FILE` names. There is no plugin directory.
[`examples/init.lua`](examples/init.lua) shows every function and event.

A script found automatically must pass the same ownership and permission
checks as the config file. Naming a script with `--lua-script` trusts that
file. Kettle reads at most 4 MiB of it.

## Functions

Text arguments must be UTF-8.

| Function | What it does |
| --- | --- |
| `kettle.version()` | Returns Kettle's version. |
| `kettle.config_path()` | Returns the config file in use, or nil. |
| `kettle.theme()` | Returns the theme in effect. |
| `kettle.send_text(text)` | Types `text` into the focused pane; a newline runs a command. |
| `kettle.exec_action(name)` | Runs an action by its keybind name, such as `new_tab`. |
| `kettle.notify(title, body)` | Shows a desktop notification. |
| `kettle.set_theme(name)` | Switches the theme. |
| `kettle.on(event, callback)` | Calls `callback` on each `event` (below). |
| `kettle.add_menu_item(label, callback)` | Adds a right-click menu row; `callback` takes no arguments. |
| `kettle.add_url_handler(name, pattern, callback)` | Offers URLs that match `pattern` to `callback`. |

`send_text`, `exec_action`, `notify` and `set_theme` return `true` when queued,
or `false` when a limit or sandbox policy refuses them (and log why). Effects
happen after the script returns: an unknown theme can queue successfully and
fail later. `kettle.theme()` changes only after the theme is applied.

## Events

| Event | Callback arguments |
| --- | --- |
| `startup` | none; the first window and pane exist |
| `tab_add` | the new tab's index, from 0 |
| `tab_close` | the closing tab's index, from 0 |
| `bell` | pane id |
| `pane_close` | pane id, for every pane that ends: an explicit close fires it before the process is stopped; a pane that goes with its tab or window, or whose shell exited, fires it as it is removed |
| `output` | pane id, the output as a table of byte values |
| `pane_focus` | previous pane id or nil, new pane id |
| `title_changed` | pane id, new title |
| `url_clicked` | the URL, before URL handlers run |

`string.char(table.unpack(bytes))` turns an `output` table into a string.
Output reaches callbacks in bounded batches and some can be dropped under
load, so do not use it as a complete log. Callbacks run on the UI thread:
keep them short.

## URL handlers

Handlers run in the order they were added. `pattern` is a Lua pattern for
`string.match`, not a regular expression. The callback gets the whole URL
and returns:

- a non-empty string: open that URL instead (Kettle checks it first);
- `true`: the handler opened it, so Kettle does nothing;
- anything else, or an error: the next handler gets the URL.

## Limits

| What | Limit |
| --- | --- |
| One `send_text` | 1 MiB |
| Queued commands | 1,024, and 8 MiB of queued text |
| Notification title and body | 8 KiB each |
| Action and theme names | 256 bytes |
| Callbacks per event | 256 |
| Menu rows | 256, labels up to 1 KiB |
| URL handlers | 256, names up to 256 bytes, patterns up to 4 KiB |
| Pattern matching for one URL | about 2 million steps across all handlers |
| `string.find`, `match`, `gmatch`, `gsub` in safe/restricted modes | 20 million matcher steps per call (shared across iterations) |
| Lua memory | 256 MiB |

A callback that runs for about 128 million Lua instructions is stopped.
An event callback stopped that way is removed until Kettle restarts; one
that raises an error is logged and kept.

## Sandbox

`lua-sandbox` in the config sets what a script may do:

- `safe` (the default) removes the Lua functions that run programs or touch
  files: `os.execute`, `os.remove`, `os.rename`, `io.open`, `io.popen`,
  `io.tmpfile` (raises an error naming the sandbox mode),
  `loadfile`, `dofile` and the like. `os.time` and `os.date` stay.
  `require` loads only preloaded modules and modules at absolute paths.
- `restricted` is `safe`, and `send_text` and `exec_action` return false.
- `trusted` restores the command and file functions.

No level allows the `debug` library or native C modules. `safe` still lets
a script type commands into a pane, so read a plugin before you use it.
