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

/// This server's inline-card state.
#[derive(Debug, Default)]
pub(crate) struct DisplaySession {
    /// Whether the harness prints hook messages: interactive Claude Code.
    /// Headless and SDK sessions run no hook, so they get no card.
    claude_cli: bool,
    pending: Mutex<HashMap<String, (String, Instant)>>,
}

impl DisplaySession {
    /// The session of this process: cards only when Kettle's own plugin
    /// launched it, so a hook will print them, and only in an interactive
    /// Claude Code session.
    pub(crate) fn from_env(card_hook: bool) -> Self {
        let entrypoint = std::env::var("CLAUDE_CODE_ENTRYPOINT").ok();
        Self::new(cards_enabled(card_hook, entrypoint.as_deref()))
    }

    pub(crate) fn new(claude_cli: bool) -> Self {
        Self {
            claude_cli,
            pending: Mutex::new(HashMap::new()),
        }
    }

    /// The tool-use id a `kettle_show` call may have a card for: only in a
    /// session that prints hook messages, only with a plain bounded id, and
    /// only while there is room to keep its delivery.
    pub(crate) fn card_for(&self, params: &Value, now: Instant) -> Option<String> {
        if !self.claude_cli {
            return None;
        }
        let id = tool_use_id(params)?;
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

/// Whether a server asks for cards: only when Kettle's plugin launched it,
/// whose hook prints them, for an interactive Claude Code session.
fn cards_enabled(card_hook: bool, entrypoint: Option<&str>) -> bool {
    card_hook && entrypoint == Some("cli")
}

static SESSION: std::sync::OnceLock<DisplaySession> = std::sync::OnceLock::new();

/// Set this process's session before it serves: `card_hook` when Kettle's
/// own plugin launched it.
pub(crate) fn init_session(card_hook: bool) {
    let _ = SESSION.set(DisplaySession::from_env(card_hook));
}

/// The process's session; one that was never set asks for no card.
pub(crate) fn session() -> &'static DisplaySession {
    SESSION.get_or_init(|| DisplaySession::new(false))
}

/// The tool-use id `tools/call` params carry in their metadata, when it is a
/// plain bounded token. The model's arguments never supply it.
pub(crate) fn tool_use_id(params: &Value) -> Option<String> {
    let id = params.get("_meta")?.get(TOOL_USE_ID_META)?.as_str()?;
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
        let session = DisplaySession::new(true);
        assert_eq!(
            session.card_for(&call("toolu_01AbC-9"), now).as_deref(),
            Some("toolu_01AbC-9")
        );
        assert_eq!(
            DisplaySession::new(false).card_for(&call("toolu_1"), now),
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
        assert!(cards_enabled(true, Some("cli")));
        // Even an interactive Claude Code session asks for none unless
        // Kettle's plugin, whose hook prints cards, launched the server.
        assert!(!cards_enabled(false, Some("cli")));
        // Headless and SDK sessions run no hook.
        for entrypoint in [None, Some("sdk-cli"), Some("sdk-ts"), Some("")] {
            assert!(!cards_enabled(true, entrypoint), "{entrypoint:?}");
            assert!(!cards_enabled(false, entrypoint), "{entrypoint:?}");
        }
    }

    #[test]
    fn a_delivery_is_taken_once_and_waits_a_few_seconds() {
        let now = Instant::now();
        let session = DisplaySession::new(true);
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
        let session = DisplaySession::new(true);
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
