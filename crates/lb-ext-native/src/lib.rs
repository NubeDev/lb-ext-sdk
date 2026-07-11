//! The native (Tier-2) extension SDK — the child side of the host↔sidecar wire.
//!
//! A native extension is a subprocess the host (`lb-supervisor`) supervises over stdio, exchanging
//! `Content-Length`-framed JSON on a tiny closed control protocol (`init`/`health`/`call`/`shutdown`).
//! This crate is the whole runtime such an extension needs: the wire message shapes (mirrors of lb's
//! `lb-supervisor::rpc`), the framing (mirror of `lb-supervisor::frame`), the protocol-version
//! handshake, and a [`serve`] loop that owns the wire so an extension author only writes tool bodies.
//! It does not — and must not — expose the host-side supervisor; that stays in the platform (`lb`),
//! so this facade can be published without freezing host internals as public API.
//!
//! ## Writing a native extension
//!
//! ```no_run
//! use lb_ext_native::{serve_stdio, Tools};
//!
//! struct MyExt;
//! impl Tools for MyExt {
//!     fn tools(&self) -> Vec<String> { vec!["greet".into()] }
//!     async fn call(&mut self, tool: &str, input: &str) -> Result<String, String> {
//!         match tool {
//!             "greet" => Ok(format!("{{\"hello\":{input}}}")),
//!             other => Err(format!("unknown tool: {other}")),
//!         }
//!     }
//! }
//!
//! #[tokio::main]
//! async fn main() -> std::io::Result<()> {
//!     serve_stdio(MyExt).await
//! }
//! ```
//!
//! See lb's `docs/scope/extensions/ext-out-of-tree-scope.md` (the native tier) and
//! `docs/scope/extensions/native-callback-transport-scope.md`.

pub mod frame;
pub mod handshake;
pub mod serve;
pub mod stdio;
pub mod wire;

pub use handshake::{InitReply, PROTOCOL_MAJOR};
pub use serve::{serve, Tools};
pub use stdio::serve_stdio;
pub use wire::{CallParams, Method, Reply, Request};

/// The host-callback client — how a native extension calls BACK into the host's MCP surface.
///
/// The stdio wire above is host→child (the host dispatches tools *to* this extension). To call a host
/// verb the *other* direction — `SidecarClient::from_env().call_tool("authz.check_scoped", args)` — an
/// extension uses this re-exported [`lb_sidecar_client`] surface. It is verb-agnostic: the extension
/// reaches whatever host verb its grant (`requested ∩ admin_approved`) includes; an ungranted call is
/// a typed [`CallError::Denied`], never a panic. Re-exported here so a native extension depends only on
/// `lb-ext-native` for both directions of the wire.
pub use lb_sidecar_client::{self, CallError, Config, SidecarClient};
