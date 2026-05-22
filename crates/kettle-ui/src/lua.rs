//! Cycle 324: Lua scripting foundation (WezTerm parity).
//!
//! Exposes a `kettle` namespace inside a Lua VM so the user's
//! `--lua-script PATH` (or future `<config-dir>/init.lua`) can read
//! kettle's runtime state. Foundation sub-cycle ships read-only
//! introspection; subsequent sub-cycles add side-effect APIs:
//!
//!   cycle 324 (this one): kettle.version() / config_path() / theme()
//!   cycle 325+:           kettle.send_text(s), set_tab_title(s)
//!   cycle 326+:           kettle.notify(title, body)
//!   cycle 327+:           kettle.on(event, callback) event hooks
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
        let lua = Lua::new();
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
        lua.globals()
            .set("kettle", kettle_tbl)
            .context("install kettle namespace")?;
        Ok(Self { lua, pending })
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
    pub fn exec_file(&self, path: &Path) -> Result<()> {
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
    fn exec_file_runs_a_real_script() {
        // The script writes a result into a global so the test can
        // check it ran. Same shape as a user's `~/.config/kettle/
        // init.lua` setting up environment.
        let dir = std::env::temp_dir();
        let path = dir.join("kettle-lua-cycle324-smoke.lua");
        std::fs::write(&path, "answer = kettle.version()\n").expect("write");
        let eng = LuaEngine::new("Default").expect("init");
        eng.exec_file(&path).expect("exec");
        let v = eng.eval_str("return answer").expect("eval");
        assert_eq!(v, env!("CARGO_PKG_VERSION"));
        let _ = std::fs::remove_file(&path);
    }
}
