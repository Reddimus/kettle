-- ~/.config/kettle/init.lua
--
-- Example Lua script for kettle. Loaded with `kettle --lua-script
-- ~/.config/kettle/init.lua`. Demonstrates the
-- `kettle.*` namespace's most-used features. Copy what you need;
-- delete the rest.
--
-- All paths through `kettle.*` are sandboxed by default
-- (`lua-sandbox = safe` in config). The dangerous Lua stdlib
-- (os.execute, io.open, debug.*, ...) is nil'd.
--
-- That is not the same as "a plugin cannot run commands". This very
-- file runs one below, by typing it: `kettle.send_text("clear\n")`
-- reaches the shell and the newline submits the line. Safe mode
-- guards against a careless plugin touching the filesystem or
-- spawning something behind your back; it does not contain a hostile
-- one. For a plugin you have not read, set `lua-sandbox = restricted`,
-- where `send_text` and `exec_action` refuse and everything else in
-- this file still works.
--
-- The Lua sandbox's resource caps bound runaway scripts:
--   - kettle.send_text(s): s ≤ 1 MiB
--   - kettle.notify(t, b): each field ≤ 8 KiB
--   - the cumulative command queue ≤ 1024 entries
-- so a hostile or runaway script can't OOM kettle.
--
-- Bypass the sandbox with `lua-sandbox = trusted` in config when
-- you genuinely need the full Lua stdlib. Trusted mode is opt-in
-- and explicitly out of scope for the SECURITY.md Lua sandbox
-- escape policy.

-------------------------------------------------------------------------------
-- 1. Read-only introspection.
-------------------------------------------------------------------------------

print("kettle version:    " .. kettle.version())
print("kettle config:     " .. (kettle.config_path() or "<none>"))
print("kettle theme:      " .. kettle.theme())

-------------------------------------------------------------------------------
-- 2. Custom URL handlers (Terminator parity: `url_handlers.py`).
--
-- Register a pattern -> callback. When a detected, allowed pane-content
-- URL matches the pattern, the callback runs BEFORE the
-- default open-in-browser path. The callback receives the matched
-- text and must return one of:
--   - a string URL (kettle opens it via the OS / custom handler)
--   - true (the handler opened it itself; kettle opens nothing)
--   - nil or false (decline; kettle falls through to the next
--     handler and then to its own default open)
--
-- A handler that raises is treated as a decline and logged: a handler
-- exists to enhance a link, so a broken one must never be the reason
-- a link stops working.
--
-- These recipes upgrade detectable HTTP links to HTTPS.
-- Registration does not extend URL detection or the scheme allowlist.
-- Kettle skips a pattern whose worst-case matching work is too high, and
-- counts each `+`, `*` and `-` as a repeat, so keep those few.
-------------------------------------------------------------------------------

-- Launchpad bug links, such as http://bugs.launchpad.net/bugs/12345.
kettle.add_url_handler(
    "launchpad_bug",
    "^http://bugs%.launchpad%.net/bugs/%d+$",
    function(text)
        return (text:gsub("^http:", "https:"))
    end
)

-- Launchpad branch links, such as http://code.launchpad.net/+branch/terminator/trunk.
kettle.add_url_handler(
    "launchpad_code",
    "^http://code%.launchpad%.net/.branch/[%w%p]+$",
    function(text)
        return (text:gsub("^http:", "https:"))
    end
)

-- Ubuntu package links, such as http://packages.ubuntu.com/gimp.
kettle.add_url_handler(
    "ubuntu_package",
    "^http://packages%.ubuntu%.com/[%w%p]+$",
    function(text)
        return (text:gsub("^http:", "https:"))
    end
)

-------------------------------------------------------------------------------
-- 3. Event hooks (Terminator parity: `activitywatch.py` etc).
--
-- `kettle.on(event, fn)` fires the callback when the named event
-- happens. Supported events:
--   - "startup"          → fires once after the first window paint
--   - "tab_add"          → tab index appended; payload = new index
--   - "tab_close"        → tab being closed; payload = closing index
--   - "bell"             → bell rang in a pane; payload = pane id
--   - "pane_close"        → a pane ended (closed, with its tab or window,
--                          or its shell exited); payload = pane id
--   - "output"           → pane produced output; payload = (pane id,
--                          bytes since last emission). Throttled at
--                          the dispatch site so a busy build doesn't
--                          flood the callback.
--   - "pane_focus"       → focus changed; payload = (old id or nil, new id)
--   - "title_changed"    → pane title changed; payload = (pane id, title)
--   - "url_clicked"      → URL activation; payload = URL string
-------------------------------------------------------------------------------

kettle.on("startup", function()
    -- One-time greet on first pane spawn. `kettle.notify(title, body)`
    -- routes through notify-rust + libnotify on Linux / NotificationCenter
    -- on macOS / Windows toast on Windows.
    kettle.notify("kettle ready", "Loaded " .. kettle.theme() .. " theme.")
end)

-- Example: trigger a desktop notification when a long-running build
-- prints its final line. Replace the pattern with whatever your
-- build system emits.
local last_notify = 0
local partial_lines = {}
local announced = {}
local function notify_build(pane_id, now)
    -- Coalescing — don't spam notifications, but keep collecting output.
    if now - last_notify >= 5 then
        last_notify = now
        kettle.notify("kettle", "Build done in pane " .. tostring(pane_id))
    end
end
kettle.on("pane_close", function(pane_id)
    partial_lines[pane_id] = nil
    announced[pane_id] = nil
end)
kettle.on("output", function(pane_id, bytes)
    local now = os.time()
    -- `bytes` is a table of byte values; this makes them one string.
    local text = (partial_lines[pane_id] or "") .. string.char(table.unpack(bytes))
    local first = 1
    while true do
        local last = text:find("[\r\n]", first)
        if not last then break end
        local line = text:sub(first, last - 1)
        if not announced[pane_id] and
            (line:find("BUILD SUCCESSFUL") or line:find("Compiling%s*$")) then
            notify_build(pane_id, now)
        end
        announced[pane_id] = nil
        first = last + 1
    end
    local tail = text:sub(first)
    -- A build's last line may never get a newline, so the success message
    -- also counts while its line is still open, once.
    if not announced[pane_id] and tail:find("BUILD SUCCESSFUL") then
        notify_build(pane_id, now)
        announced[pane_id] = true
    end
    -- Bound retained output even when a program never ends its line.
    partial_lines[pane_id] = tail:sub(math.max(1, #tail - 4095))
end)

-------------------------------------------------------------------------------
-- 4. Custom menu items (Terminator parity: `custom_commands.py`).
--
-- `kettle.add_menu_item(label, callback)` appends a right-click
-- menu entry. The callback receives no arguments and runs when
-- the user clicks the entry.
-------------------------------------------------------------------------------

kettle.add_menu_item("Send `clear`", function()
    -- send_text takes UTF-8 text; "\n" submits the line.
    kettle.send_text("clear\n")
end)

kettle.add_menu_item("Switch to TokyoNight Day", function()
    kettle.set_theme("TokyoNight Day")
end)

-- Trigger any kettle Action by its name (the same names you'd use
-- in `keybind = trigger = ACTION_NAME` in config).
kettle.add_menu_item("New window", function()
    kettle.exec_action("new_window")
end)

-- Send the focused pane's title or open its cwd
-- in the file manager.
kettle.add_menu_item("Insert pane name", function()
    kettle.exec_action("insert_pane_name")
end)
kettle.add_menu_item("Open cwd in files", function()
    kettle.exec_action("open_cwd")
end)
