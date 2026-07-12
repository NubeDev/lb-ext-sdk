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

/// The `params` shape for a [`Method::Call`]: which tool, its opaque-JSON input, and — additively —
/// **who** the host already authorized for this call ([`Caller`]).
///
/// `caller` is **additive-by-absence**: an old host omits it (`skip_serializing_if`), and a child
/// built against an older SDK ignores an unknown field, so the frame stays backward compatible across
/// a host/child version skew — the same rule the wasm SDK's additive fields use, and it needs NO
/// [`crate::PROTOCOL_MAJOR`] bump. A child that reads `caller` can enforce per-caller row visibility
/// (native-caller-identity scope); one that ignores it behaves exactly as before.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CallParams {
    pub tool: String,
    pub input: String,
    /// A minimal, **non-replayable** projection of the principal the host authorized for this call.
    /// `None` on an old-host frame (or a call with no resolvable caller). See [`Caller`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller: Option<Caller>,
}

/// A minimal projection of the caller the host stamps into a [`CallParams`] frame
/// (native-caller-identity scope). It is the *least* a per-caller row filter needs: **who** (`sub`),
/// **which tenant** (`ws`), **role**, and a **delegation marker** (`delegated`).
///
/// **This is NOT a token.** It carries no signature the host gateway would accept for a *new* call,
/// so an extension can never *act as* the caller against a third tool. Use it only to (1) attribute
/// this extension's own row-filter decision to the caller and (2) name `sub` as the `subject` of a
/// reach verb (`authz.check_scoped` / `authz.scope_filter`) this extension is *separately* granted to
/// delegate (`mcp:authz.delegate_reach:call`). The projection alone confers nothing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Caller {
    /// The global identity the host authorized (`user:…` / `key:…` / `agent:…`).
    pub sub: String,
    /// The workspace the call is scoped to — the hard tenant wall.
    pub ws: String,
    /// The caller's role, lower-cased (`super-admin` / `workspace-admin` / `member`).
    pub role: String,
    /// True when the caller is itself a derived (on-behalf-of) principal.
    #[serde(default)]
    pub delegated: bool,
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
            caller: None,
        };
        let back: CallParams = serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn call_params_carries_caller_round_trip() {
        let p = CallParams {
            tool: "child.get".into(),
            input: r#"{"id":"leo"}"#.into(),
            caller: Some(Caller {
                sub: "user:ana".into(),
                ws: "acme".into(),
                role: "member".into(),
                delegated: false,
            }),
        };
        let back: CallParams = serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
        assert_eq!(back, p);
        assert_eq!(back.caller.unwrap().sub, "user:ana");
    }

    /// Backward compatibility: an OLD-host frame has no `caller` key at all. It must still
    /// deserialize (→ `caller: None`), so a child on the new SDK talking to an old host is fine.
    #[test]
    fn old_frame_without_caller_deserializes_to_none() {
        let old = r#"{"tool":"echo","input":"{}"}"#;
        let params: CallParams = serde_json::from_str(old).unwrap();
        assert_eq!(params.tool, "echo");
        assert!(params.caller.is_none());
    }

    /// The absent-caller frame is byte-identical to the pre-`caller` wire (skip_serializing_if), so
    /// an old host reading a new child's echoed shape sees no unexpected field.
    #[test]
    fn absent_caller_is_omitted_on_the_wire() {
        let p = CallParams {
            tool: "echo".into(),
            input: "{}".into(),
            caller: None,
        };
        let json = serde_json::to_string(&p).unwrap();
        assert!(
            !json.contains("caller"),
            "absent caller must not serialize: {json}"
        );
    }
}
