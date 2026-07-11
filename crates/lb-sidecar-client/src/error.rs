//! The typed callback error a native sidecar sees when it calls back into the host
//! (native-callback-transport scope). A capability denial is a **first-class, distinct** variant —
//! never a panic, never conflated with a transport failure — so a sidecar can react to "the host
//! refused me" differently from "the host was unreachable". Every variant is `Display`-able for a
//! log line; none carries the token or the workspace (no secret material in an error string).

use thiserror::Error;

/// Why a `call_tool` callback failed. The load-bearing distinction is [`Denied`](CallError::Denied)
/// (the host's capability/workspace gate refused this call — a `403` from `POST /mcp/call`) versus
/// everything else (the call never got a clean gate decision). A sidecar treats `Denied` as "I am
/// not granted this" (do not retry blindly); `Transport`/`Http` as "the host/network is a problem".
#[derive(Debug, Error)]
pub enum CallError {
    /// The host **refused** the call at its capability/workspace gate (HTTP `403`). Opaque by design
    /// (the gate does not say whether the tool exists or the cap is missing — the no-oracle contract);
    /// the sidecar only learns "you may not do this". This is NOT retryable by widening — the child's
    /// grant is `requested ∩ admin_approved` and cannot grow at runtime.
    #[error("host denied the call (capability/workspace gate)")]
    Denied,

    /// The child has no callback address — `LB_GATEWAY_URL` was not injected (no gateway fronts this
    /// node, or the host is a pure control-line node). The sidecar cannot call host tools at all.
    #[error("no callback address: LB_GATEWAY_URL is not set")]
    NoGateway,

    /// The child has no `LB_EXT_TOKEN` in its env — it was not spawned by the supervisor (or the env
    /// was stripped). Without the token the host cannot authenticate the callback.
    #[error("no callback credential: LB_EXT_TOKEN is not set")]
    NoToken,

    /// The host answered with a non-`403` error status (e.g. `401` bad token, `400` bad input, `5xx`).
    /// Carries the status and the host's message for a log line — never retried as if `Denied`.
    #[error("host returned HTTP {status}: {message}")]
    Http { status: u16, message: String },

    /// The request never reached a clean HTTP response — a connect/timeout/TLS/DNS failure. The
    /// host may be down or unreachable; this is the retryable-with-backoff class (the sidecar decides).
    #[error("transport error reaching the host: {0}")]
    Transport(String),

    /// The host's `2xx` body was not the JSON the caller expected (should not happen against a real
    /// gateway; surfaced rather than panicked so a protocol drift is visible, not a crash).
    #[error("could not decode the host response: {0}")]
    Decode(String),
}
