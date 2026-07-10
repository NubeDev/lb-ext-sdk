//! The native (Tier-2) extension SDK — the child side of the host↔sidecar wire.
//!
//! A native extension is a subprocess the host supervises over stdio (newline-delimited JSON-RPC).
//! This crate is the whole contract a native extension needs: the wire message shapes, the
//! protocol-version handshake, and (below) the host-callback client. It does not — and must not —
//! expose the host-side supervisor; that stays in the platform (`lb`), so this facade can be
//! published without freezing host internals as public API.
//!
//! See lb's `docs/scope/extensions/ext-out-of-tree-scope.md` (the native tier) and
//! `docs/scope/extensions/native-callback-transport-scope.md`.

pub mod handshake;
pub mod wire;

pub use handshake::{Init, InitResult, PROTOCOL_MAJOR};
pub use wire::{Request, Response, ToolError};
