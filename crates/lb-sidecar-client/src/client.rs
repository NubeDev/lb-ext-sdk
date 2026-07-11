//! `SidecarClient` — the native-sidecar → host MCP callback transport (native-callback-transport
//! scope). A native Tier-2 sidecar (spawned by the host supervisor with its scoped identity in env)
//! uses this to CALL host MCP tools — `ingest.write`, `outbox.enqueue`, `series.find`,
//! `authz.check_scoped`, any tool its grant includes — the same way an edge user does: an
//! authenticated `POST /mcp/call` to the gateway. It is the out-of-process peer of the wasm guest's
//! in-process `host.call-tool` bridge; both are transports for the ONE MCP contract (rule 7), each
//! denied identically by the host's capability + workspace gate.
//!
//! Nothing here is tool- or extension-specific: it is the generic mechanism every native sidecar reaches
//! the host through. The child authenticates with the node-signed `LB_EXT_TOKEN` (verifiable by the
//! gateway since the minter and verifier share the node key); it can reach only its
//! `requested ∩ admin_approved` grant — a capability denial surfaces as [`CallError::Denied`], never a panic.

use serde_json::Value;

use crate::config::Config;
use crate::error::CallError;

/// A reusable client for host MCP callbacks. Holds one pooled `reqwest::Client` (connection reuse
/// across a poller's many `ingest.write`s) and the resolved [`Config`]. Cheap to clone the config;
/// construct once at sidecar start and share.
#[derive(Clone)]
pub struct SidecarClient {
    http: reqwest::Client,
    config: Config,
}

impl SidecarClient {
    /// Build a client from the injected env (the normal path). Fails with the precise reason
    /// ([`CallError::NoToken`] / [`CallError::NoGateway`]) if the supervisor identity is absent.
    pub fn from_env() -> Result<Self, CallError> {
        Ok(Self::with_config(Config::from_env()?))
    }

    /// Build a client from an explicit [`Config`] (tests, or a non-env address resolution).
    pub fn with_config(config: Config) -> Self {
        Self {
            http: reqwest::Client::new(),
            config,
        }
    }

    /// The workspace the child is scoped to (diagnostics only — the wire workspace is the token's).
    pub fn ws(&self) -> &str {
        &self.config.ws
    }

    /// Call a host MCP `tool` with JSON `input`, returning its JSON output. Authenticated with the
    /// child's scoped token; authorized at the host (`mcp:<tool>:call`, workspace-first). A `403` is
    /// mapped to [`CallError::Denied`] (the capability/workspace refusal — distinct from any other
    /// failure); other statuses to [`CallError::Http`]; a connect/timeout to [`CallError::Transport`].
    ///
    /// This is the one method a sidecar needs: `client.call_tool("ingest.write", json!({…})).await`.
    pub async fn call_tool(&self, tool: &str, input: Value) -> Result<Value, CallError> {
        // The `/mcp/call` bridge body: `{tool, args}`. No token, no workspace — the token travels in
        // the Authorization header and the host derives the workspace from it (never the body, §7).
        let body = serde_json::json!({ "tool": tool, "args": input });
        let resp = self
            .http
            .post(format!("{}/mcp/call", self.config.gateway_url))
            .bearer_auth(&self.config.token)
            .json(&body)
            .send()
            .await
            .map_err(|e| CallError::Transport(e.to_string()))?;

        let status = resp.status();
        if status.is_success() {
            return resp
                .json::<Value>()
                .await
                .map_err(|e| CallError::Decode(e.to_string()));
        }

        // A capability/workspace denial is a `403` — the ONE status a sidecar must distinguish, so it
        // gets its own variant (never conflated with transport/other-http failures). The body is not
        // inspected for a reason (the gate is opaque — no oracle).
        if status.as_u16() == 403 {
            return Err(CallError::Denied);
        }

        let message = resp.text().await.unwrap_or_default();
        Err(CallError::Http {
            status: status.as_u16(),
            message,
        })
    }
}
