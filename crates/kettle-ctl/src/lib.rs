//! Cycle 925 (agent-first A2): kettle's agent control plane — protocol,
//! transport, discovery, and client.
//!
//! This crate is UI-free and engine-free: it defines the versioned NDJSON wire
//! protocol ([`protocol`]), the local-IPC transport ([`transport`], a Unix
//! socket / Windows named pipe), the server discovery registry ([`discovery`]),
//! and a blocking [`client::Client`]. The GUI hosts the *server* side
//! (kettle-ui) over this transport; `kettle ctl` and `kettle mcp` (the bin)
//! host the client side. Keeping the protocol + transport here is the
//! forward-compat seam for the future `kettle-muxd` daemon
//! (docs/MUX-SERVER-DESIGN.md): the daemon can re-host the same server side and
//! no client changes.

pub mod client;
pub mod discovery;
// Multi-window cycle: cross-process window-presence registry (Peacock accent
// dedupe). No endpoint, always on, best-effort — see the module docs.
pub mod presence;
pub mod protocol;
pub mod transport;

pub use client::{Client, CtlError};
pub use discovery::{RegistryEntry, registry_dir};
pub use protocol::{Event, Request, Response, RpcError, error_codes};
