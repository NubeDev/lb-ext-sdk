//! The child control-wire shapes — the exact request/reply the host (`lb-supervisor`) and a native
//! extension exchange over the framed line. A small, closed method set: `init` (handshake), `health`
//! (liveness poll), `call` (dispatch a tool), `shutdown` (cooperative drain). JSON over
//! `Content-Length` framing (see [`crate::frame`]).
//!
//! These types are the **child mirror** of lb's `lb-supervisor::rpc` — they must stay byte-for-byte
//! serde-compatible with the host side, because they *are* the wire. The host sends [`Request`]; the
//! child replies [`Reply`]. Method-specific arguments ride `params` as an opaque JSON string (the
//! same opaque-JSON ABI the wasm tier uses — richer schemas stay host-side), so this control surface
//! stays tiny and stable while individual tool schemas evolve.

use serde::{Deserialize, Serialize};

/// A request from the host to the child. `id` correlates the reply; `method` is the verb.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Request {
    pub id: u64,
    pub method: Method,
    /// Method-specific arguments as a raw JSON string. Empty for `init`/`health`/`shutdown`.
    #[serde(default)]
    pub params: String,
}

/// The closed set of control methods. A new method is a deliberate protocol change (bump
/// [`crate::PROTOCOL_MAJOR`]), never an ad-hoc string.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Method {
    /// Handshake: the host asks the child to confirm it is ready. Sent once, right after spawn.
    Init,
    /// Liveness poll: the child must reply within the host's health window or be treated as dead.
    Health,
    /// Dispatch a tool: `params` carries a [`CallParams`] JSON.
    Call,
    /// Cooperative shutdown: the child should drain and exit; the host escalates to a kill after grace.
    Shutdown,
}

/// A reply from the child, correlated by `id`. Exactly one of `result`/`error` is set.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Reply {
    pub id: u64,
    /// The success payload (a raw JSON string), present when the call succeeded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    /// The error message, present when the call failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Reply {
    pub fn ok(id: u64, result: impl Into<String>) -> Self {
        Self {
            id,
            result: Some(result.into()),
            error: None,
        }
    }
    pub fn err(id: u64, error: impl Into<String>) -> Self {
        Self {
            id,
            result: None,
            error: Some(error.into()),
        }
    }
}

/// The `params` shape for a [`Method::Call`]: which tool and its opaque-JSON input.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CallParams {
    pub tool: String,
    pub input: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_round_trips() {
        let req = Request {
            id: 7,
            method: Method::Call,
            params: r#"{"tool":"series.read","input":"{}"}"#.into(),
        };
        let back: Request = serde_json::from_str(&serde_json::to_string(&req).unwrap()).unwrap();
        assert_eq!(back, req);
    }

    #[test]
    fn method_serializes_lowercase() {
        assert_eq!(serde_json::to_string(&Method::Init).unwrap(), "\"init\"");
        assert_eq!(
            serde_json::to_string(&Method::Shutdown).unwrap(),
            "\"shutdown\""
        );
    }

    #[test]
    fn reply_ok_and_err_are_exclusive() {
        let ok = Reply::ok(1, "[]");
        assert!(ok.result.is_some() && ok.error.is_none());
        let err = Reply::err(1, "boom");
        assert!(err.result.is_none() && err.error.as_deref() == Some("boom"));
    }

    #[test]
    fn call_params_round_trip() {
        let p = CallParams {
            tool: "ingest.write".into(),
            input: r#"{"n":1}"#.into(),
        };
        let back: CallParams = serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
        assert_eq!(back, p);
    }
}
