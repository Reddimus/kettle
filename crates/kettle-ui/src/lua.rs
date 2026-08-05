//! Lua scripting foundation (WezTerm parity).
//!
//! Exposes a `kettle` namespace inside a Lua VM so the user's
//! `--lua-script PATH` (or future `<config-dir>/init.lua`) can read
//! kettle's runtime state. The foundation ships read-only
//! introspection; side-effect APIs build on top of it:
//!
//!   kettle.version() / config_path() / theme()  -- read-only foundation
//!   kettle.send_text(s), kettle.exec_action(name)
//!   kettle.notify(title, body), kettle.set_theme(name)
//!   kettle.on(event, callback) event hooks       -- foundation; see
//!                                                    docs/TERMINATOR-PLUGIN-DESIGN.md
//!                                                    for the full roadmap
//!
//! Why read-only first: hooking Lua into the live App requires
//! threading an Arc<Mutex<...>> handle through, which is the kind
//! of plumbing that's easier to verify in isolation. The foundation
//! ships the dep + the VM + the namespace + a drift guard;
//! the side-effect APIs add incrementally without re-touching
//! the wiring.

use anyhow::{Context, Result};
use mlua::Lua;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Per-call cap on `kettle.send_text(s)`. A hostile
/// `init.lua` running under the default `lua-sandbox = safe` mode
/// (where `os.execute`/`io.popen` are nil'd by the safe-sandbox setup
/// in `new_inner`) could still queue gigabytes of PTY-bound text via
/// `for i=1,10000 do kettle.send_text(string.rep("X", 1<<20)) end`.
/// 1 MiB per call covers any realistic multi-line snippet paste
/// with massive headroom and stops the bomb shape early.
const MAX_LUA_SEND_TEXT_BYTES: usize = 1 << 20;

/// Aggregate cap (bytes) on the SUM of every currently-queued
/// `SendText` command's payload, independent of `MAX_LUA_SEND_TEXT_BYTES`
/// (per-call) and `MAX_PENDING_COMMANDS` (per-entry-count). Without this,
/// a script could stay under both existing caps yet still bankrupt kettle:
/// `for i=1,1024 do kettle.send_text(string.rep('X', 1<<20)) end` queues
/// 1024 commands (the entry cap) each exactly at the 1 MiB per-call cap.
/// 8 MiB (8x the per-call cap) covers pasting several large snippets back
/// to back with real headroom while making that aggregate-bomb shape
/// impossible. Past this cap, `bounded_push` rejects further `SendText`
/// pushes until `drain_commands` resets the running total.
const MAX_LUA_PENDING_SEND_BYTES: usize = 8 << 20;

/// Per-call cap on `kettle.notify(title, body)`. Real
/// desktop notifications are tiny (titles ~30 chars, bodies a
/// sentence or two). 8 KiB per field is ~100× over realistic
/// use, ample for a multi-line code-snippet body, and matches
/// the notify-rust crate's typical practical limits without
/// burning unbounded heap on a hostile script that builds a
/// huge title in a loop.
const MAX_LUA_NOTIFY_BYTES: usize = 8 << 10;

/// Action and theme names are identifiers, not bulk-data channels.
/// The built-in names are all far below this limit; bounding them keeps
/// 1024 individually valid commands from retaining arbitrarily large
/// Rust strings outside Lua's own heap accounting.
const MAX_LUA_ACTION_NAME_BYTES: usize = 256;
const MAX_LUA_THEME_NAME_BYTES: usize = 256;

/// Cap on the in-process LuaCommand queue length. A
/// hostile script that fires `for i=1,10^9 do
/// kettle.exec_action("noop") end` (or any other API) would grow
/// `pending` without bound — even short commands at 32 bytes each
/// × 10^9 = 32 GB. 1024 entries is well above any realistic
/// init.lua's batch (most fire 1-10 commands; a power user wiring
/// up a couple dozen hooks tops out around 50). Past the cap, new
/// pushes return `false` with a `log::warn`.
const MAX_PENDING_COMMANDS: usize = 1024;

/// Per-registry caps on Lua-registered callbacks. The
/// command queue above is bounded against a hostile `init.lua`, but the
/// callback registries (`kettle.on`, `add_menu_item`, `add_url_handler`) were
/// not — a runaway `for i=1,1e9 do kettle.on('output', f) end` grew the
/// registry unbounded AND made every event fire walk a giant list. Sized far
/// above any legitimate plugin (a busy config wires up a few dozen). Past the
/// cap, registration returns `false` with a single `log::warn` (the flags below
/// keep a pathological loop from spamming the log).
const MAX_LUA_CALLBACKS_PER_EVENT: usize = 256;
const MAX_LUA_MENU_ITEMS: usize = 256;
const MAX_LUA_URL_HANDLERS: usize = 256;

/// Registry string fields are metadata, not bulk-data channels. Menu labels
/// render on one line, so 1 KiB leaves ample room for descriptive Unicode
/// labels while bounding the full 256-item registry to 256 KiB of label bytes.
/// URL-handler names are identifiers and use the same 256-byte allowance as
/// action/theme names. Patterns can reasonably be more involved, so they get
/// 4 KiB each (at most 1 MiB across the bounded handler registry).
const MAX_LUA_MENU_LABEL_BYTES: usize = 1 << 10;
const MAX_LUA_URL_HANDLER_NAME_BYTES: usize = 256;
const MAX_LUA_URL_PATTERN_BYTES: usize = 4 << 10;

static LUA_EVENTS_WARNED: AtomicBool = AtomicBool::new(false);
static LUA_UNKNOWN_EVENT_WARNED: AtomicBool = AtomicBool::new(false);
static LUA_MENU_WARNED: AtomicBool = AtomicBool::new(false);
static LUA_URL_WARNED: AtomicBool = AtomicBool::new(false);

/// Backing store for the Lua side-effect command queue. Holds the
/// command list plus a running total of bytes queued via `SendText`
/// specifically, so `bounded_push` can enforce `MAX_LUA_PENDING_SEND_BYTES`
/// (an aggregate cap) alongside `MAX_PENDING_COMMANDS` (an entry-count cap)
/// without a second lock or a separate atomic that could drift out of sync
/// with the actual queue contents. Both fields live behind the same
/// `Mutex`, so a push that updates one always sees a consistent view of
/// the other.
#[derive(Default)]
struct PendingQueue {
    commands: Vec<LuaCommand>,
    /// Sum of `s.len()` for every `LuaCommand::SendText(s)` currently
    /// in `commands`. Reset to 0 whenever `commands` is drained (the
    /// bytes it was tracking no longer exist in the queue).
    send_text_bytes: usize,
}

/// Locked push with queue-length AND (for `SendText`)
/// aggregate-byte caps. All four `kettle.*` side-effect callbacks route
/// through this so the `MAX_PENDING_COMMANDS` invariant is enforced
/// exactly once, not duplicated four times. The returned boolean tells Lua
/// whether the command was admitted to this in-process queue. It does not
/// promise eventual PTY delivery; that remains asynchronous and can still
/// fail if the target pane closes. A poisoned mutex is the only hard error
/// because it means kettle's queue state is no longer trustworthy.
fn bounded_push(pending: &Mutex<PendingQueue>, cmd: LuaCommand) -> mlua::Result<bool> {
    let mut q = pending
        .lock()
        .map_err(|e| mlua::Error::external(format!("pending mutex: {e}")))?;
    if q.commands.len() >= MAX_PENDING_COMMANDS {
        log::warn!(
            "lua command queue saturated at {MAX_PENDING_COMMANDS}; dropping {:?}",
            std::mem::discriminant(&cmd)
        );
        return Ok(false);
    }
    // Aggregate byte cap only applies to SendText: it is the only variant
    // intended to carry bulk data. Checked (and, on admission, accounted)
    // under the same lock as the length check above so the two invariants
    // can never observe a torn intermediate state.
    if let LuaCommand::SendText(ref s) = cmd {
        let prospective_total = q.send_text_bytes.saturating_add(s.len());
        if prospective_total > MAX_LUA_PENDING_SEND_BYTES {
            log::warn!(
                "lua send_text queue saturated at {} aggregate bytes (cap {MAX_LUA_PENDING_SEND_BYTES}); dropping {} more bytes",
                q.send_text_bytes,
                s.len()
            );
            return Ok(false);
        }
        q.send_text_bytes = prospective_total;
    }
    q.commands.push(cmd);
    Ok(true)
}

/// Side-effect commands buffered from Lua. The Lua VM
/// can't directly mutate App state (lifetime + threading), so
/// side-effect APIs (send_text, exec_action, notify, set_theme) push
/// onto this queue and the App drains it after the script
/// returns. Same shape as the `--remote-send` / `--remote-file`
/// remote-command file's line buffer, just in-process.
#[derive(Debug, Clone)]
pub enum LuaCommand {
    /// `kettle.send_text(s)` → write s to the focused pane's PTY.
    SendText(String),
    /// `kettle.exec_action(name)` → dispatch a named kettle action
    /// (parsed via `Action::from_name`). The name is whatever the
    /// keybind grammar accepts: `new_tab`, `split_right`,
    /// `toggle_vi_mode`, etc.
    ExecAction(String),
    /// `kettle.notify(title, body)` → desktop notification
    /// via notify-rust. Fires once kettle drains commands so a script
    /// running before the first paint doesn't race the notification
    /// daemon.
    Notify { title: String, body: String },
    /// `kettle.set_theme(name)` → switch the active theme
    /// at runtime. Looked up case-insensitively against the ~500
    /// bundled themes via Theme::find_name; falls through with
    /// log::warn if no match.
    SetTheme(String),
}

/// Terminator plugin parity (design doc:
/// docs/TERMINATOR-PLUGIN-DESIGN.md): event hooks. The foundation
/// ships the registry + dispatch surface; wiring each variant to
/// its actual emission site in App follows incrementally.
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
    /// Terminator plugin parity: emitted when a split pane is
    /// closed — the pane analog of [`TabClose`](Self::TabClose). Payload: the
    /// id of the pane that closed. Fired *before* the pane's PTY teardown so
    /// plugins keyed by pane id (status bars, per-pane theme overlays,
    /// activity watchers) can drop their state. Used by
    /// `kettle.on('pane_close', function(pane_id) … end)`.
    PaneClose(u64),
    /// Terminator plugin parity (phase 3 of
    /// docs/TERMINATOR-PLUGIN-DESIGN.md): emitted on each batch of
    /// PTY output drained from a pane.
    /// Payload: pane id + bytes since last emission. Throttled
    /// at the dispatch site (App level) — Lua callbacks see a
    /// coalesced byte chunk, not every individual chunk from a
    /// busy build.
    Output(u64, Vec<u8>),
    /// Terminator plugin parity (focus event hook).
    /// Emitted when keyboard focus moves
    /// between panes (within a tab, across tabs, or window
    /// restore). Payload: (previous_focused_pane_id_or_nil,
    /// new_focused_pane_id). Used by status-bar plugins,
    /// activity-watch plugins, and per-pane theme overlays
    /// that need to react to focus changes.
    PaneFocus(Option<u64>, u64),
    /// Terminator plugin parity (title-change event hook).
    /// Emitted when a pane's title
    /// changes — via OSC 0/2 (shell-issued), inline edit
    /// (`Action::EditPaneTitle`), reset (TermEvent::ResetTitle),
    /// or the remote-context detection in `poll_remote_contexts`. Payload:
    /// (pane_id, new_title). Used by status-bar plugins and
    /// title-mirroring plugins.
    TitleChanged(u64, String),
    /// Terminator plugin parity (URL-click event hook).
    /// Emitted on every safe URL click
    /// — before any pattern-handler dispatch — so analytics /
    /// logging / workflow-trigger plugins see ALL URL clicks,
    /// regardless of which handler ultimately opens them.
    /// Payload: the URI string.
    UrlClicked(String),
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
            LuaEvent::PaneClose(_) => "pane_close",
            LuaEvent::Output(_, _) => "output",
            LuaEvent::PaneFocus(_, _) => "pane_focus",
            LuaEvent::TitleChanged(_, _) => "title_changed",
            LuaEvent::UrlClicked(_) => "url_clicked",
        }
    }
}

/// Resolve a script-provided event name without allocating a Rust `String`.
/// Keeping this list closed prevents a script from creating one registry table
/// per arbitrary name and makes the per-event callback cap a global bound over
/// the nine event kinds Kettle can actually emit.
fn supported_lua_event_name(name: &[u8]) -> Option<&'static str> {
    [
        "startup",
        "tab_add",
        "tab_close",
        "bell",
        "pane_close",
        "output",
        "pane_focus",
        "title_changed",
        "url_clicked",
    ]
    .into_iter()
    .find(|candidate| name == candidate.as_bytes())
}

/// Owned Lua VM with kettle's namespace registered. Single-threaded
/// today (Lua VMs aren't natively reentrant); a future change may
/// wrap in `Arc<Mutex<...>>` to allow event-hook callbacks from
/// the App's threads.
/// The every-N-instructions hook fires once per this many
/// Lua VM instructions. Large enough that any normal callback (which runs far
/// fewer instructions) never triggers it — zero overhead in the common case —
/// while still giving fine-grained runaway detection.
const HOOK_INSTRUCTION_INTERVAL: u32 = 1_000_000;

/// Default per-invocation budget, expressed as the max number of
/// hook fires before a script is force-aborted. `128 × 1_000_000` ≈ 128 M
/// instructions — an enormous margin over any realistic plugin call, but a
/// `while true do end` (incl. inside the `output` callback) trips it in well
/// under a second instead of wedging the UI thread forever.
const DEFAULT_MAX_HOOK_FIRES: u64 = 128;

/// What the registered Lua URL handlers decided about a URL.
///
/// Spelled out as three cases rather than a bool because a handler can also
/// REWRITE the URL — the shape every handler in `docs/examples/init.lua` uses,
/// and the one a bool silently threw away.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UrlHandlerOutcome {
    /// Open this URL instead of the matched text.
    Open(String),
    /// A handler dealt with the URL itself; open nothing.
    Handled,
    /// Nobody claimed it — continue to `custom_url_handler` / the system open.
    Fallthrough,
}

pub struct LuaEngine {
    lua: Lua,
    /// Side-effect commands queued by Lua functions (plus the
    /// running `SendText` byte total used to enforce
    /// `MAX_LUA_PENDING_SEND_BYTES`). Drained by the App after
    /// exec_file returns.
    pending: Arc<Mutex<PendingQueue>>,
    /// Canonical active theme exposed by `kettle.theme()`. App updates this
    /// only after a queued `set_theme` name resolves and is applied.
    active_theme: Arc<Mutex<String>>,
    /// Instruction-budget watchdog. `hook_fires` counts how
    /// many times the every-N-instructions hook has fired since the current
    /// top-level Lua invocation began (reset by `arm_budget`); when it exceeds
    /// the cap captured in the hook closure, the hook returns an error,
    /// aborting a runaway script. Without this, a plugin's `while true do end`
    /// (or an infinite `output` callback) froze the UI thread permanently —
    /// there was no CPU budget.
    hook_fires: Arc<AtomicU64>,
}

impl LuaEngine {
    /// Construct a fresh Lua VM with the read-only
    /// `kettle` namespace installed:
    ///
    ///   kettle.version()      → string  e.g. "1.7.8"
    ///   kettle.config_path()  → string|nil  the resolved config path
    ///   kettle.theme()        → string  e.g. "TokyoNight Night"
    ///
    /// Fails if the Lua VM can't initialize (resource exhaustion,
    /// not normally seen). Adding entries to the namespace is the
    /// happy path for future additions — extend this function.
    pub fn new(theme_name: &str) -> Result<Self> {
        Self::new_with_sandbox(theme_name, kettle_config::LuaSandbox::Safe)
    }

    /// Terminator plugin parity (phase 12 of
    /// docs/TERMINATOR-PLUGIN-DESIGN.md): build a VM at the configured trust
    /// level.
    ///
    /// `Safe` and `Restricted` both nil the Lua stdlib functions that execute
    /// external processes or open arbitrary files: `os.execute`, `os.exit`,
    /// `os.remove`, `os.rename`, `io.open`, `io.popen`, `io.lines`,
    /// `io.input`, `io.output`, `package.loadlib`, `loadfile`, `dofile`. The
    /// rest of the stdlib (string, table, math, `os.date`/`os.time`/
    /// `os.getenv`/`os.difftime`, `io.read`, ...) stays usable.
    ///
    /// **Nil'ing those is not what stops a plugin running commands.**
    /// `kettle.send_text` types into the focused shell, and a newline in that
    /// text runs what it typed — the documented example plugin clears the
    /// screen by typing the word and a newline. `Safe` guards against a
    /// careless plugin reaching the filesystem or spawning something behind
    /// your back; it does not contain a hostile one. `Restricted` is the level
    /// that does: it installs `send_text` and `exec_action` as refusals, so a
    /// plugin can observe, notify and restyle but cannot drive the terminal.
    ///
    /// Errors from setting these to nil are bubbled up — a Lua VM where the
    /// standard globals can't be removed isn't safe to proceed with.
    pub fn new_with_sandbox(theme_name: &str, sandbox: kettle_config::LuaSandbox) -> Result<Self> {
        Self::new_inner(theme_name, sandbox, DEFAULT_MAX_HOOK_FIRES)
    }

    /// Test-only constructor that dials the instruction budget down
    /// so a runaway aborts in a few million instructions (sub-second) rather
    /// than the ~128 M production cap.
    #[cfg(test)]
    pub(crate) fn new_with_max_hook_fires(theme_name: &str, max_fires: u64) -> Result<Self> {
        Self::new_inner(theme_name, kettle_config::LuaSandbox::Safe, max_fires)
    }

    /// The real constructor. `max_fires` is the per-invocation
    /// instruction-budget cap (in hook fires), captured by value into the hook
    /// closure — fixed for the VM's life, so no shared field is needed.
    fn new_inner(
        theme_name: &str,
        sandbox: kettle_config::LuaSandbox,
        max_fires: u64,
    ) -> Result<Self> {
        use kettle_config::LuaSandbox;
        let lua = Lua::new();
        // Whether this plugin may drive the terminal: type into a pane or
        // dispatch an action. Everything else is available at every level.
        let may_drive_terminal = !matches!(sandbox, LuaSandbox::Restricted);
        if !matches!(sandbox, LuaSandbox::Trusted) {
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
            // NOTE on `debug.*`: mlua's `Lua::new()`
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
        // Cap the VM heap. The instruction-budget
        // hook below only fires on Lua bytecode dispatch, so a single native call
        // (`string.rep('X', 1<<32)`, unbounded table growth) allocates before any
        // check can fire — this turns runaway allocation into a recoverable Lua
        // error instead of an OOM-abort of kettle. 256 MiB is far above any sane
        // plugin yet bounds a hostile/buggy one.
        lua.set_memory_limit(256 << 20)?;
        // Install the instruction-budget watchdog. The hook
        // fires every `HOOK_INSTRUCTION_INTERVAL` VM instructions; when a single
        // top-level invocation exceeds `max_hook_fires` fires it errors out,
        // unwinding the runaway script. User Lua can't disable it — mlua's
        // `StdLib::ALL_SAFE` excludes the `debug` library (so `debug.sethook`
        // is unreachable), and the hook is set from the Rust side here.
        let hook_fires = Arc::new(AtomicU64::new(0));
        {
            let fires = hook_fires.clone();
            lua.set_hook(
                mlua::HookTriggers::new().every_nth_instruction(HOOK_INSTRUCTION_INTERVAL),
                move |_lua, _debug| {
                    let n = fires.fetch_add(1, Ordering::Relaxed) + 1;
                    if n > max_fires {
                        Err(mlua::Error::RuntimeError(
                            "kettle: Lua script exceeded its instruction budget \
                             (possible infinite loop); aborted"
                                .to_string(),
                        ))
                    } else {
                        Ok(mlua::VmState::Continue)
                    }
                },
            )
            .context("install Lua instruction-budget hook")?;
        }
        let pending: Arc<Mutex<PendingQueue>> = Arc::new(Mutex::new(PendingQueue::default()));
        let kettle_tbl = lua.create_table().context("create kettle table")?;
        // Expose values as callable functions (not bare strings) so
        // user scripts use the conventional `kettle.version()` form.
        // Callable form also makes it easy to add side effects later
        // (e.g. `send_text` needs to be a function anyway).
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
        let active_theme = Arc::new(Mutex::new(theme_name.to_string()));
        let theme_for_lua = Arc::clone(&active_theme);
        kettle_tbl
            .set(
                "theme",
                lua.create_function(move |_, ()| {
                    theme_for_lua
                        .lock()
                        .map(|theme| theme.clone())
                        .map_err(|e| mlua::Error::external(format!("active theme mutex: {e}")))
                })?,
            )
            .context("set kettle.theme")?;
        // Side-effect API. `kettle.send_text(s)` queues
        // a SendText command for the App to drain + write to the
        // focused pane's PTY. Lua-side it looks synchronous, but
        // the actual PTY write happens once the script returns —
        // a kettle script can't observe its own typing.
        // Per-call cap on `s` (MAX_LUA_SEND_TEXT_BYTES), global cap
        // on the queue length (MAX_PENDING_COMMANDS), AND an aggregate
        // cap on the SUM of every queued SendText's bytes
        // (MAX_LUA_PENDING_SEND_BYTES, enforced inside bounded_push)
        // — see the constants above for the threat-model rationale.
        let pending_for_send = Arc::clone(&pending);
        kettle_tbl
            .set(
                "send_text",
                lua.create_function(move |_, value: mlua::LuaString| {
                    if !may_drive_terminal {
                        log::warn!(
                            "kettle.send_text: refused under `lua-sandbox = \
                             restricted` (typing into a pane can run commands)"
                        );
                        return Ok(false);
                    }
                    let len = value.as_bytes().len();
                    if len > MAX_LUA_SEND_TEXT_BYTES {
                        log::warn!(
                            "kettle.send_text: dropping {} bytes (cap {MAX_LUA_SEND_TEXT_BYTES})",
                            len
                        );
                        return Ok(false);
                    }
                    let s = value.to_str()?.to_string();
                    bounded_push(&pending_for_send, LuaCommand::SendText(s))
                })?,
            )
            .context("set kettle.send_text")?;
        // kettle.exec_action(name) dispatches a kettle
        // Action by its keybind-grammar name. Lua scripts get the
        // same dispatch power as the keymap — `new_tab`,
        // `split_right`, `toggle_vi_mode`, etc.
        let pending_for_action = Arc::clone(&pending);
        kettle_tbl
            .set(
                "exec_action",
                lua.create_function(move |_, value: mlua::LuaString| {
                    if !may_drive_terminal {
                        log::warn!(
                            "kettle.exec_action: refused under \
                             `lua-sandbox = restricted`"
                        );
                        return Ok(false);
                    }
                    let len = value.as_bytes().len();
                    if len > MAX_LUA_ACTION_NAME_BYTES {
                        log::warn!(
                            "kettle.exec_action: rejecting {}-byte name (cap {MAX_LUA_ACTION_NAME_BYTES})",
                            len
                        );
                        return Ok(false);
                    }
                    let name = value.to_str()?.to_string();
                    bounded_push(&pending_for_action, LuaCommand::ExecAction(name))
                })?,
            )
            .context("set kettle.exec_action")?;
        // Phase 7 of docs/TERMINATOR-PLUGIN-DESIGN.md: kettle.notify(title, body?)
        // queues a desktop notification. Body is optional;
        // `kettle.notify('Build done')` works too.
        // Per-field cap (MAX_LUA_NOTIFY_BYTES) on title +
        // body so a hostile script can't smuggle gigabytes through
        // the notify API. Real desktop notifications are tiny.
        let pending_for_notify = Arc::clone(&pending);
        kettle_tbl
            .set(
                "notify",
                lua.create_function(move |_, args: mlua::Variadic<mlua::LuaString>| {
                    let mut iter = args.into_iter();
                    let title_value = iter.next();
                    let body_value = iter.next();
                    let title_len = title_value
                        .as_ref()
                        .map_or(0, |value| value.as_bytes().len());
                    let body_len = body_value
                        .as_ref()
                        .map_or(0, |value| value.as_bytes().len());
                    if title_len > MAX_LUA_NOTIFY_BYTES || body_len > MAX_LUA_NOTIFY_BYTES {
                        log::warn!(
                            "kettle.notify: dropping (title {} bytes, body {} bytes; \
                             cap {MAX_LUA_NOTIFY_BYTES} each)",
                            title_len,
                            body_len
                        );
                        return Ok(false);
                    }
                    let title = title_value
                        .map(|value| value.to_str().map(|value| value.to_string()))
                        .transpose()?
                        .unwrap_or_default();
                    let body = body_value
                        .map(|value| value.to_str().map(|value| value.to_string()))
                        .transpose()?
                        .unwrap_or_default();
                    bounded_push(&pending_for_notify, LuaCommand::Notify { title, body })
                })?,
            )
            .context("set kettle.notify")?;
        // Phase 10 of docs/TERMINATOR-PLUGIN-DESIGN.md: kettle.set_theme(name)
        // queues a SetTheme command; the App drains + applies via
        // the existing NextTheme infrastructure.
        let pending_for_theme = Arc::clone(&pending);
        kettle_tbl
            .set(
                "set_theme",
                lua.create_function(move |_, value: mlua::LuaString| {
                    let len = value.as_bytes().len();
                    if len > MAX_LUA_THEME_NAME_BYTES {
                        log::warn!(
                            "kettle.set_theme: rejecting {}-byte name (cap {MAX_LUA_THEME_NAME_BYTES})",
                            len
                        );
                        return Ok(false);
                    }
                    let name = value.to_str()?.to_string();
                    bounded_push(&pending_for_theme, LuaCommand::SetTheme(name))
                })?,
            )
            .context("set kettle.set_theme")?;
        // Phase 8 of docs/TERMINATOR-PLUGIN-DESIGN.md: kettle.add_menu_item(
        //   label, callback). Lua-supplied entries that render BELOW
        // the built-in context-menu items. Callback is
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
                lua.create_function(|lua, (label, cb): (mlua::LuaString, mlua::Function)| {
                    let label_len = label.as_bytes().len();
                    if label_len > MAX_LUA_MENU_LABEL_BYTES {
                        if !LUA_MENU_WARNED.swap(true, Ordering::Relaxed) {
                            log::warn!(
                                "kettle.add_menu_item: rejecting {label_len}-byte label \
                                     (cap {MAX_LUA_MENU_LABEL_BYTES})"
                            );
                        }
                        return Ok(false);
                    }
                    // Match the former `String` argument's UTF-8 contract,
                    // but retain Lua's existing string in the registry
                    // rather than allocating a Rust copy.
                    let _ = label.to_str()?;

                    let items: mlua::Table = lua.named_registry_value("kettle_menu_items")?;
                    let n = items.len()?;
                    if n as usize >= MAX_LUA_MENU_ITEMS {
                        if !LUA_MENU_WARNED.swap(true, Ordering::Relaxed) {
                            log::warn!(
                                "kettle.add_menu_item: registry capped at \
                                     {MAX_LUA_MENU_ITEMS}; ignoring further items"
                            );
                        }
                        return Ok(false);
                    }
                    let entry = lua.create_table()?;
                    entry.set("label", label)?;
                    entry.set("callback", cb)?;
                    items.set(n + 1, entry)?;
                    Ok(true)
                })?,
            )
            .context("set kettle.add_menu_item")?;
        // Phase 9 of docs/TERMINATOR-PLUGIN-DESIGN.md: kettle.add_url_handler(
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
                    |lua,
                     (name, pattern, cb): (
                        mlua::LuaString,
                        mlua::LuaString,
                        mlua::Function,
                    )| {
                        let name_len = name.as_bytes().len();
                        let pattern_len = pattern.as_bytes().len();
                        if name_len > MAX_LUA_URL_HANDLER_NAME_BYTES
                            || pattern_len > MAX_LUA_URL_PATTERN_BYTES
                        {
                            if !LUA_URL_WARNED.swap(true, Ordering::Relaxed) {
                                log::warn!(
                                    "kettle.add_url_handler: rejecting (name {name_len} bytes, \
                                     pattern {pattern_len} bytes; caps \
                                     {MAX_LUA_URL_HANDLER_NAME_BYTES} and \
                                     {MAX_LUA_URL_PATTERN_BYTES})"
                                );
                            }
                            return Ok(false);
                        }
                        // Preserve the old UTF-8-only API without copying either
                        // field into a Rust allocation before registry admission.
                        let _ = name.to_str()?;
                        let _ = pattern.to_str()?;

                        let handlers: mlua::Table =
                            lua.named_registry_value("kettle_url_handlers")?;
                        let n = handlers.len()?;
                        if n as usize >= MAX_LUA_URL_HANDLERS {
                            if !LUA_URL_WARNED.swap(true, Ordering::Relaxed) {
                                log::warn!(
                                    "kettle.add_url_handler: registry capped at \
                                     {MAX_LUA_URL_HANDLERS}; ignoring further handlers"
                                );
                            }
                            return Ok(false);
                        }
                        let entry = lua.create_table()?;
                        entry.set("name", name)?;
                        entry.set("pattern", pattern)?;
                        entry.set("callback", cb)?;
                        handlers.set(n + 1, entry)?;
                        Ok(true)
                    },
                )?,
            )
            .context("set kettle.add_url_handler")?;
        // Terminator plugin parity foundation:
        // `kettle.on(event_name, callback)` registers a Lua function to
        // fire when the named event occurs. Stored as a registry-table
        // entry keyed by one of the nine LuaEvent names; callbacks accumulate
        // in a list (multiple subscribers per event). Unknown names return
        // false instead of creating permanent registry keys.
        //
        // Today's wiring: registry installed + drift-guarded. App-side
        // emission per LuaEvent variant lands incrementally
        // (see docs/TERMINATOR-PLUGIN-DESIGN.md phase 3+).
        let event_table = lua.create_table().context("create event-hooks table")?;
        lua.set_named_registry_value("kettle_events", event_table)
            .context("register kettle_events table")?;
        kettle_tbl
            .set(
                "on",
                lua.create_function(
                    |lua, (requested_name, cb): (mlua::LuaString, mlua::Function)| {
                        let Some(name) =
                            supported_lua_event_name(requested_name.as_bytes().as_ref())
                        else {
                            if !LUA_UNKNOWN_EVENT_WARNED.swap(true, Ordering::Relaxed) {
                                log::warn!(
                                    "kettle.on: rejecting unknown event name; supported events: \
                                     startup, tab_add, tab_close, bell, pane_close, output, \
                                     pane_focus, title_changed, url_clicked"
                                );
                            }
                            return Ok(false);
                        };

                        let events: mlua::Table = lua.named_registry_value("kettle_events")?;
                        let list: mlua::Table = match events.get::<mlua::Value>(name)? {
                            mlua::Value::Table(t) => t,
                            _ => {
                                let t = lua.create_table()?;
                                events.set(name, t.clone())?;
                                t
                            }
                        };
                        let n = list.len()?;
                        if n as usize >= MAX_LUA_CALLBACKS_PER_EVENT {
                            if !LUA_EVENTS_WARNED.swap(true, Ordering::Relaxed) {
                                log::warn!(
                                    "kettle.on('{name}'): capped at \
                                     {MAX_LUA_CALLBACKS_PER_EVENT} callbacks; \
                                     ignoring further subscribers"
                                );
                            }
                            return Ok(false);
                        }
                        list.set(n + 1, cb)?;
                        Ok(true)
                    },
                )?,
            )
            .context("set kettle.on")?;
        lua.globals()
            .set("kettle", kettle_tbl)
            .context("install kettle namespace")?;
        Ok(Self {
            lua,
            pending,
            active_theme,
            hook_fires,
        })
    }

    /// Reset the instruction budget at the start of a
    /// top-level Lua invocation, so each call gets the full budget rather than
    /// sharing one cumulative counter across the session (which would
    /// eventually false-trip a long-lived plugin). Called by every public entry
    /// point that runs user Lua.
    fn arm_budget(&self) {
        self.hook_fires.store(0, Ordering::Relaxed);
    }

    /// Synchronize `kettle.theme()` after App successfully applies a queued
    /// theme change. Invalid names never reach this method, so introspection
    /// continues to describe the active configuration rather than the last
    /// requested value.
    pub(crate) fn set_active_theme(&self, theme_name: &str) -> Result<()> {
        let mut active = self
            .active_theme
            .lock()
            .map_err(|e| anyhow::anyhow!("active theme mutex: {e}"))?;
        theme_name.clone_into(&mut *active);
        Ok(())
    }

    /// List the labels of every Lua-registered context
    /// menu item, in registration order. Used by App to extend the
    /// built-in context menu with kettle.add_menu_item entries.
    pub fn list_menu_item_labels(&self) -> mlua::Result<Vec<String>> {
        self.arm_budget();
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

    /// Invoke the Lua callback for menu-item index
    /// `idx` (0-based; the App walks the registered list in the
    /// same order `list_menu_item_labels` returned). Errors
    /// log::warn + don't propagate.
    pub fn invoke_menu_item(&self, idx: usize) {
        self.arm_budget();
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

    /// Run the registered URL handlers against `url` and report what they
    /// decided.
    ///
    /// The contract is the one `docs/examples/init.lua` documents to users,
    /// because that is the file they copy from:
    ///
    /// * **a string** — open THAT URL instead of the matched text. Every
    ///   handler in the shipped example file is this shape (`LP: #12345` →
    ///   `https://bugs.launchpad.net/bugs/12345`), and the returned string
    ///   used to be discarded, so every one of them did nothing.
    /// * **`true`** — the handler opened it itself; kettle must not open
    ///   anything.
    /// * **`nil` / no return / `false`** — declined. Try the next handler,
    ///   then kettle's own open.
    /// * **an error** — declined, and logged. A handler exists to enhance a
    ///   link, so a broken one must never be the reason a link stops
    ///   working; it used to claim the URL anyway and leave the click dead.
    ///
    /// The caller re-checks a returned URL against `is_safe_url` before
    /// opening it. A handler runs in the user's own config, but it is fed
    /// untrusted terminal text, so it must not become a way to turn that text
    /// into a URL kettle would otherwise refuse.
    ///
    /// Uses Lua's built-in `string.match` for pattern compatibility
    /// with Terminator's URLHandler regex semantics (which are
    /// Python-flavored, but Lua patterns are similar enough for
    /// the common URL shapes — alternation isn't supported but most
    /// URL handlers don't need it).
    pub fn try_url_handler(&self, url: &str) -> UrlHandlerOutcome {
        self.arm_budget();
        let r: mlua::Result<UrlHandlerOutcome> = (|| {
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
                // A bad pattern is this handler's problem and nobody else's.
                // `string.match` raises on malformed patterns (an unclosed
                // `[`, say), and propagating that error abandoned the whole
                // loop — so registering one broken pattern disabled every
                // handler after it, and the user saw only a log line.
                // Registration checks length and UTF-8 but cannot check the
                // pattern, since Lua patterns are only validated on use.
                let m: mlua::Value = match s.call((url, pattern.as_str())) {
                    Ok(v) => v,
                    Err(e) => {
                        log::warn!("lua url_handler {i} has an unusable pattern, skipping it: {e}");
                        continue;
                    }
                };
                if matches!(m, mlua::Value::Nil) {
                    continue;
                }
                let cb: mlua::Function = entry.get("callback")?;
                match cb.call::<mlua::Value>(url.to_string()) {
                    Ok(mlua::Value::String(rewritten)) => {
                        let text = rewritten.to_string_lossy();
                        let text = text.trim();
                        if !text.is_empty() {
                            return Ok(UrlHandlerOutcome::Open(text.to_string()));
                        }
                        // An empty string says nothing; treat it as a decline
                        // rather than opening the empty URL.
                    }
                    Ok(mlua::Value::Boolean(true)) => return Ok(UrlHandlerOutcome::Handled),
                    // `false`, `nil`, no return at all, or anything else:
                    // declined, try the next handler.
                    Ok(_) => {}
                    Err(e) => {
                        log::warn!(
                            "lua url_handler callback {i} failed, falling back to                              kettle's own open: {e}"
                        );
                    }
                }
            }
            Ok(UrlHandlerOutcome::Fallthrough)
        })();
        r.unwrap_or_else(|e| {
            log::warn!("lua try_url_handler: {e}");
            UrlHandlerOutcome::Fallthrough
        })
    }

    /// Fire a named event to every Lua callback registered
    /// for it. Args are converted from `&str` for simplicity (every
    /// current event payload fits as a single string; future events
    /// can extend with a richer arg type).
    ///
    /// Errors from individual callbacks log::warn but DON'T abort
    /// kettle — one broken plugin can't take down the terminal.
    pub fn fire_event(&self, event: &LuaEvent) {
        self.arm_budget();
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
                        LuaEvent::Bell(pane_id) | LuaEvent::PaneClose(pane_id) => cb.call(*pane_id),
                        LuaEvent::Output(pane_id, bytes) => {
                            // Send bytes as a Lua string (UTF-8 not
                            // assumed — raw bytes are fine, callbacks
                            // can string.byte / string.sub them).
                            cb.call((*pane_id, bytes.as_slice()))
                        }
                        LuaEvent::PaneFocus(prev, cur) => {
                            // Map Option<u64> -> mlua::Value (Nil or
                            // Integer) so user code can write
                            // `function(prev, cur) if prev == nil ...`
                            // The cur id is always non-nil; nil only
                            // signals "first focus after startup".
                            let prev_val: mlua::Value = match prev {
                                Some(id) => mlua::Value::Integer(*id as i64),
                                None => mlua::Value::Nil,
                            };
                            cb.call((prev_val, *cur))
                        }
                        LuaEvent::TitleChanged(pane_id, title) => {
                            cb.call((*pane_id, title.as_str()))
                        }
                        LuaEvent::UrlClicked(uri) => cb.call(uri.as_str()),
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

    /// Drain pending side-effect commands queued by Lua
    /// during the most recent script execution. Returns whatever
    /// the script accumulated; the buffer (and the `SendText`
    /// aggregate-byte counter it was gating) is reset.
    pub fn drain_commands(&self) -> Vec<LuaCommand> {
        self.pending
            .lock()
            .map(|mut q| {
                q.send_text_bytes = 0;
                std::mem::take(&mut q.commands)
            })
            .unwrap_or_default()
    }

    /// Run the contents of a Lua file. Errors bubble up via anyhow
    /// with the script path attached so the user sees which file
    /// failed.
    ///
    /// Bound the read at 4 MiB. Real init.lua files run
    /// a few KB to ~100 KB; pulling in a moderately complex plugin
    /// suite might reach a few hundred KB. 4 MiB is a generous
    /// margin while still catching a swap-attack blob (same
    /// defense-in-depth pattern as the session.json and config
    /// read-size caps). Past the cap the script is refused with an
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
        self.arm_budget();
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
        self.arm_budget();
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
    fn on_rejects_unknown_events_without_creating_registry_keys() {
        let eng = LuaEngine::new("Default").expect("init");

        let accepted = eng
            .eval_str(
                "local accepted = 0
                 for i = 1, 1000 do
                    if kettle.on('unknown_' .. i, function() end) then
                        accepted = accepted + 1
                    end
                 end
                 return accepted",
            )
            .expect("unknown registrations return admission results");
        assert_eq!(accepted, "0");

        let events: mlua::Table = eng
            .lua
            .named_registry_value("kettle_events")
            .expect("event registry");
        assert_eq!(
            events
                .clone()
                .pairs::<String, mlua::Value>()
                .collect::<mlua::Result<Vec<_>>>()
                .expect("read event registry")
                .len(),
            0,
            "unknown names must not create registry tables"
        );

        assert_eq!(
            eng.eval_str(
                "local names = {
                    'startup', 'tab_add', 'tab_close', 'bell', 'pane_close',
                    'output', 'pane_focus', 'title_changed', 'url_clicked'
                 }
                 for _, name in ipairs(names) do
                    if not kettle.on(name, function() end) then
                        return false
                    end
                 end
                 return true",
            )
            .expect("implemented event names register"),
            "true"
        );
        let mut registered_names = events
            .pairs::<String, mlua::Value>()
            .map(|entry| entry.map(|(name, _)| name))
            .collect::<mlua::Result<Vec<_>>>()
            .expect("read registered event names");
        registered_names.sort();
        assert_eq!(
            registered_names,
            [
                "bell",
                "output",
                "pane_close",
                "pane_focus",
                "startup",
                "tab_add",
                "tab_close",
                "title_changed",
                "url_clicked",
            ]
        );
    }

    #[test]
    fn on_accepts_exact_per_event_cap_and_rejects_the_next_callback() {
        let eng = LuaEngine::new("Default").expect("init");
        let result = eng
            .eval_str(&format!(
                "cap_fired = 0
                 for i = 1, {MAX_LUA_CALLBACKS_PER_EVENT} do
                    if not kettle.on('output', function()
                        cap_fired = cap_fired + 1
                    end) then
                        return 'rejected_at_' .. i
                    end
                 end
                 if kettle.on('output', function()
                    cap_fired = cap_fired + 1000
                 end) then
                    return 'accepted_past_cap'
                 end
                 if not kettle.on('startup', function() end) then
                    return 'cap_was_not_per_event'
                 end
                 return 'bounded'"
            ))
            .expect("register event callbacks at boundary");
        assert_eq!(result, "bounded");

        let events: mlua::Table = eng
            .lua
            .named_registry_value("kettle_events")
            .expect("event registry");
        let output: mlua::Table = events.get("output").expect("output callbacks");
        assert_eq!(
            output.len().expect("output callback count") as usize,
            MAX_LUA_CALLBACKS_PER_EVENT
        );
        eng.fire_event(&LuaEvent::Output(1, b"x".to_vec()));
        assert_eq!(
            eng.eval_str("return cap_fired").expect("callback count"),
            MAX_LUA_CALLBACKS_PER_EVENT.to_string()
        );
    }

    #[test]
    fn menu_and_url_registries_accept_exact_caps_and_reject_the_next_entry() {
        let eng = LuaEngine::new("Default").expect("init");
        assert_eq!(
            eng.eval_str(&format!(
                "for i = 1, {MAX_LUA_MENU_ITEMS} do
                    if not kettle.add_menu_item('item ' .. i, function() end) then
                        return 'menu_rejected_at_' .. i
                    end
                 end
                 return kettle.add_menu_item('past cap', function() end)"
            ))
            .expect("register menu items at boundary"),
            "false"
        );
        assert_eq!(
            eng.list_menu_item_labels().expect("menu labels").len(),
            MAX_LUA_MENU_ITEMS
        );

        assert_eq!(
            eng.eval_str(&format!(
                "for i = 1, {MAX_LUA_URL_HANDLERS} do
                    if not kettle.add_url_handler(
                        'handler ' .. i, 'https://example%.com/' .. i, function() end
                    ) then
                        return 'url_rejected_at_' .. i
                    end
                 end
                 return kettle.add_url_handler(
                    'past cap', 'https://example%.com/past', function() end
                 )"
            ))
            .expect("register URL handlers at boundary"),
            "false"
        );
        let handlers: mlua::Table = eng
            .lua
            .named_registry_value("kettle_url_handlers")
            .expect("URL-handler registry");
        assert_eq!(
            handlers.len().expect("URL-handler count") as usize,
            MAX_LUA_URL_HANDLERS
        );
    }

    #[test]
    fn registry_strings_accept_exact_byte_caps_and_reject_oversize_values() {
        let eng = LuaEngine::new("Default").expect("init");
        assert_eq!(
            eng.eval_str(&format!(
                "return kettle.add_menu_item(
                    string.rep('M', {MAX_LUA_MENU_LABEL_BYTES}), function() end
                 )"
            ))
            .expect("exact-cap menu label"),
            "true"
        );
        assert_eq!(
            eng.eval_str(&format!(
                "return kettle.add_menu_item(
                    string.char(255) .. string.rep('M', {MAX_LUA_MENU_LABEL_BYTES}),
                    function() end
                 )"
            ))
            .expect("oversize menu label is rejected before UTF-8 conversion"),
            "false"
        );

        assert_eq!(
            eng.eval_str(&format!(
                "return kettle.add_url_handler(
                    string.rep('N', {MAX_LUA_URL_HANDLER_NAME_BYTES}),
                    'https://example%.com/.*',
                    function() end
                 )"
            ))
            .expect("exact-cap URL-handler name"),
            "true"
        );
        assert_eq!(
            eng.eval_str(&format!(
                "return kettle.add_url_handler(
                    string.char(255) ..
                        string.rep('N', {MAX_LUA_URL_HANDLER_NAME_BYTES}),
                    'https://example%.com/.*',
                    function() end
                 )"
            ))
            .expect("oversize URL-handler name is rejected before UTF-8 conversion"),
            "false"
        );

        assert_eq!(
            eng.eval_str(&format!(
                "return kettle.add_url_handler(
                    'exact pattern',
                    string.rep('P', {MAX_LUA_URL_PATTERN_BYTES}),
                    function() end
                 )"
            ))
            .expect("exact-cap URL pattern"),
            "true"
        );
        assert_eq!(
            eng.eval_str(&format!(
                "return kettle.add_url_handler(
                    'oversize pattern',
                    string.char(255) .. string.rep('P', {MAX_LUA_URL_PATTERN_BYTES}),
                    function() end
                 )"
            ))
            .expect("oversize URL pattern is rejected before UTF-8 conversion"),
            "false"
        );

        let labels = eng.list_menu_item_labels().expect("menu labels");
        assert_eq!(labels.len(), 1);
        assert_eq!(labels[0].len(), MAX_LUA_MENU_LABEL_BYTES);
        let handlers: mlua::Table = eng
            .lua
            .named_registry_value("kettle_url_handlers")
            .expect("URL-handler registry");
        assert_eq!(handlers.len().expect("URL-handler count"), 2);
    }

    #[test]
    fn kettle_namespace_exposes_version_and_theme() {
        // The minimum-viable read-only Lua
        // surface: a user's init.lua can call kettle.version() +
        // kettle.theme() to print which kettle they're running.
        let eng = LuaEngine::new("TokyoNight Night").expect("init");
        let v = eng.eval_str("return kettle.version()").expect("version");
        assert_eq!(v, env!("CARGO_PKG_VERSION"));
        let t = eng.eval_str("return kettle.theme()").expect("theme");
        assert_eq!(t, "TokyoNight Night");
    }

    #[test]
    fn kettle_theme_tracks_successfully_applied_runtime_theme() {
        let eng = LuaEngine::new("Default").expect("init");
        eng.set_active_theme("Solarized Dark")
            .expect("synchronize active theme");
        assert_eq!(
            eng.eval_str("return kettle.theme()").expect("theme"),
            "Solarized Dark"
        );
    }

    #[test]
    fn kettle_config_path_returns_a_string_or_nil() {
        // `kettle.config_path()`
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
        // `kettle.send_text(s)` queues a
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
        // `kettle.exec_action(name)` queues
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
        // `kettle.notify(title, body?)` must
        // queue a `LuaCommand::Notify` with the title + optional
        // body; the `drain_lua_hook_commands` helper
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
        // `kettle.set_theme(name)` must queue
        // a `LuaCommand::SetTheme` with the name; the
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
        // kettle.on('startup', fn) registers
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

    /// `LuaEvent::PaneFocus(prev_opt, cur)`
    /// emits to Lua as `(prev|nil, cur)` so plugins can branch on
    /// the first focus after startup (`prev == nil`) vs.
    /// subsequent focus changes.
    #[test]
    fn pane_focus_event_emits_optional_prev_and_current() {
        let eng = LuaEngine::new("Default").expect("init");
        eng.eval_str(
            "history = {}
             kettle.on('pane_focus', function(prev, cur)
                history[#history + 1] = (prev or 'nil') .. '->' .. cur
             end)",
        )
        .expect("eval");
        // First focus after startup: prev is None / Lua nil.
        eng.fire_event(&LuaEvent::PaneFocus(None, 42));
        // Subsequent focus change: prev is the previous pane id.
        eng.fire_event(&LuaEvent::PaneFocus(Some(42), 17));
        eng.fire_event(&LuaEvent::PaneFocus(Some(17), 42));
        assert_eq!(eng.eval_str("return #history").unwrap(), "3");
        assert_eq!(eng.eval_str("return history[1]").unwrap(), "nil->42");
        assert_eq!(eng.eval_str("return history[2]").unwrap(), "42->17");
        assert_eq!(eng.eval_str("return history[3]").unwrap(), "17->42");
        // Name resolves correctly (script-facing).
        assert_eq!(LuaEvent::PaneFocus(None, 1).name(), "pane_focus");
    }

    /// `LuaEvent::PaneClose(pane_id)` emits to Lua as a
    /// single integer pane id (the pane analog of `tab_close`), so plugins can
    /// drop per-pane state when a split closes.
    #[test]
    fn pane_close_event_emits_pane_id() {
        let eng = LuaEngine::new("Default").expect("init");
        // Script-facing name.
        assert_eq!(LuaEvent::PaneClose(1).name(), "pane_close");
        eng.eval_str(
            "closed = {}
             kettle.on('pane_close', function(id)
                closed[#closed + 1] = id
             end)",
        )
        .expect("eval");
        eng.fire_event(&LuaEvent::PaneClose(7));
        eng.fire_event(&LuaEvent::PaneClose(13));
        assert_eq!(eng.eval_str("return #closed").unwrap(), "2");
        assert_eq!(eng.eval_str("return closed[1]").unwrap(), "7");
        assert_eq!(eng.eval_str("return closed[2]").unwrap(), "13");
        // No subscribers is a no-op (doesn't panic).
        let eng2 = LuaEngine::new("Default").expect("init");
        eng2.fire_event(&LuaEvent::PaneClose(99));
    }

    /// `LuaEvent::TitleChanged(pane_id,
    /// title)` emits to Lua as `(pane_id, title_string)` so
    /// plugins can mirror titles into status bars.
    #[test]
    fn title_changed_event_emits_pane_id_and_title() {
        let eng = LuaEngine::new("Default").expect("init");
        eng.eval_str(
            "titles = {}
             kettle.on('title_changed', function(id, t)
                titles[#titles + 1] = id .. ':' .. t
             end)",
        )
        .expect("eval");
        eng.fire_event(&LuaEvent::TitleChanged(7, "kettle".into()));
        eng.fire_event(&LuaEvent::TitleChanged(7, "$ vim main.rs".into()));
        eng.fire_event(&LuaEvent::TitleChanged(13, "ssh prod".into()));
        assert_eq!(eng.eval_str("return #titles").unwrap(), "3");
        assert_eq!(eng.eval_str("return titles[1]").unwrap(), "7:kettle");
        assert_eq!(eng.eval_str("return titles[2]").unwrap(), "7:$ vim main.rs");
        assert_eq!(eng.eval_str("return titles[3]").unwrap(), "13:ssh prod");
        assert_eq!(
            LuaEvent::TitleChanged(0, String::new()).name(),
            "title_changed"
        );
    }

    /// `LuaEvent::UrlClicked(uri)` emits
    /// to Lua as `(uri_string,)` and is fired BEFORE pattern-
    /// handler dispatch — analytics plugins see every URL click.
    #[test]
    fn url_clicked_event_emits_uri() {
        let eng = LuaEngine::new("Default").expect("init");
        eng.eval_str(
            "urls = {}
             kettle.on('url_clicked', function(uri)
                urls[#urls + 1] = uri
             end)",
        )
        .expect("eval");
        eng.fire_event(&LuaEvent::UrlClicked("https://kettle.dev".into()));
        eng.fire_event(&LuaEvent::UrlClicked("file:///tmp/foo.txt".into()));
        eng.fire_event(&LuaEvent::UrlClicked("mailto:user@example.com".into()));
        assert_eq!(eng.eval_str("return #urls").unwrap(), "3");
        assert_eq!(
            eng.eval_str("return urls[1]").unwrap(),
            "https://kettle.dev"
        );
        assert_eq!(
            eng.eval_str("return urls[2]").unwrap(),
            "file:///tmp/foo.txt"
        );
        assert_eq!(
            eng.eval_str("return urls[3]").unwrap(),
            "mailto:user@example.com"
        );
        assert_eq!(LuaEvent::UrlClicked(String::new()).name(), "url_clicked");
    }

    #[test]
    fn on_event_hook_isolates_callback_errors() {
        // A callback that raises a Lua error doesn't
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
        // `kettle.add_menu_item(label, cb)`
        // appends a {label, callback} entry to the
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

    /// A runaway script (`while true do end`) must be
    /// force-aborted by the instruction-budget hook instead of wedging the
    /// thread forever. The test dials the budget down so the abort happens in a
    /// few million instructions (sub-second) rather than the ~128 M prod cap.
    #[test]
    fn runaway_script_aborted_by_instruction_budget() {
        // ≈2 M instructions before abort (2 hook fires × 1 M interval).
        let eng = LuaEngine::new_with_max_hook_fires("Default", 2).expect("init");
        let err = eng
            .eval_str("while true do end")
            .expect_err("an infinite loop must be aborted, not hang");
        let msg = format!("{err:?}").to_lowercase();
        assert!(
            msg.contains("budget") || msg.contains("instruction"),
            "abort error should mention the budget: {err:?}"
        );
        // The VM survives the abort: a normal expression still evaluates (the
        // budget is re-armed per invocation).
        assert_eq!(eng.eval_str("return 1 + 1").unwrap(), "2");
    }

    /// The budget is per-invocation — a tight-but-finite loop that
    /// fits well under the cap runs to completion across repeated calls without
    /// a cumulative counter eventually tripping it.
    #[test]
    fn finite_loops_run_within_budget() {
        let eng = LuaEngine::new("Default").expect("init");
        for _ in 0..5 {
            // ~50k iterations — far below one hook interval (1 M instructions).
            assert_eq!(
                eng.eval_str("local s=0 for i=1,50000 do s=s+i end return s")
                    .unwrap(),
                "1250025000"
            );
        }
    }

    #[test]
    fn url_handler_matches_pattern_and_short_circuits() {
        // `kettle.add_url_handler(name,
        // pattern, cb)` registers a handler;
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
        // Matching URL → the callback runs. It returns nothing, which per the
        // documented contract declines, so kettle still opens the original.
        assert_eq!(
            eng.try_url_handler("https://github.com/kettle"),
            UrlHandlerOutcome::Fallthrough
        );
        assert_eq!(eng.eval_str("return github_hits").unwrap(), "1");
        // Non-matching URL → the callback does not run at all.
        assert_eq!(
            eng.try_url_handler("https://example.com/"),
            UrlHandlerOutcome::Fallthrough
        );
        assert_eq!(eng.eval_str("return github_hits").unwrap(), "1");
    }

    /// A handler must never be the reason a link stops opening.
    ///
    /// A matching handler used to claim the URL no matter what happened
    /// inside it — the callback's result was discarded as `mlua::Result<()>`
    /// and an error was logged and ignored. So a plugin with a typo in it
    /// made every URL matching its pattern silently unopenable: the click did
    /// nothing, and the only trace was a log line. Both an error and an
    /// explicit decline now fall through to the next handler, and finally to
    /// kettle's own open.
    #[test]
    fn a_broken_or_declining_url_handler_leaves_the_link_working() {
        let eng = LuaEngine::new("Default").expect("init");
        eng.eval_str(
            "kettle.add_url_handler('boom', 'https://boom%.example/.*',
                 function(_url) error('this plugin has a bug') end)
             kettle.add_url_handler('decline', 'https://decline%.example/.*',
                 function(_url) return false end)
             claimed_hits = 0
             kettle.add_url_handler('claim', 'https://claim%.example/.*',
                 function(_url) claimed_hits = claimed_hits + 1 return true end)",
        )
        .expect("eval");

        assert_eq!(
            eng.try_url_handler("https://boom.example/x"),
            UrlHandlerOutcome::Fallthrough,
            "a handler that raised must not swallow the URL"
        );
        assert_eq!(
            eng.try_url_handler("https://decline.example/x"),
            UrlHandlerOutcome::Fallthrough,
            "a handler that returned false declined it"
        );
        // A handler that opened the URL itself says so with `true`, and then
        // kettle must not open it a second time.
        assert_eq!(
            eng.try_url_handler("https://claim.example/x"),
            UrlHandlerOutcome::Handled
        );
        assert_eq!(eng.eval_str("return claimed_hits").unwrap(), "1");
    }

    /// A failing handler must not stop a LATER one from claiming the URL —
    /// `continue`, not `return`. Two plugins can register overlapping
    /// patterns, and the first one being broken must not disable the second.
    /// The shape every handler in `docs/examples/init.lua` uses: match some
    /// text, return a REWRITTEN URL, and kettle opens that instead.
    ///
    /// The returned string used to be discarded — the result was typed
    /// `mlua::Result<()>` — while the handler still claimed the URL. So every
    /// example we ship (`LP: #12345` → Launchpad, `lp:branch` → code.launchpad,
    /// `apt://gimp`) matched, ran, produced the right URL, and then opened
    /// nothing at all. Copying the documented file gave you dead links.
    #[test]
    fn a_handler_that_rewrites_the_url_gets_that_url_opened() {
        let eng = LuaEngine::new("Default").expect("init");
        // Lifted from docs/examples/init.lua.
        eng.eval_str(
            "kettle.add_url_handler('launchpad_bug', '%f[%w][lL][pP]:?%s?#?(%d+)',
                 function(text)
                     local num = text:match('(%d+)')
                     if num then
                         return 'https://bugs.launchpad.net/bugs/' .. num
                     end
                 end)",
        )
        .expect("eval");
        assert_eq!(
            eng.try_url_handler("LP: #12345"),
            UrlHandlerOutcome::Open("https://bugs.launchpad.net/bugs/12345".to_string()),
            "a rewritten URL must reach the opener, not be thrown away"
        );

        // A handler whose branch does not fire returns nil, which declines —
        // the original text is left to kettle rather than being swallowed.
        assert_eq!(
            eng.try_url_handler("LP: none"),
            UrlHandlerOutcome::Fallthrough
        );
    }

    /// An empty string is not a URL. Returning one must decline rather than
    /// ask the opener to launch nothing.
    #[test]
    fn a_handler_returning_an_empty_string_declines() {
        let eng = LuaEngine::new("Default").expect("init");
        eng.eval_str(
            "kettle.add_url_handler('blank', 'https://blank%.example/.*',
                 function(_url) return '   ' end)",
        )
        .expect("eval");
        assert_eq!(
            eng.try_url_handler("https://blank.example/x"),
            UrlHandlerOutcome::Fallthrough
        );
    }

    /// A broken PATTERN must not disable the handlers registered after it.
    ///
    /// Lua patterns are validated on use, not at registration, so
    /// `string.match` raises on something like an unclosed `[`. That error
    /// used to propagate out of the loop, so one bad pattern silently
    /// disabled every later handler — and the only trace was a log line.
    #[test]
    fn an_unusable_pattern_does_not_disable_the_handlers_after_it() {
        let eng = LuaEngine::new("Default").expect("init");
        eng.eval_str(
            "kettle.add_url_handler('broken_pattern', '[', function(_url) return true end)
             kettle.add_url_handler('good', 'https://good%.example/.*',
                 function(_url) return 'https://rewritten.example/ok' end)",
        )
        .expect("eval");
        assert_eq!(
            eng.try_url_handler("https://good.example/x"),
            UrlHandlerOutcome::Open("https://rewritten.example/ok".to_string()),
            "a handler registered after a broken pattern must still run"
        );
    }

    #[test]
    fn a_failing_handler_does_not_block_a_later_match() {
        let eng = LuaEngine::new("Default").expect("init");
        eng.eval_str(
            "rescued = 0
             kettle.add_url_handler('first_broken', 'https://both%.example/.*',
                 function(_url) error('bug') end)
             kettle.add_url_handler('second_good', 'https://both%.example/.*',
                 function(_url) rescued = rescued + 1 return true end)",
        )
        .expect("eval");
        assert_eq!(
            eng.try_url_handler("https://both.example/x"),
            UrlHandlerOutcome::Handled,
            "the second handler must still get its chance"
        );
        assert_eq!(eng.eval_str("return rescued").unwrap(), "1");
    }

    #[test]
    fn exec_file_runs_a_real_script() {
        // The script writes a result into a global so the test can
        // check it ran. Same shape as a user's `~/.config/kettle/
        // init.lua` setting up environment.
        // PID + nanos so parallel `cargo test` runs don't
        // race on a shared /tmp path. Same pattern as the
        // oversize-script test below.
        let path = std::env::temp_dir().join(format!(
            "kettle-lua-smoke-{}-{}.lua",
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

    /// An oversized Lua script must be refused
    /// via the metadata pre-check rather than read into RAM. Real
    /// init.lua files top out around 100 KB; a multi-MB blob is either
    /// a runaway autogen script or a swap-attack — either way the
    /// load should fail loud rather than OOM mid-load.
    #[test]
    fn exec_file_rejects_oversize_script() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("kettle-lua-oversize-{}.lua", std::process::id()));
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

    /// `lua-sandbox = safe` nils `os.execute` and `io.popen`, which reads like
    /// "a safe-mode plugin cannot run programs". It cannot: `kettle.send_text`
    /// types into the focused shell, and a newline in that text runs what it
    /// typed — the shipped example plugin clears the screen exactly that way.
    /// Safe mode is a guard against a careless plugin, not a container for a
    /// hostile one, and `restricted` is the level that actually holds: the two
    /// APIs that drive the terminal refuse, and everything else still works.
    #[test]
    fn only_the_restricted_level_stops_a_plugin_driving_the_terminal() {
        use kettle_config::LuaSandbox;

        // Precondition: safe mode nils the obvious process-spawning API, so
        // the point below is about what that guarantee does NOT cover.
        let safe = LuaEngine::new_with_sandbox("Default", LuaSandbox::Safe).expect("init safe");
        assert_eq!(safe.eval_str("return type(os.execute)").unwrap(), "nil");
        // ...and yet it can type a command and press return.
        assert_eq!(
            safe.eval_str("return kettle.send_text('whoami\\n')")
                .unwrap(),
            "true",
            "safe mode accepts terminal input — this is the documented API, \
             not an oversight; it is why safe mode is not a containment level"
        );
        assert_eq!(
            safe.eval_str("return kettle.exec_action('new_tab')")
                .unwrap(),
            "true"
        );
        assert_eq!(
            safe.drain_commands().len(),
            2,
            "both side effects reached the queue"
        );

        // Restricted refuses both, and says so in the return value rather than
        // pretending to have queued the work.
        let restricted = LuaEngine::new_with_sandbox("Default", LuaSandbox::Restricted)
            .expect("init restricted");
        assert_eq!(
            restricted
                .eval_str("return kettle.send_text('whoami\\n')")
                .unwrap(),
            "false"
        );
        assert_eq!(
            restricted
                .eval_str("return kettle.exec_action('new_tab')")
                .unwrap(),
            "false"
        );
        assert!(
            restricted.drain_commands().is_empty(),
            "a refused call must queue nothing at all"
        );
        // The stdlib nils still apply — restricted is strictly stricter.
        assert_eq!(
            restricted.eval_str("return type(os.execute)").unwrap(),
            "nil"
        );
        // And the rest of the API is untouched: a restricted plugin can still
        // observe and report, which is the whole point of having the level.
        assert_eq!(
            restricted.eval_str("return type(kettle.version)").unwrap(),
            "function"
        );
        assert_eq!(
            restricted
                .eval_str("return kettle.notify('title', 'body')")
                .unwrap(),
            "true"
        );
    }

    #[test]
    fn safe_sandbox_nils_dangerous_stdlib_apis() {
        // The safe-mode sandbox (default
        // `lua-sandbox = safe`) nils the Lua stdlib APIs that can
        // execute external processes, open arbitrary files, or load
        // native shared libraries. A future refactor removing one of
        // these nils silently degrades the security posture documented
        // in SECURITY.md's "Lua plugin sandbox escape" in-scope entry.
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
        // The opt-in `lua-sandbox =
        // trusted` mode leaves the dangerous APIs callable. Users
        // explicitly setting `trusted` accept the full Lua stdlib
        // surface (per SECURITY.md's "Lua plugin sandbox escape" entry:
        // opt-in trust is out-of-scope for sandbox-escape reports).
        // A future refactor that nils
        // these even in trusted mode silently breaks user scripts.
        let eng = LuaEngine::new_with_sandbox("Default", kettle_config::LuaSandbox::Trusted)
            .expect("init (trusted sandbox)");
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

    /// Pin that mlua's `Lua::new()` default
    /// excludes the entire `debug` library (per its `StdLib::ALL_SAFE`
    /// load list). If a future refactor switches to `Lua::unsafe_new()`
    /// or explicitly loads `StdLib::DEBUG`, the dangerous methods
    /// (`debug.getregistry` reaches into mlua's reference table,
    /// `debug.sethook` is an instruction-level DoS hook, `debug.set*`
    /// breaks opaque-userdata encapsulation) would silently become
    /// reachable from user scripts. This test catches that at every
    /// trust level — none of them is meant to expose the debug surface.
    #[test]
    fn lua_default_globals_exclude_debug_library() {
        for sandbox in [
            kettle_config::LuaSandbox::Restricted,
            kettle_config::LuaSandbox::Safe,
            kettle_config::LuaSandbox::Trusted,
        ] {
            let eng = LuaEngine::new_with_sandbox("Default", sandbox).expect("init");
            assert_eq!(
                eng.eval_str("return type(debug)").unwrap(),
                "nil",
                "{sandbox:?}: `debug` library must be nil at the global \
                 level — mlua's Lua::new() defaults exclude it. If a future \
                 refactor switches to Lua::unsafe_new() or loads StdLib::DEBUG, \
                 update SECURITY.md's Lua plugin sandbox escape notes accordingly"
            );
        }
    }

    /// `kettle.send_text(s)` must drop
    /// strings larger than `MAX_LUA_SEND_TEXT_BYTES`. Pre-cap, a
    /// hostile script (or even a buggy legitimate one that runs
    /// away on a loop) could queue gigabytes of PTY-bound text and
    /// OOM kettle at the App's drain step. Verifies the rejection
    /// behavior: the call returns false without raising a Lua error
    /// and the queue stays empty.
    #[test]
    fn send_text_reports_oversized_payload_rejection() {
        let eng = LuaEngine::new("Default").expect("init");
        // 1 MiB + 1 byte — one byte over the cap.
        let oversize = (super::MAX_LUA_SEND_TEXT_BYTES + 1).to_string();
        assert_eq!(
            eng.eval_str(&format!(
                "return kettle.send_text(string.rep('X', {oversize}))"
            ))
            .expect("oversized send returns an admission result"),
            "false"
        );
        assert_eq!(
            eng.eval_str("return kettle.send_text('legit')")
                .expect("legitimate send returns an admission result"),
            "true"
        );
        let cmds = eng.drain_commands();
        // Exactly one command queued — the second (legit) call. The
        // oversize first call must drop without queueing.
        assert_eq!(cmds.len(), 1, "queue: {cmds:?}");
        assert!(
            matches!(&cmds[0], LuaCommand::SendText(s) if s == "legit"),
            "expected only the 'legit' send_text to queue, got {cmds:?}"
        );
    }

    /// `kettle.notify(title, body)` rejects
    /// each field over `MAX_LUA_NOTIFY_BYTES`. Real desktop
    /// notifications are tiny; oversized title/body almost certainly
    /// indicates a runaway script.
    #[test]
    fn notify_reports_oversized_field_rejection() {
        let eng = LuaEngine::new("Default").expect("init");
        let oversize = (super::MAX_LUA_NOTIFY_BYTES + 1).to_string();
        // Oversize title → drop.
        assert_eq!(
            eng.eval_str(&format!(
                "return kettle.notify(string.rep('T', {oversize}), 'body')"
            ))
            .expect("oversized title returns an admission result"),
            "false"
        );
        // Oversize body → drop.
        assert_eq!(
            eng.eval_str(&format!(
                "return kettle.notify('title', string.rep('B', {oversize}))"
            ))
            .expect("oversized body returns an admission result"),
            "false"
        );
        // Sane call → admitted.
        assert_eq!(
            eng.eval_str("return kettle.notify('ok', 'sane')")
                .expect("sane notification returns an admission result"),
            "true"
        );
        let cmds = eng.drain_commands();
        assert_eq!(cmds.len(), 1, "queue: {cmds:?}");
        assert!(
            matches!(&cmds[0], LuaCommand::Notify { title, body }
                if title == "ok" && body == "sane"),
            "expected only the sane notify to queue, got {cmds:?}"
        );
    }

    #[test]
    fn action_and_theme_names_are_bounded_and_report_rejection() {
        let eng = LuaEngine::new("Default").expect("init");
        let action_oversize = super::MAX_LUA_ACTION_NAME_BYTES + 1;
        let theme_oversize = super::MAX_LUA_THEME_NAME_BYTES + 1;
        assert_eq!(
            eng.eval_str(&format!(
                "return kettle.exec_action(string.rep('A', {action_oversize}))"
            ))
            .expect("oversized action returns an admission result"),
            "false"
        );
        assert_eq!(
            eng.eval_str(&format!(
                "return kettle.set_theme(string.rep('T', {theme_oversize}))"
            ))
            .expect("oversized theme returns an admission result"),
            "false"
        );
        assert_eq!(
            eng.eval_str("return kettle.exec_action('new_tab')")
                .expect("bounded action"),
            "true"
        );
        assert_eq!(
            eng.eval_str("return kettle.set_theme('Default')")
                .expect("bounded theme"),
            "true"
        );
        let commands = eng.drain_commands();
        assert_eq!(commands.len(), 2, "queue: {commands:?}");
        assert!(matches!(
            &commands[0],
            LuaCommand::ExecAction(name) if name == "new_tab"
        ));
        assert!(matches!(
            &commands[1],
            LuaCommand::SetTheme(name) if name == "Default"
        ));
    }

    /// The queue length caps at
    /// `MAX_PENDING_COMMANDS`. A hostile script firing
    /// `for i=1,10^9 do kettle.exec_action('noop') end` would
    /// otherwise grow the Vec without bound — even at 32 bytes per
    /// entry that's 32 GB.
    #[test]
    fn pending_queue_caps_at_max_pending_commands() {
        let eng = LuaEngine::new("Default").expect("init");
        // Fire 1.5× the cap. Use exec_action with a short name so
        // each entry is small (the test is about queue *length*,
        // not per-entry size).
        let n = super::MAX_PENDING_COMMANDS + super::MAX_PENDING_COMMANDS / 2;
        assert_eq!(
            eng.eval_str(&format!(
                "local accepted = true; \
                 for _ = 1, {n} do accepted = kettle.exec_action('no_op') end; \
                 return accepted"
            ))
            .expect("script runs"),
            "false",
            "the calls after the entry cap must report rejection"
        );
        let cmds = eng.drain_commands();
        assert_eq!(
            cmds.len(),
            super::MAX_PENDING_COMMANDS,
            "queue must cap at MAX_PENDING_COMMANDS, not {}",
            cmds.len()
        );
    }

    /// The aggregate SUM of queued `SendText` bytes must cap at
    /// `MAX_LUA_PENDING_SEND_BYTES`, independent of the per-call cap
    /// (`MAX_LUA_SEND_TEXT_BYTES`) and the per-entry-count cap
    /// (`MAX_PENDING_COMMANDS`). Reproduces the audit scenario almost
    /// verbatim: a script that queues many commands each individually
    /// under the per-call cap, and far fewer than the entry-count cap,
    /// but whose combined payload would otherwise be huge once
    /// concatenated at the App's PTY-write step.
    #[test]
    fn send_text_queue_caps_aggregate_bytes() {
        let eng = LuaEngine::new("Default").expect("init");
        let chunk = super::MAX_LUA_SEND_TEXT_BYTES; // exactly at the per-call cap.
        let calls = (super::MAX_LUA_PENDING_SEND_BYTES / chunk) + 1; // one call past the aggregate cap.
        eng.eval_str(&format!(
            "for _ = 1, {calls} do kettle.send_text(string.rep('X', {chunk})) end; return true"
        ))
        .expect("script runs (over-cap sends drop silently, they don't error)");
        let cmds = eng.drain_commands();
        let expected_admitted = super::MAX_LUA_PENDING_SEND_BYTES / chunk;
        assert_eq!(
            cmds.len(),
            expected_admitted,
            "expected exactly {expected_admitted} full-size sends admitted before the \
             aggregate cap saturates, got {} (queue: lengths {:?})",
            cmds.len(),
            cmds.iter()
                .map(|c| match c {
                    LuaCommand::SendText(s) => s.len(),
                    other => panic!("unexpected non-SendText command: {other:?}"),
                })
                .collect::<Vec<_>>()
        );
        let total: usize = cmds
            .iter()
            .map(|c| match c {
                LuaCommand::SendText(s) => s.len(),
                _ => unreachable!(),
            })
            .sum();
        assert!(
            total <= super::MAX_LUA_PENDING_SEND_BYTES,
            "aggregate queued bytes {total} exceeded the cap {}",
            super::MAX_LUA_PENDING_SEND_BYTES
        );

        // While the aggregate cap is saturated, even a tiny SendText is
        // dropped — the cap gates the running total, not just individual
        // oversized calls.
        eng.eval_str(&format!(
            "for _ = 1, {calls} do kettle.send_text(string.rep('X', {chunk})) end \
             kettle.send_text('tiny'); return true"
        ))
        .expect("script runs");
        let cmds2 = eng.drain_commands();
        assert!(
            cmds2
                .iter()
                .all(|c| !matches!(c, LuaCommand::SendText(s) if s == "tiny")),
            "a tiny send_text must still be dropped once the aggregate byte \
             cap is already saturated: {cmds2:?}"
        );

        // Drain resets the running total: after draining, the queue can
        // accept fresh SendText bytes up to the cap again.
        eng.eval_str("kettle.send_text('post-drain')")
            .expect("eval");
        let cmds3 = eng.drain_commands();
        assert_eq!(cmds3.len(), 1);
        assert!(matches!(&cmds3[0], LuaCommand::SendText(s) if s == "post-drain"));
    }
}
