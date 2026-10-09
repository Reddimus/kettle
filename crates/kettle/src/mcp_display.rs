//! Inline cards' private side in `kettle mcp --display`: whether this server
//! serves a session that prints hook messages, the card deliveries waiting
//! for their hook, and the one-time retrieval the hook makes.
//!
//! A delivery is Kettle's own card text for one tool call. It never reaches
//! the model: the model's result says only that the media was sent, and the
//! hook retrieves the text once, by the call's tool-use id, within a few
//! seconds.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::Value;

/// Most deliveries waiting at once.
pub(crate) const MAX_PENDING_CARDS: usize = 16;
/// How long a delivery waits for its hook. The hook runs as the call
/// completes, so a delivery older than this belongs to a hook that will not
/// come.
pub(crate) const PENDING_LIFETIME: Duration = Duration::from_secs(5);
/// Longest tool-use id accepted.
pub(crate) const MAX_TOOL_USE_ID_BYTES: usize = 128;
/// Where Claude Code puts a model call's tool-use id in `tools/call`.
pub(crate) const TOOL_USE_ID_META: &str = "claudecode/toolUseId";
/// Where Codex puts a model call's id in `tools/call` (Codex CLI 0.162.0);
/// its hooks get the same id as `${tool_use_id}`.
pub(crate) const CODEX_CALL_ID_META: &str = "callId";

/// Whose hook prints this server's cards.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CardHook {
    /// The `PostToolUse` hook of Kettle's Claude Code plugin.
    Claude,
    /// The `PostToolUse` hook Kettle's Codex launch adds.
    Codex,
}

impl CardHook {
    /// Where the harness puts a model call's id in `tools/call`.
    fn call_id_meta(self) -> &'static str {
        match self {
            Self::Claude => TOOL_USE_ID_META,
            Self::Codex => CODEX_CALL_ID_META,
        }
    }

    /// The card Kettle is asked for.
    pub(crate) fn target(self) -> kettle_ctl::show::InlineTarget {
        match self {
            Self::Claude => kettle_ctl::show::InlineTarget::ClaudeHook,
            Self::Codex => kettle_ctl::show::InlineTarget::CodexHook,
        }
    }
}

/// This server's inline-card state.
#[derive(Debug, Default)]
pub(crate) struct DisplaySession {
    /// The hook that prints this session's cards, if any. Headless and SDK
    /// Claude Code sessions run no hook, so they get no card.
    hook: Option<CardHook>,
    pending: Mutex<HashMap<String, (String, Instant)>>,
}

impl DisplaySession {
    /// The session of this process: cards only when Kettle launched it with
    /// a hook that prints them: Kettle's own Claude Code plugin, in an
    /// interactive session, or Kettle's Codex launch.
    pub(crate) fn from_env(hook: Option<CardHook>) -> Self {
        let entrypoint = std::env::var("CLAUDE_CODE_ENTRYPOINT").ok();
        Self::new(cards_enabled(hook, entrypoint.as_deref()))
    }

    pub(crate) fn new(hook: Option<CardHook>) -> Self {
        Self {
            hook,
            pending: Mutex::new(HashMap::new()),
        }
    }

    /// The hook that prints this session's cards.
    pub(crate) fn hook(&self) -> Option<CardHook> {
        self.hook
    }

    /// The call id a `kettle_show` call may have a card for: only in a
    /// session with a hook, only with a plain bounded id, and only while
    /// there is room to keep its delivery.
    pub(crate) fn card_for(&self, params: &Value, now: Instant) -> Option<String> {
        let id = call_id(params, self.hook?.call_id_meta())?;
        let mut pending = self.pending.lock().ok()?;
        expire(&mut pending, now);
        (pending.len() < MAX_PENDING_CARDS && !pending.contains_key(&id)).then_some(id)
    }

    /// Keep `message` for the hook of call `id`. False when it cannot be
    /// kept: the model is then told the shelf has the media, not a card.
    pub(crate) fn store(&self, id: String, message: String, now: Instant) -> bool {
        let Ok(mut pending) = self.pending.lock() else {
            return false;
        };
        expire(&mut pending, now);
        if pending.len() >= MAX_PENDING_CARDS || pending.contains_key(&id) {
            return false;
        }
        pending.insert(id, (message, now + PENDING_LIFETIME));
        true
    }

    /// The delivery for call `id`, once.
    pub(crate) fn take(&self, id: &str, now: Instant) -> Option<String> {
        let mut pending = self.pending.lock().ok()?;
        expire(&mut pending, now);
        pending.remove(id).map(|(message, _)| message)
    }
}

fn expire(pending: &mut HashMap<String, (String, Instant)>, now: Instant) {
    pending.retain(|_, (_, deadline)| now < *deadline);
}

/// The hook a server asks for cards for: Claude Code's only in an
/// interactive session; Codex's only ever runs in one, since Kettle's launch
/// adds it to interactive sessions alone.
fn cards_enabled(hook: Option<CardHook>, entrypoint: Option<&str>) -> Option<CardHook> {
    match hook? {
        CardHook::Claude => (entrypoint == Some("cli")).then_some(CardHook::Claude),
        CardHook::Codex => Some(CardHook::Codex),
    }
}

static SESSION: std::sync::OnceLock<DisplaySession> = std::sync::OnceLock::new();

/// Set this process's session before it serves: `hook` when Kettle
/// launched it with one.
pub(crate) fn init_session(hook: Option<CardHook>) {
    let _ = SESSION.set(DisplaySession::from_env(hook));
}

/// The process's session; one that was never set asks for no card.
pub(crate) fn session() -> &'static DisplaySession {
    SESSION.get_or_init(|| DisplaySession::new(None))
}

/// Whether a `tools/call` came from a model: it carries a harness's call id
/// in its metadata, which a hook's own call does not.
pub(crate) fn from_model(params: &Value) -> bool {
    params.get("_meta").is_some_and(|meta| {
        [TOOL_USE_ID_META, CODEX_CALL_ID_META]
            .iter()
            .any(|key| meta.get(*key).is_some())
    })
}

/// The call id `tools/call` params carry in their metadata under `key`,
/// when it is a plain bounded token. The model's arguments never supply it.
fn call_id(params: &Value, key: &str) -> Option<String> {
    let id = params.get("_meta")?.get(key)?.as_str()?;
    let plain = !id.is_empty()
        && id.len() <= MAX_TOOL_USE_ID_BYTES
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-');
    plain.then(|| id.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn call(id: &str) -> Value {
        json!({"name": "kettle_show", "_meta": {TOOL_USE_ID_META: id}})
    }

    #[test]
    fn only_an_interactive_session_with_a_plain_id_gets_a_card() {
        let now = Instant::now();
        let session = DisplaySession::new(Some(CardHook::Claude));
        assert_eq!(
            session.card_for(&call("toolu_01AbC-9"), now).as_deref(),
            Some("toolu_01AbC-9")
        );
        assert_eq!(
            DisplaySession::new(None).card_for(&call("toolu_1"), now),
            None
        );
        for id in [
            "",
            "has space",
            "quote\"",
            &"x".repeat(MAX_TOOL_USE_ID_BYTES + 1),
        ] {
            assert_eq!(session.card_for(&call(id), now), None, "{id:?}");
        }
        assert_eq!(session.card_for(&json!({"name": "kettle_show"}), now), None);
        assert_eq!(
            session.card_for(&json!({"arguments": {TOOL_USE_ID_META: "toolu_1"}}), now),
            None,
            "the arguments never supply the id"
        );
    }

    #[test]
    fn no_card_without_kettles_hook_and_an_interactive_session() {
        let claude = Some(CardHook::Claude);
        assert_eq!(cards_enabled(claude, Some("cli")), claude);
        // Even an interactive Claude Code session asks for none unless
        // Kettle's plugin, whose hook prints cards, launched the server.
        assert_eq!(cards_enabled(None, Some("cli")), None);
        // Headless and SDK sessions run no hook.
        for entrypoint in [None, Some("sdk-cli"), Some("sdk-ts"), Some("")] {
            assert_eq!(cards_enabled(claude, entrypoint), None, "{entrypoint:?}");
            assert_eq!(cards_enabled(None, entrypoint), None, "{entrypoint:?}");
        }
        // Kettle adds Codex's hook to interactive sessions alone, and Codex
        // passes its server no Claude Code variable.
        assert_eq!(
            cards_enabled(Some(CardHook::Codex), None),
            Some(CardHook::Codex)
        );
    }

    /// A Codex session keys its cards by Codex's call id, and only a call
    /// that carries one is the model's.
    #[test]
    fn a_codex_session_keys_cards_by_codexs_call_id() {
        let now = Instant::now();
        let session = DisplaySession::new(Some(CardHook::Codex));
        let codex = json!({"name": "kettle_show", "_meta": {CODEX_CALL_ID_META: "exec-1a2b"}});
        assert_eq!(session.card_for(&codex, now).as_deref(), Some("exec-1a2b"));
        assert_eq!(
            session.card_for(&call("toolu_1"), now),
            None,
            "Claude Code's key"
        );
        assert_eq!(
            session.hook().map(CardHook::target),
            Some(kettle_ctl::show::InlineTarget::CodexHook)
        );
        assert!(from_model(&codex));
        assert!(from_model(&call("toolu_1")));
        // A hook's own call carries only Codex's thread id.
        assert!(!from_model(
            &json!({"_meta": {"threadId": "t", "progressToken": 2}})
        ));
        assert!(!from_model(&json!({"name": "kettle_card"})));
    }

    #[test]
    fn a_delivery_is_taken_once_and_waits_a_few_seconds() {
        let now = Instant::now();
        let session = DisplaySession::new(Some(CardHook::Claude));
        assert!(session.store("toolu_1".into(), "rows".into(), now));
        assert!(
            !session.store("toolu_1".into(), "again".into(), now),
            "one per call"
        );
        assert_eq!(
            session.card_for(&call("toolu_1"), now),
            None,
            "already waiting"
        );
        assert_eq!(session.take("toolu_1", now).as_deref(), Some("rows"));
        assert_eq!(session.take("toolu_1", now), None, "once");
        assert!(session.store("toolu_2".into(), "rows".into(), now));
        assert_eq!(
            session.take("toolu_2", now + PENDING_LIFETIME),
            None,
            "expired"
        );
    }

    #[test]
    fn pending_deliveries_are_bounded() {
        let now = Instant::now();
        let session = DisplaySession::new(Some(CardHook::Claude));
        for n in 0..MAX_PENDING_CARDS {
            assert!(session.store(format!("toolu_{n}"), "rows".into(), now));
        }
        assert!(!session.store("toolu_x".into(), "rows".into(), now));
        assert_eq!(session.card_for(&call("toolu_x"), now), None);
        // Expired ones make room.
        let later = now + PENDING_LIFETIME;
        assert!(session.store("toolu_x".into(), "rows".into(), later));
    }
}
