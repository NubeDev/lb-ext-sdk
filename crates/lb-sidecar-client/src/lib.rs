//! `lb-sidecar-client` — the generic native-sidecar → host MCP callback transport
//! (native-callback-transport scope). A native Tier-2 sidecar (spawned by the host supervisor,
//! carrying `LB_EXT_WS`/`LB_EXT_ID`/`LB_EXT_TOKEN`/`LB_GATEWAY_URL` in its env) uses [`SidecarClient`]
//! to CALL host MCP tools over an authenticated `POST /mcp/call` — the out-of-process peer of the wasm
//! guest's in-process `host.call-tool` bridge. Both are transports for the one MCP contract (rule 7);
//! each is denied identically by the host's capability + workspace gate (a `403` → [`CallError::Denied`]).
//!
//! **Verb- and product-agnostic.** Nothing here special-cases a tool or an extension. A sidecar reaches
//! whatever verb its grant includes — `ingest.write`, `outbox.enqueue`, `authz.check_scoped`, any
//! `<ext>.<tool>` — through this one crate; it only knows how to authenticate as the child and speak
//! the `/mcp/call` shape. This crate is re-exported by [`lb-ext-native`](https://docs.rs/lb-ext-native)
//! so a native extension carries a single platform dependency.
//!
//! ```no_run
//! # async fn demo() -> Result<(), lb_sidecar_client::CallError> {
//! use lb_sidecar_client::SidecarClient;
//! use serde_json::json;
//! let host = SidecarClient::from_env()?; // reads the supervisor-injected identity
//! let out = host
//!     .call_tool("ingest.write", json!({ "series": "ros.temp", "payload": 21 }))
//!     .await?;
//! # let _ = out;
//! # Ok(())
//! # }
//! ```

mod client;
mod config;
mod error;

pub use client::SidecarClient;
pub use config::{Config, GATEWAY_ENV, ID_ENV, TOKEN_ENV, WS_ENV};
pub use error::CallError;
