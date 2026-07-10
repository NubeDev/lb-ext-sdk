//! The `init` handshake payload and its protocol-version gate.
//!
//! This is the native-tier analogue of `lb_sdk::WORLD_MAJOR`. WASM guests are refused at load on a
//! world-major mismatch; the native `init` handshake carries the same protection so that once a
//! native extension pins a *published* `lb-ext-native`, host/child ABI drift is caught, not silent.
//!
//! On the wire (see [`crate::wire`]) the host sends `Request{method: Init}` and the child replies
//! with an [`InitReply`] JSON as the reply `result`: the protocol major it was built against plus the
//! tools it is prepared to serve. The host compares the major against its own and refuses a mismatch
//! exactly as loudly as a `world` mismatch.

use serde::{Deserialize, Serialize};

/// Major version of the native sidecar wire protocol. Bumping this breaks every native extension
/// built against an older `lb-ext-native` — a deliberate, rare act, mirroring `lb_sdk::WORLD_MAJOR`.
/// The child announces it in [`InitReply`]; the host compares against its own and refuses a mismatch.
pub const PROTOCOL_MAJOR: u64 = 0;

/// The child's reply to the host's `init` request: the wire major it speaks and the tools it serves.
/// Serialized to JSON and returned as the `init` reply's `result` string.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InitReply {
    /// The wire protocol major this child was built against — [`PROTOCOL_MAJOR`].
    pub protocol_major: u64,
    /// The tools this child implements, so the host can reject a dispatch for an unknown tool early.
    #[serde(default)]
    pub tools: Vec<String>,
}

impl InitReply {
    /// Build the child's `init` reply, stamping the compiled-in [`PROTOCOL_MAJOR`].
    pub fn new(tools: impl IntoIterator<Item = String>) -> Self {
        Self {
            protocol_major: PROTOCOL_MAJOR,
            tools: tools.into_iter().collect(),
        }
    }

    /// Host-side check: is this child's protocol major compatible with `host_major`?
    /// Compatibility is major-equality (semver) — the same rule `lb_sdk::world_major_matches` uses.
    pub fn compatible_with(&self, host_major: u64) -> bool {
        self.protocol_major == host_major
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_stamps_current_major() {
        assert_eq!(InitReply::new([]).protocol_major, PROTOCOL_MAJOR);
    }

    #[test]
    fn compatible_only_on_equal_major() {
        let init = InitReply::new([]);
        assert!(init.compatible_with(PROTOCOL_MAJOR));
        assert!(!init.compatible_with(PROTOCOL_MAJOR + 1));
    }

    #[test]
    fn init_reply_round_trips_json() {
        let res = InitReply::new(["series.read".into(), "ingest.write".into()]);
        let back: InitReply = serde_json::from_str(&serde_json::to_string(&res).unwrap()).unwrap();
        assert_eq!(back, res);
        assert_eq!(back.tools.len(), 2);
    }
}
