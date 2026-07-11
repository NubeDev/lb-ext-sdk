//! The child's callback configuration — read once from the env the supervisor injected
//! (native-callback-transport scope). A native sidecar is spawned with `LB_EXT_WS` / `LB_EXT_ID` /
//! `LB_EXT_TOKEN` / `LB_GATEWAY_URL` (native/spec.rs); this reads them into one value the client
//! carries. Kept separate from the client (FILE-LAYOUT: reading env is one responsibility, making
//! HTTP calls another) and constructible from explicit params so a test can build a config without
//! touching process env (env is process-global and races across parallel tests).

use crate::error::CallError;

/// The env var names the supervisor injects. Public so a test (or a sidecar's own diagnostics) can
/// reference the exact keys rather than string-duplicating them.
pub const WS_ENV: &str = "LB_EXT_WS";
pub const ID_ENV: &str = "LB_EXT_ID";
pub const TOKEN_ENV: &str = "LB_EXT_TOKEN";
pub const GATEWAY_ENV: &str = "LB_GATEWAY_URL";

/// The resolved callback context: where to POST and what to authenticate with. The workspace is NOT
/// sent on the wire (the host derives it from the token, never the body — the hard wall, §7); it is
/// kept only for the child's own diagnostics/log lines. The token is a bearer credential — treat it
/// as secret (never logged).
#[derive(Clone)]
pub struct Config {
    /// The gateway base URL to POST `/mcp/call` to, e.g. `http://127.0.0.1:8080`. No trailing slash.
    pub gateway_url: String,
    /// The scoped `LB_EXT_TOKEN` (a node-signed JWT) sent as `Authorization: Bearer`.
    pub token: String,
    /// The child's own extension id (`LB_EXT_ID`) — diagnostics only.
    pub ext_id: String,
    /// The child's workspace (`LB_EXT_WS`) — diagnostics only; the wire workspace comes from the token.
    pub ws: String,
}

impl Config {
    /// Read the callback config from the process environment (the normal path — the supervisor
    /// injected these). A missing token is [`CallError::NoToken`]; a missing gateway URL is
    /// [`CallError::NoGateway`] — both distinct so a sidecar can log precisely why it cannot call back.
    pub fn from_env() -> Result<Self, CallError> {
        let token = non_empty_env(TOKEN_ENV).ok_or(CallError::NoToken)?;
        let gateway_url = non_empty_env(GATEWAY_ENV).ok_or(CallError::NoGateway)?;
        Ok(Self {
            gateway_url: gateway_url.trim_end_matches('/').to_string(),
            token,
            ext_id: non_empty_env(ID_ENV).unwrap_or_default(),
            ws: non_empty_env(WS_ENV).unwrap_or_default(),
        })
    }

    /// Build a config from explicit values (tests, or a sidecar that resolves its address another
    /// way). No env access — so a test can construct one without racing the process-global env.
    pub fn new(
        gateway_url: impl Into<String>,
        token: impl Into<String>,
        ws: impl Into<String>,
        ext_id: impl Into<String>,
    ) -> Self {
        Self {
            gateway_url: gateway_url.into().trim_end_matches('/').to_string(),
            token: token.into(),
            ws: ws.into(),
            ext_id: ext_id.into(),
        }
    }
}

/// Read an env var, treating an empty value as absent (an injected-but-blank var is not a credential).
fn non_empty_env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}
