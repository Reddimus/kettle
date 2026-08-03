-- ~/.config/kettle/init.lua
--
-- Example Lua script for kettle. Loaded with `kettle --lua-script
-- ~/.config/kettle/init.lua`. Demonstrates the
-- `kettle.*` namespace's most-used features. Copy what you need;
-- delete the rest.
--
-- All paths through `kettle.*` are sandboxed by default
-- (`lua-sandbox = safe` in config). The dangerous Lua stdlib
-- (os.execute, io.open, debug.*, ...) is nil'd. The Lua sandbox's
-- resource caps bound runaway scripts:
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
-- Register a pattern → callback. When kettle's pane-content URL
-- detector matches the pattern, the callback runs BEFORE the
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
-- The Launchpad / APT examples below port the Terminator
-- `url_handlers.py` defaults so a kettle user can opt into the
-- same Ubuntu-flavored URL recognition.
-------------------------------------------------------------------------------

-- Launchpad bug references: `LP: #12345` → bugs.launchpad.net.
kettle.add_url_handler(
    "launchpad_bug",
    "%f[%w][lL][pP]:?%s?#?(%d+)",
    function(text)
        local num = text:match("(%d+)")
        if num then
            return "https://bugs.launchpad.net/bugs/" .. num
        end
    end
)

-- Launchpad branch refs: `lp:terminator/trunk` → code.launchpad.net.
kettle.add_url_handler(
    "launchpad_code",
    "%f[%w][lL][pP]:[a-z0-9][a-z0-9%+.%-/]+",
    function(text)
        local branch = text:gsub("^[lL][pP]:", "")
        return "https://code.launchpad.net/+branch/" .. branch
    end
)

-- APT URLs: `apt://gimp` → trigger `apt://` system handler. The
-- system OS opener handles `apt:` schemes natively on Ubuntu /
-- Debian.
kettle.add_url_handler(
    "apt",
    "apt:[^%s]+",
    function(text)
        return text
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
--   - "pane_close"        → pane about to close; payload = pane id
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
kettle.on("output", function(pane_id, bytes)
    -- Coalescing — don't spam notifications.
    local now = os.time()  -- only available in `lua-sandbox = trusted`;
                           -- safe-mode users would store last-notify in
                           -- a counter incremented per output instead.
    if now - last_notify < 5 then return end
    local text = string.char(table.unpack(bytes))
    if text:find("BUILD SUCCESSFUL") or text:find("Compiling%s*$") then
        last_notify = now
        kettle.notify("kettle", "Build done in pane " .. tostring(pane_id))
    end
end)

-------------------------------------------------------------------------------
-- 4. Custom menu items (Terminator parity: `custom_commands.py`).
--
-- `kettle.add_menu_item(label, callback)` appends a right-click
-- menu entry. The callback receives no arguments and runs when
-- the user clicks the entry.
-------------------------------------------------------------------------------

kettle.add_menu_item("Send `clear`", function()
    -- send_text accepts arbitrary bytes; "\n" submits the line.
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
