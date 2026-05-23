//! Cycle 324: Lua scripting foundation (WezTerm parity).
//!
//! Exposes a `kettle` namespace inside a Lua VM so the user's
//! `--lua-script PATH` (or future `<config-dir>/init.lua`) can read
//! kettle's runtime state. Foundation sub-cycle ships read-only
//! introspection; subsequent sub-cycles add side-effect APIs:
//!
//!   cycle 324 (this one): kettle.version() / config_path() / theme()
//!   cycle 325+:           kettle.send_text(s), set_tab_title(s)
//!   cycle 326+:           kettle.exec_action(name)
//!   cycle 365+:           kettle.on(event, callback) event hooks
//!                         (foundation; see docs/TERMINATOR-PLUGIN-DESIGN.md
//!                         for the full sub-cycle roadmap)
//!
//! Why read-only first: hooking Lua into the live App requires
//! threading an Arc<Mutex<...>> handle through, which is the kind
//! of plumbing that's easier to verify in isolation. Foundation
//! cycle ships the dep + the VM + the namespace + a drift guard;
//! the side-effect APIs add incrementally without re-touching
//! the wiring.

use anyhow::{Context, Result};
use mlua::Lua;
use std::path::Path;
use std::sync::{Arc, Mutex};

/// Cycle 325: side-effect commands buffered from Lua. The Lua VM
/// can't directly mutate App state (lifetime + threading), so
/// side-effect APIs (send_text, set_tab_title, notify, ...) push
/// onto this queue and the App drains it after the script
/// returns. Same shape as the cycle-302 remote-control IPC's
/// line-buffer, just in-process.
#[derive(Debug, Clone)]
pub enum LuaCommand {
    /// `kettle.send_text(s)` → write s to the focused pane's PTY.
    SendText(String),
    /// `kettle.exec_action(name)` → dispatch a named kettle action
    /// (parsed via `Action::from_name`). The name is whatever the
    /// keybind grammar accepts: `new_tab`, `split_right`,
    /// `toggle_vi_mode`, etc.
    ExecAction(String),
    /// `kettle.notify(title, body)` (cycle 371) → desktop notification
    /// via notify-rust. Fires once kettle drains commands so a script
    /// running before the first paint doesn't race the notification
    /// daemon.
    Notify { title: String, body: String },
    /// `kettle.set_theme(name)` (cycle 373) → switch the active theme
    /// at runtime. Looked up case-insensitively against the ~500
    /// bundled themes via Theme::find_name; falls through with
    /// log::warn if no match.
    SetTheme(String),
}

/// Cycle 365 (Terminator plugin parity, design doc:
/// docs/TERMINATOR-PLUGIN-DESIGN.md): event hooks. Foundation sub-cycle
/// ships the registry + dispatch surface; subsequent sub-cycles wire
/// each variant to the actual emission site in App.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum LuaEvent {
    /// Emitted once after kettle's first window + first pane are
    /// alive. Use for one-time init: set theme, apply argv-style
    /// modifications.
    Startup,
    /// Emitted on each tab insertion. Payload: tab index.
    TabAdd(usize),
    /// Emitted on each tab close. Payload: tab index that was closed.
    TabClose(usize),
    /// Emitted when a bell rings in a pane. Payload: pane id.
    Bell(u64),
    /// Cycle 377 (Terminator plugin parity, plugin sub-cycle 3):
    /// emitted on each batch of PTY output drained from a pane.
    /// Payload: pane id + bytes since last emission. Throttled
    /// at the dispatch site (App level) — Lua callbacks see a
    /// coalesced byte chunk, not every individual chunk from a
    /// busy build.
    Output(u64, Vec<u8>),
}

impl LuaEvent {
    /// String name used by user scripts: `kettle.on('startup', ...)`,
    /// `kettle.on('tab_add', ...)`, etc.
    pub fn name(&self) -> &'static str {
        match self {
            LuaEvent::Startup => "startup",
            LuaEvent::TabAdd(_) => "tab_add",
            LuaEvent::TabClose(_) => "tab_close",
            LuaEvent::Bell(_) => "bell",
            LuaEvent::Output(_, _) => "output",
        }
    }
}

/// Owned Lua VM with kettle's namespace registered. Single-threaded
/// today (Lua VMs aren't natively reentrant); future cycles may
/// wrap in `Arc<Mutex<...>>` to allow event-hook callbacks from
/// the App's threads.
pub struct LuaEngine {
    lua: Lua,
    /// Side-effect commands queued by Lua functions. Drained by
    /// the App after exec_file returns.
    pending: Arc<Mutex<Vec<LuaCommand>>>,
}

impl LuaEngine {
    /// Construct a fresh Lua VM with the cycle-324 read-only
    /// `kettle` namespace installed:
    ///
    ///   kettle.version()      → string  e.g. "1.7.8"
    ///   kettle.config_path()  → string|nil  the resolved config path
    ///   kettle.theme()        → string  e.g. "TokyoNight Night"
    ///
    /// Fails if the Lua VM can't initialize (resource exhaustion,
    /// not normally seen). Adding entries to the namespace is the
    /// happy path for subsequent sub-cycles — extend this function.
    pub fn new(theme_name: &str) -> Result<Self> {
        Self::new_with_sandbox(theme_name, true)
    }

    /// Cycle 376 (Terminator plugin parity, plugin sub-cycle 12):
    /// build a VM with the configured sandbox level. `safe = true`
    /// nil's the Lua stdlib functions that can execute external
    /// processes or open arbitrary files: `os.execute`, `os.exit`,
    /// `os.remove`, `os.rename`, `io.open`, `io.popen`,
    /// `io.lines`, `io.input`, `io.output`, `package.loadlib`,
    /// `loadfile`, `dofile`. The rest of the stdlib (string, table,
    /// math, os.date/os.time/os.getenv/os.difftime, io.read, ...)
    /// stays usable.
    ///
    /// Errors from setting these to nil are bubbled up — a Lua VM
    /// where the standard globals can't be removed isn't safe to
    /// proceed with.
    pub fn new_with_sandbox(theme_name: &str, safe: bool) -> Result<Self> {
        let lua = Lua::new();
        if safe {
            // Block dangerous APIs. Setting to nil is the canonical
            // sandbox pattern in mlua / WezTerm / Neovim plugins.
            let globals = lua.globals();
            if let Ok(os_tbl) = globals.get::<mlua::Table>("os") {
                for k in [
                    "execute",
                    "exit",
                    "remove",
                    "rename",
                    "tmpname",
                    "setlocale",
                ] {
                    let _ = os_tbl.set(k, mlua::Value::Nil);
                }
            }
            if let Ok(io_tbl) = globals.get::<mlua::Table>("io") {
                for k in [
                    "open", "popen", "lines", "input", "output", "stdin", "stdout", "stderr",
                ] {
                    let _ = io_tbl.set(k, mlua::Value::Nil);
                }
            }
            // loadfile / dofile read arbitrary files; deny.
            let _ = globals.set("loadfile", mlua::Value::Nil);
            let _ = globals.set("dofile", mlua::Value::Nil);
            // package.loadlib loads native shared libraries → can
            // execute arbitrary code. Always nil in safe mode.
            if let Ok(pkg) = globals.get::<mlua::Table>("package") {
                let _ = pkg.set("loadlib", mlua::Value::Nil);
            }
            // NOTE on `debug.*` (audited cycle 591): mlua's `Lua::new()`
            // loads `StdLib::ALL_SAFE`, which EXCLUDES the `debug`
            // library entirely. The dangerous methods —
            // `debug.getregistry` (reach into mlua's reference table),
            // `debug.sethook` (DoS via instruction hooks),
            // `debug.set{metatable,local,upvalue}` (break opaque
            // userdata) — are already unreachable from kettle's Lua
            // VM. No explicit nil-sweep needed here. The
            // `safe_sandbox_pins_mlua_default_excludes_debug` drift
            // guard pins this so a future refactor that switches to
            // `Lua::unsafe_new()` (or explicitly loads `StdLib::DEBUG`)
            // fails the gauntlet rather than silently widening the
            // surface.
        }
        let pending: Arc<Mutex<Vec<LuaCommand>>> = Arc::new(Mutex::new(Vec::new()));
        let kettle_tbl = lua.create_table().context("create kettle table")?;
        // Expose values as callable functions (not bare strings) so
        // user scripts use the conventional `kettle.version()` form.
        // Callable form also makes it easy to add side effects later
        // (e.g. cycle-325's send_text needs to be a function anyway).
        let version: &str = env!("CARGO_PKG_VERSION");
        kettle_tbl
            .set(
                "version",
                lua.create_function(move |_, ()| Ok(version.to_string()))?,
            )
            .context("set kettle.version")?;
        let cfg_path: Option<String> =
            kettle_config::Config::default_path().map(|p| p.display().to_string());
        kettle_tbl
            .set(
                "config_path",
                lua.create_function(move |_, ()| Ok(cfg_path.clone()))?,
            )
            .context("set kettle.config_path")?;
        let theme = theme_name.to_string();
        kettle_tbl
            .set(
                "theme",
                lua.create_function(move |_, ()| Ok(theme.clone()))?,
            )
            .context("set kettle.theme")?;
        // Cycle 325: side-effect API. `kettle.send_text(s)` queues
        // a SendText command for the App to drain + write to the
        // focused pane's PTY. Lua-side it looks synchronous, but
        // the actual PTY write happens once the script returns —
        // a kettle script can't observe its own typing.
        let pending_for_send = Arc::clone(&pending);
        kettle_tbl
            .set(
                "send_text",
                lua.create_function(move |_, s: String| {
                    pending_for_send
                        .lock()
                        .map(|mut v| v.push(LuaCommand::SendText(s)))
                        .map_err(|e| mlua::Error::external(format!("pending mutex: {e}")))?;
                    Ok(())
                })?,
            )
            .context("set kettle.send_text")?;
        // Cycle 326: kettle.exec_action(name) dispatches a kettle
        // Action by its keybind-grammar name. Lua scripts get the
        // same dispatch power as the keymap — `new_tab`,
        // `split_right`, `toggle_vi_mode`, etc.
        let pending_for_action = Arc::clone(&pending);
        kettle_tbl
            .set(
                "exec_action",
                lua.create_function(move |_, name: String| {
                    pending_for_action
                        .lock()
                        .map(|mut v| v.push(LuaCommand::ExecAction(name)))
                        .map_err(|e| mlua::Error::external(format!("pending mutex: {e}")))?;
                    Ok(())
                })?,
            )
            .context("set kettle.exec_action")?;
        // Cycle 371 (plugin sub-cycle 7): kettle.notify(title, body?)
        // queues a desktop notification. Body is optional;
        // `kettle.notify('Build done')` works too.
        let pending_for_notify = Arc::clone(&pending);
        kettle_tbl
            .set(
                "notify",
                lua.create_function(move |_, args: mlua::Variadic<String>| {
                    let mut iter = args.into_iter();
                    let title = iter.next().unwrap_or_default();
                    let body = iter.next().unwrap_or_default();
                    pending_for_notify
                        .lock()
                        .map(|mut v| v.push(LuaCommand::Notify { title, body }))
                        .map_err(|e| mlua::Error::external(format!("pending mutex: {e}")))?;
                    Ok(())
                })?,
            )
            .context("set kettle.notify")?;
        // Cycle 373 (plugin sub-cycle 10): kettle.set_theme(name)
        // queues a SetTheme command; the App drains + applies via
        // the existing NextTheme infrastructure.
        let pending_for_theme = Arc::clone(&pending);
        kettle_tbl
            .set(
                "set_theme",
                lua.create_function(move |_, name: String| {
                    pending_for_theme
                        .lock()
                        .map(|mut v| v.push(LuaCommand::SetTheme(name)))
                        .map_err(|e| mlua::Error::external(format!("pending mutex: {e}")))?;
                    Ok(())
                })?,
            )
            .context("set kettle.set_theme")?;
        // Cycle 375 (plugin sub-cycle 8): kettle.add_menu_item(
        //   label, callback). Lua-supplied entries that render BELOW
        // the built-in context-menu items (cycle-245). Callback is
        // a Lua function invoked when the item is clicked.
        //
        // Storage: kettle_menu_items registry table; each entry is
        // a {label, callback} table.
        let menu_items_tbl = lua
            .create_table()
            .context("create kettle_menu_items table")?;
        lua.set_named_registry_value("kettle_menu_items", menu_items_tbl)
            .context("register kettle_menu_items")?;
        kettle_tbl
            .set(
                "add_menu_item",
                lua.create_function(|lua, (label, cb): (String, mlua::Function)| {
                    let items: mlua::Table = lua.named_registry_value("kettle_menu_items")?;
                    let entry = lua.create_table()?;
                    entry.set("label", label)?;
                    entry.set("callback", cb)?;
                    let n = items.len()?;
                    items.set(n + 1, entry)?;
                    Ok(())
                })?,
            )
            .context("set kettle.add_menu_item")?;
        // Cycle 374 (plugin sub-cycle 9): kettle.add_url_handler(
        //   name, pattern, callback). Lua-supplied URL handlers that
        // run when a URL matches the pattern, BEFORE kettle's default
        // open-in-browser path. Use case: route Launchpad/GitHub PR
        // URLs to a custom CLI tool instead of the system browser.
        //
        // Storage: same `kettle_events` registry table used by
        // kettle.on; just a different key (`url_handlers`) that
        // holds a list of {name, pattern, callback} tables.
        let url_handlers_tbl = lua
            .create_table()
            .context("create kettle_url_handlers table")?;
        lua.set_named_registry_value("kettle_url_handlers", url_handlers_tbl)
            .context("register kettle_url_handlers")?;
        kettle_tbl
            .set(
                "add_url_handler",
                lua.create_function(
                    |lua, (name, pattern, cb): (String, String, mlua::Function)| {
                        let handlers: mlua::Table =
                            lua.named_registry_value("kettle_url_handlers")?;
                        let entry = lua.create_table()?;
                        entry.set("name", name)?;
                        entry.set("pattern", pattern)?;
                        entry.set("callback", cb)?;
                        let n = handlers.len()?;
                        handlers.set(n + 1, entry)?;
                        Ok(())
                    },
                )?,
            )
            .context("set kettle.add_url_handler")?;
        // Cycle 365 (Terminator plugin parity foundation):
        // `kettle.on(event_name, callback)` registers a Lua function to
        // fire when the named event occurs. Stored as a registry-table
        // entry keyed by event name; callbacks accumulate in a list
        // (multiple subscribers per event).
        //
        // Today's wiring: registry installed + drift-guarded. App-side
        // emission per LuaEvent variant lands in subsequent sub-cycles
        // (see docs/TERMINATOR-PLUGIN-DESIGN.md sub-cycle 3+).
        let event_table = lua.create_table().context("create event-hooks table")?;
        lua.set_named_registry_value("kettle_events", event_table)
            .context("register kettle_events table")?;
        kettle_tbl
            .set(
                "on",
                lua.create_function(|lua, (name, cb): (String, mlua::Function)| {
                    let events: mlua::Table = lua.named_registry_value("kettle_events")?;
                    let list: mlua::Table = match events.get::<mlua::Value>(name.clone())? {
                        mlua::Value::Table(t) => t,
                        _ => {
                            let t = lua.create_table()?;
                            events.set(name.clone(), t.clone())?;
                            t
                        }
                    };
                    let n = list.len()?;
                    list.set(n + 1, cb)?;
                    Ok(())
                })?,
            )
            .context("set kettle.on")?;
        lua.globals()
            .set("kettle", kettle_tbl)
            .context("install kettle namespace")?;
        Ok(Self { lua, pending })
    }

    /// Cycle 375: list the labels of every Lua-registered context
    /// menu item, in registration order. Used by App to extend the
    /// cycle-245 context menu with kettle.add_menu_item entries.
    pub fn list_menu_item_labels(&self) -> mlua::Result<Vec<String>> {
        let items: mlua::Table = self.lua.named_registry_value("kettle_menu_items")?;
        let n = items.len()?;
        let mut out = Vec::with_capacity(n as usize);
        for i in 1..=n {
            let entry: mlua::Table = items.get(i)?;
            let label: String = entry.get("label")?;
            out.push(label);
        }
        Ok(out)
    }

    /// Cycle 375: invoke the Lua callback for menu-item index
    /// `idx` (0-based; the App walks the registered list in the
    /// same order `list_menu_item_labels` returned). Errors
    /// log::warn + don't propagate.
    pub fn invoke_menu_item(&self, idx: usize) {
        let result: mlua::Result<()> = (|| {
            let items: mlua::Table = self.lua.named_registry_value("kettle_menu_items")?;
            let entry: mlua::Table = items.get(idx + 1)?;
            let cb: mlua::Function = entry.get("callback")?;
            let r: mlua::Result<()> = cb.call(());
            if let Err(e) = r {
                log::warn!("lua menu-item {idx} callback: {e}");
            }
            Ok(())
        })();
        if let Err(e) = result {
            log::warn!("lua invoke_menu_item({idx}): {e}");
        }
    }

    /// Cycle 374: invoke the first registered URL handler whose
    /// pattern matches the given URL. Returns true when a handler
    /// claimed the URL (kettle should NOT also open it); false
    /// otherwise (kettle continues to its default open-in-browser
    /// path).
    ///
    /// Uses Lua's built-in `string.match` for pattern compatibility
    /// with Terminator's URLHandler regex semantics (which are
    /// Python-flavored, but Lua patterns are similar enough for
    /// the common URL shapes — alternation isn't supported but most
    /// URL handlers don't need it).
    pub fn try_url_handler(&self, url: &str) -> bool {
        let r: mlua::Result<bool> = (|| {
            let handlers: mlua::Table = self.lua.named_registry_value("kettle_url_handlers")?;
            let n = handlers.len()?;
            for i in 1..=n {
                let entry: mlua::Table = handlers.get(i)?;
                let pattern: String = entry.get("pattern")?;
                // Use Lua's string.match for compat with user-typed
                // patterns. If it returns non-nil, the handler matches.
                let s: mlua::Function = self
                    .lua
                    .globals()
                    .get::<mlua::Table>("string")?
                    .get("match")?;
                let m: mlua::Value = s.call((url, pattern.as_str()))?;
                if !matches!(m, mlua::Value::Nil) {
                    let cb: mlua::Function = entry.get("callback")?;
                    let call_result: mlua::Result<()> = cb.call(url.to_string());
                    if let Err(e) = call_result {
                        log::warn!("lua url_handler callback {i}: {e}");
                    }
                    return Ok(true);
                }
            }
            Ok(false)
        })();
        r.unwrap_or_else(|e| {
            log::warn!("lua try_url_handler: {e}");
            false
        })
    }

    /// Cycle 365: fire a named event to every Lua callback registered
    /// for it. Args are converted from `&str` for simplicity (every
    /// current event payload fits as a single string; future events
    /// can extend with a richer arg type).
    ///
    /// Errors from individual callbacks log::warn but DON'T abort
    /// kettle — one broken plugin can't take down the terminal.
    pub fn fire_event(&self, event: &LuaEvent) {
        let result: mlua::Result<()> = (|| {
            let events: mlua::Table = self.lua.named_registry_value("kettle_events")?;
            let name = event.name();
            let list: mlua::Value = events.get(name)?;
            if let mlua::Value::Table(callbacks) = list {
                let n = callbacks.len()?;
                for i in 1..=n {
                    let cb: mlua::Function = callbacks.get(i)?;
                    let call_result: mlua::Result<()> = match event {
                        LuaEvent::Startup => cb.call(()),
                        LuaEvent::TabAdd(idx) | LuaEvent::TabClose(idx) => cb.call(*idx),
                        LuaEvent::Bell(pane_id) => cb.call(*pane_id),
                        LuaEvent::Output(pane_id, bytes) => {
                            // Send bytes as a Lua string (UTF-8 not
                            // assumed — raw bytes are fine, callbacks
                            // can string.byte / string.sub them).
                            cb.call((*pane_id, bytes.as_slice()))
                        }
                    };
                    if let Err(e) = call_result {
                        log::warn!("lua event {name} callback {i}: {e}");
                    }
                }
            }
            Ok(())
        })();
        if let Err(e) = result {
            log::warn!("lua fire_event({:?}): {e}", event.name());
        }
    }

    /// Cycle 325: drain pending side-effect commands queued by Lua
    /// during the most recent script execution. Returns whatever
    /// the script accumulated; the buffer is reset.
    pub fn drain_commands(&self) -> Vec<LuaCommand> {
        self.pending
            .lock()
            .map(|mut v| std::mem::take(&mut *v))
            .unwrap_or_default()
    }

    /// Run the contents of a Lua file. Errors bubble up via anyhow
    /// with the script path attached so the user sees which file
    /// failed.
    ///
    /// Cycle 587: bound the read at 4 MiB. Real init.lua files run
    /// a few KB to ~100 KB; pulling in a moderately complex plugin
    /// suite might reach a few hundred KB. 4 MiB is a generous
    /// margin while still catching a swap-attack blob (same
    /// defense-in-depth pattern as cycle 585 session.json + cycle
    /// 586 config). Past the cap the script is refused with an
    /// `anyhow` error so the user gets a clear diagnostic rather
    /// than an OOM mid-load.
    pub fn exec_file(&self, path: &Path) -> Result<()> {
        const MAX_LUA_SCRIPT_BYTES: u64 = 4 * 1024 * 1024;
        let size = std::fs::metadata(path)
            .with_context(|| format!("stat lua script {}", path.display()))?
            .len();
        if size > MAX_LUA_SCRIPT_BYTES {
            anyhow::bail!(
                "lua script {} is {size} bytes (cap {MAX_LUA_SCRIPT_BYTES}); refusing to load",
                path.display()
            );
        }
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("read lua script {}", path.display()))?;
        self.lua
            .load(&text)
            .set_name(path.display().to_string())
            .exec()
            .with_context(|| format!("execute lua script {}", path.display()))?;
        Ok(())
    }

    /// Evaluate a Lua expression and return its result as a string.
    /// Used by drift guards + by future `kettle eval` introspection.
    /// Returns the empty string if the expression evaluates to
    /// `nil`.
    pub fn eval_str(&self, expr: &str) -> Result<String> {
        let v: mlua::Value = self
            .lua
            .load(expr)
            .eval()
            .with_context(|| format!("eval lua expr {expr:?}"))?;
        match v {
            mlua::Value::Nil => Ok(String::new()),
            mlua::Value::String(s) => Ok(s.to_str()?.to_string()),
            mlua::Value::Integer(n) => Ok(n.to_string()),
            mlua::Value::Number(n) => Ok(n.to_string()),
            mlua::Value::Boolean(b) => Ok(b.to_string()),
            // Other types (tables, functions) round-trip as their
            // Lua repr; not perfect but useful for sanity checks.
            other => Ok(format!("{other:?}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kettle_namespace_exposes_version_and_theme() {
        // Cycle 324 drift guard. The minimum-viable read-only Lua
        // surface: a user's init.lua can call kettle.version() +
        // kettle.theme() to print which kettle they're running.
        let eng = LuaEngine::new("TokyoNight Night").expect("init");
        let v = eng.eval_str("return kettle.version()").expect("version");
        assert_eq!(v, env!("CARGO_PKG_VERSION"));
        let t = eng.eval_str("return kettle.theme()").expect("theme");
        assert_eq!(t, "TokyoNight Night");
    }

    #[test]
    fn kettle_config_path_returns_a_string_or_nil() {
        // Cycle 446 drift guard. `kettle.config_path()` (cycle 324)
        // returns either the resolved XDG-style config path as a
        // string or nil. Plugins inspecting their environment
        // (e.g. `kettle.notify("config: " .. (kettle.config_path() or
        // "default"))`) need both branches to behave consistently.
        // Without this test a future refactor of Config::default_path
        // could silently degrade the API.
        let eng = LuaEngine::new("Default").expect("init");
        // type(kettle.config_path()) is 'string' on every supported
        // host that exposes XDG_CONFIG_HOME / %APPDATA% / $HOME, and
        // 'nil' otherwise. Either is fine — the contract is one or
        // the other (no error, no number, no table).
        let kind = eng
            .eval_str("return type(kettle.config_path())")
            .expect("eval");
        assert!(
            kind == "string" || kind == "nil",
            "kettle.config_path() returned unexpected type {kind:?}"
        );
    }

    #[test]
    fn kettle_namespace_arithmetic_still_works() {
        // Sanity: the standard library functions in mlua's Lua VM
        // are usable. Without this, a user script calling
        // `string.format(...)` or `math.floor(...)` would fail
        // mysteriously.
        let eng = LuaEngine::new("Default").expect("init");
        let r = eng.eval_str("return 2 + 3").expect("eval");
        assert_eq!(r, "5");
        let r2 = eng.eval_str("return string.upper('hello')").expect("eval");
        assert_eq!(r2, "HELLO");
    }

    #[test]
    fn send_text_queues_command_drained_by_app() {
        // Cycle 325 drift guard. `kettle.send_text(s)` queues a
        // command that the App drains + writes to the focused pane.
        let eng = LuaEngine::new("Default").expect("init");
        eng.eval_str("kettle.send_text('echo hello\\n')")
            .expect("eval");
        eng.eval_str("kettle.send_text('ls -la\\n')").expect("eval");
        let cmds = eng.drain_commands();
        assert_eq!(cmds.len(), 2);
        match (&cmds[0], &cmds[1]) {
            (LuaCommand::SendText(a), LuaCommand::SendText(b)) => {
                assert_eq!(a, "echo hello\n");
                assert_eq!(b, "ls -la\n");
            }
            other => panic!("unexpected commands: {other:?}"),
        }
        // Drain is destructive — second drain returns empty.
        assert_eq!(eng.drain_commands().len(), 0);
    }

    #[test]
    fn exec_action_queues_named_action() {
        // Cycle 326 drift guard. `kettle.exec_action(name)` queues
        // the name string; App turns it into an Action via
        // `Action::from_name` at drain time.
        let eng = LuaEngine::new("Default").expect("init");
        eng.eval_str("kettle.exec_action('new_tab')").expect("eval");
        eng.eval_str("kettle.exec_action('toggle_vi_mode')")
            .expect("eval");
        let cmds = eng.drain_commands();
        assert_eq!(cmds.len(), 2);
        match (&cmds[0], &cmds[1]) {
            (LuaCommand::ExecAction(a), LuaCommand::ExecAction(b)) => {
                assert_eq!(a, "new_tab");
                assert_eq!(b, "toggle_vi_mode");
            }
            other => panic!("unexpected commands: {other:?}"),
        }
    }

    #[test]
    fn notify_queues_notify_command() {
        // Cycle 430 drift guard. `kettle.notify(title, body?)` must
        // queue a `LuaCommand::Notify` with the title + optional
        // body; the cycle-426-428 `drain_lua_hook_commands` helper
        // depends on this variant being present.
        let eng = LuaEngine::new("Default").expect("init");
        eng.eval_str("kettle.notify('Build done', 'rustc finished')")
            .expect("eval");
        eng.eval_str("kettle.notify('Quick ping')").expect("eval");
        let cmds = eng.drain_commands();
        assert_eq!(cmds.len(), 2);
        match (&cmds[0], &cmds[1]) {
            (
                LuaCommand::Notify {
                    title: t1,
                    body: b1,
                },
                LuaCommand::Notify {
                    title: t2,
                    body: b2,
                },
            ) => {
                assert_eq!(t1, "Build done");
                assert_eq!(b1, "rustc finished");
                assert_eq!(t2, "Quick ping");
                // Optional body defaults to empty string when omitted.
                assert_eq!(b2, "");
            }
            other => panic!("unexpected commands: {other:?}"),
        }
    }

    #[test]
    fn set_theme_queues_set_theme_command() {
        // Cycle 430 drift guard. `kettle.set_theme(name)` must queue
        // a `LuaCommand::SetTheme` with the name; the cycle-426-428
        // `drain_lua_hook_commands` helper resolves it via
        // `kettle_config::Theme::find_name` at drain time.
        let eng = LuaEngine::new("Default").expect("init");
        eng.eval_str("kettle.set_theme('TokyoNight Night')")
            .expect("eval");
        eng.eval_str("kettle.set_theme('Solarized Dark')")
            .expect("eval");
        let cmds = eng.drain_commands();
        assert_eq!(cmds.len(), 2);
        match (&cmds[0], &cmds[1]) {
            (LuaCommand::SetTheme(a), LuaCommand::SetTheme(b)) => {
                assert_eq!(a, "TokyoNight Night");
                assert_eq!(b, "Solarized Dark");
            }
            other => panic!("unexpected commands: {other:?}"),
        }
    }

    #[test]
    fn on_event_hook_registers_and_fires() {
        // Cycle 365 drift guard. kettle.on('startup', fn) registers
        // a callback; fire_event(Startup) invokes it. Errors from
        // individual callbacks DON'T propagate (logged + skipped),
        // so a broken plugin can't take down kettle.
        let eng = LuaEngine::new("Default").expect("init");
        // Multiple subscribers + one writes to a global as a side
        // effect we can check.
        eng.eval_str(
            "fired = 0
             kettle.on('startup', function() fired = fired + 10 end)
             kettle.on('startup', function() fired = fired + 1 end)",
        )
        .expect("eval");
        eng.fire_event(&LuaEvent::Startup);
        assert_eq!(eng.eval_str("return fired").unwrap(), "11");
        // Re-firing accumulates again.
        eng.fire_event(&LuaEvent::Startup);
        assert_eq!(eng.eval_str("return fired").unwrap(), "22");
        // Event variants with payload pass the payload to the callback.
        eng.eval_str(
            "tabs_seen = {}
             kettle.on('tab_add', function(i)
                tabs_seen[#tabs_seen + 1] = i
             end)",
        )
        .expect("eval");
        eng.fire_event(&LuaEvent::TabAdd(0));
        eng.fire_event(&LuaEvent::TabAdd(2));
        assert_eq!(eng.eval_str("return #tabs_seen").unwrap(), "2");
        assert_eq!(eng.eval_str("return tabs_seen[1]").unwrap(), "0");
        assert_eq!(eng.eval_str("return tabs_seen[2]").unwrap(), "2");
        // Firing an event with no subscribers is a no-op.
        eng.fire_event(&LuaEvent::Bell(7));
    }

    #[test]
    fn on_event_hook_isolates_callback_errors() {
        // Cycle 365: a callback that raises a Lua error doesn't
        // propagate — logged + skipped + sibling callbacks still
        // run. This is the "broken plugin doesn't take down
        // kettle" contract.
        let eng = LuaEngine::new("Default").expect("init");
        eng.eval_str(
            "ok_count = 0
             kettle.on('startup', function() error('intentional') end)
             kettle.on('startup', function() ok_count = ok_count + 1 end)",
        )
        .expect("eval");
        eng.fire_event(&LuaEvent::Startup);
        // Sibling callback ran despite the first one erroring.
        assert_eq!(eng.eval_str("return ok_count").unwrap(), "1");
    }

    #[test]
    fn add_menu_item_registers_and_invoke_runs_callback() {
        // Cycle 435 drift guard. `kettle.add_menu_item(label, cb)`
        // (cycle 375) appends a {label, callback} entry to the
        // kettle_menu_items registry; `invoke_menu_item(idx)` calls
        // the matching callback. Plugin-context-menu surface relies
        // on both. Callback errors log + don't propagate.
        let eng = LuaEngine::new("Default").expect("init");
        eng.eval_str(
            "fired = 0
             kettle.add_menu_item('First', function() fired = fired + 1 end)
             kettle.add_menu_item('Second', function() fired = fired + 10 end)
             kettle.add_menu_item('Broken', function() error('intentional') end)",
        )
        .expect("eval");
        eng.invoke_menu_item(0);
        assert_eq!(eng.eval_str("return fired").unwrap(), "1");
        eng.invoke_menu_item(1);
        assert_eq!(eng.eval_str("return fired").unwrap(), "11");
        // Broken callback errors log but don't propagate.
        eng.invoke_menu_item(2);
        // Out-of-range index errors log but don't propagate either.
        eng.invoke_menu_item(99);
        // fired is unchanged after the broken + out-of-range calls.
        assert_eq!(eng.eval_str("return fired").unwrap(), "11");
    }

    #[test]
    fn url_handler_matches_pattern_and_short_circuits() {
        // Cycle 435 drift guard. `kettle.add_url_handler(name,
        // pattern, cb)` (cycle 374) registers a handler;
        // `try_url_handler(url)` returns true + invokes the cb when
        // the Lua-pattern matches, false otherwise. Used by the
        // url-open path to let plugins claim URLs before the system
        // browser sees them.
        let eng = LuaEngine::new("Default").expect("init");
        eng.eval_str(
            "github_hits = 0
             kettle.add_url_handler(
                'github',
                'https://github%.com/.*',
                function(_url) github_hits = github_hits + 1 end
             )",
        )
        .expect("eval");
        // Matching URL → handler claims it.
        assert!(eng.try_url_handler("https://github.com/kettle"));
        assert_eq!(eng.eval_str("return github_hits").unwrap(), "1");
        // Non-matching URL → kettle's default path proceeds.
        assert!(!eng.try_url_handler("https://example.com/"));
        assert_eq!(eng.eval_str("return github_hits").unwrap(), "1");
    }

    #[test]
    fn exec_file_runs_a_real_script() {
        // The script writes a result into a global so the test can
        // check it ran. Same shape as a user's `~/.config/kettle/
        // init.lua` setting up environment.
        // Cycle 592: PID + nanos so parallel `cargo test` runs don't
        // race on a shared /tmp path. Same pattern as the cycle-587
        // oversize-script test below.
        let path = std::env::temp_dir().join(format!(
            "kettle-lua-cycle324-smoke-{}-{}.lua",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::write(&path, "answer = kettle.version()\n").expect("write");
        let eng = LuaEngine::new("Default").expect("init");
        eng.exec_file(&path).expect("exec");
        let v = eng.eval_str("return answer").expect("eval");
        assert_eq!(v, env!("CARGO_PKG_VERSION"));
        let _ = std::fs::remove_file(&path);
    }

    /// Cycle 587 drift guard: an oversized Lua script must be refused
    /// via the metadata pre-check rather than read into RAM. Real
    /// init.lua files top out around 100 KB; a multi-MB blob is either
    /// a runaway autogen script or a swap-attack — either way the
    /// load should fail loud rather than OOM mid-load.
    #[test]
    fn exec_file_rejects_oversize_script() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "kettle-lua-cycle587-oversize-{}.lua",
            std::process::id()
        ));
        // 5 MiB of LEGITIMATE Lua (`x = x + 1\n` repeated). Verifies
        // the size gate fires BEFORE loading — even a syntactically
        // valid payload past the cap is refused, so a future refactor
        // that drops the size check fails the gauntlet.
        let line = "x = x + 1\n";
        let copies = (5 * 1024 * 1024) / line.len() + 1;
        let oversize: String = line.repeat(copies);
        std::fs::write(&path, &oversize).expect("write oversize lua");
        let eng = LuaEngine::new("Default").expect("init");
        let err = eng
            .exec_file(&path)
            .expect_err("oversize script must be refused");
        assert!(
            err.to_string().contains("refusing to load"),
            "expected refusal message, got: {err}"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn safe_sandbox_nils_dangerous_stdlib_apis() {
        // Cycle 571 drift guard. The cycle-376 sandbox (default
        // `lua-sandbox = safe`) nils the Lua stdlib APIs that can
        // execute external processes, open arbitrary files, or load
        // native shared libraries. A future refactor removing one of
        // these nils silently degrades the security posture documented
        // in SECURITY.md (cycle 447 "Lua plugin sandbox escape" in
        // scope).
        //
        // Assert every member of the nil-list is `nil` after sandbox
        // construction. The list mirrors the in-code nil sweep.
        let eng = LuaEngine::new("Default").expect("init (safe sandbox)");
        // os.* — process control + filesystem mutation.
        for api in [
            "os.execute",
            "os.exit",
            "os.remove",
            "os.rename",
            "os.tmpname",
            "os.setlocale",
        ] {
            let kind = eng
                .eval_str(&format!("return type({api})"))
                .unwrap_or_else(|_| panic!("eval type({api})"));
            assert_eq!(
                kind, "nil",
                "sandbox should nil {api} but got type {kind:?}"
            );
        }
        // io.* — arbitrary file open/read/write.
        for api in [
            "io.open",
            "io.popen",
            "io.lines",
            "io.input",
            "io.output",
            "io.stdin",
            "io.stdout",
            "io.stderr",
        ] {
            let kind = eng
                .eval_str(&format!("return type({api})"))
                .unwrap_or_else(|_| panic!("eval type({api})"));
            assert_eq!(
                kind, "nil",
                "sandbox should nil {api} but got type {kind:?}"
            );
        }
        // Global file-load functions.
        for api in ["loadfile", "dofile"] {
            let kind = eng
                .eval_str(&format!("return type({api})"))
                .unwrap_or_else(|_| panic!("eval type({api})"));
            assert_eq!(
                kind, "nil",
                "sandbox should nil {api} but got type {kind:?}"
            );
        }
        // package.loadlib — native code execution.
        let kind = eng
            .eval_str("return type(package.loadlib)")
            .expect("eval type(package.loadlib)");
        assert_eq!(
            kind, "nil",
            "sandbox should nil package.loadlib but got type {kind:?}"
        );

        // Sanity: SAFE stdlib functions stay callable. If a future
        // refactor over-nils, this catches it from the other side.
        assert_eq!(
            eng.eval_str("return type(string.upper)").unwrap(),
            "function"
        );
        assert_eq!(
            eng.eval_str("return type(table.insert)").unwrap(),
            "function"
        );
        assert_eq!(eng.eval_str("return type(math.floor)").unwrap(), "function");
    }

    #[test]
    fn trusted_sandbox_leaves_stdlib_intact() {
        // Cycle 571 companion test. The opt-in `lua-sandbox =
        // trusted` mode leaves the dangerous APIs callable. Users
        // explicitly setting `trusted` accept the full Lua stdlib
        // surface (cycle-447 SECURITY.md: opt-in trust, out-of-scope
        // for sandbox-escape reports). A future refactor that nils
        // these even in trusted mode silently breaks user scripts.
        let eng = LuaEngine::new_with_sandbox("Default", false).expect("init (trusted sandbox)");
        // os.execute exists in trusted mode (still a function).
        assert_eq!(eng.eval_str("return type(os.execute)").unwrap(), "function");
        // io.open exists in trusted mode.
        assert_eq!(eng.eval_str("return type(io.open)").unwrap(), "function");
        // package.loadlib exists in trusted mode.
        assert_eq!(
            eng.eval_str("return type(package.loadlib)").unwrap(),
            "function"
        );
    }

    /// Cycle 591 drift guard: pin that mlua's `Lua::new()` default
    /// excludes the entire `debug` library (per its `StdLib::ALL_SAFE`
    /// load list). If a future refactor switches to `Lua::unsafe_new()`
    /// or explicitly loads `StdLib::DEBUG`, the dangerous methods
    /// (`debug.getregistry` reaches into mlua's reference table,
    /// `debug.sethook` is an instruction-level DoS hook, `debug.set*`
    /// breaks opaque-userdata encapsulation) would silently become
    /// reachable from user scripts. This test catches that on both
    /// safe and trusted sandbox modes — neither is meant to expose
    /// the debug surface.
    #[test]
    fn lua_default_globals_exclude_debug_library() {
        for safe in [true, false] {
            let eng = LuaEngine::new_with_sandbox("Default", safe).expect("init");
            assert_eq!(
                eng.eval_str("return type(debug)").unwrap(),
                "nil",
                "safe={safe}: `debug` library must be nil at the global \
                 level — mlua's Lua::new() defaults exclude it. If a future \
                 refactor switches to Lua::unsafe_new() or loads StdLib::DEBUG, \
                 update SECURITY.md cycle-447 + cycle-591 notes accordingly"
            );
        }
    }
}
