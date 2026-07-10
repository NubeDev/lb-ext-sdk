//! The `init` handshake and its protocol-version gate.
//!
//! This is the native-tier analogue of `lb_sdk::WORLD_MAJOR`. WASM guests are refused at load on a
//! world-major mismatch; before this crate existed the native `init` handshake carried no version at
//! all, so once a native extension pins a *published* `lb-ext-native`, host/child ABI drift became
//! possible and silent. The handshake now carries an explicit protocol major: the host reads it at
//! `init` and refuses a child whose major differs, exactly as loudly as a world mismatch.

use serde::{Deserialize, Serialize};

/// Major version of the native sidecar wire protocol. Bumping this breaks every native extension
/// built against an older `lb-ext-native` — a deliberate, rare act, mirroring `lb_sdk::WORLD_MAJOR`.
/// The child announces it in [`Init`]; the host compares against its own and refuses a mismatch.
pub const PROTOCOL_MAJOR: u64 = 0;

/// The child's opening message on spawn: who it is and which wire major it speaks. The host verifies
/// `protocol_major == its own` before dispatching a single `call`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Init {
    /// The extension id (must match the installed manifest's id).
    pub ext_id: String,
    /// The wire protocol major this child was built against — [`PROTOCOL_MAJOR`].
    pub protocol_major: u64,
}

impl Init {
    /// Build the child's `init` for `ext_id`, stamping the compiled-in [`PROTOCOL_MAJOR`].
    pub fn new(ext_id: impl Into<String>) -> Self {
        Self {
            ext_id: ext_id.into(),
            protocol_major: PROTOCOL_MAJOR,
        }
    }

    /// Host-side check: is this child's protocol major compatible with `host_major`?
    /// Compatibility is major-equality (semver) — the same rule `lb_sdk::world_major_matches` uses.
    pub fn compatible_with(&self, host_major: u64) -> bool {
        self.protocol_major == host_major
    }
}

/// The host's reply to a compatible [`Init`]: the tools the child is granted, so it can reject a
/// dispatch for a tool outside its grant without a round-trip.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitResult {
    pub granted_tools: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_stamps_current_major() {
        assert_eq!(Init::new("cooler-panel").protocol_major, PROTOCOL_MAJOR);
    }

    #[test]
    fn compatible_only_on_equal_major() {
        let init = Init::new("cooler-panel");
        assert!(init.compatible_with(PROTOCOL_MAJOR));
        assert!(!init.compatible_with(PROTOCOL_MAJOR + 1));
    }

    #[test]
    fn init_result_round_trips_json() {
        let res = InitResult {
            granted_tools: vec!["series.read".into(), "ingest.write".into()],
        };
        let back: InitResult = serde_json::from_str(&serde_json::to_string(&res).unwrap()).unwrap();
        assert_eq!(back.granted_tools.len(), 2);
    }

    #[test]
    fn init_round_trips_json() {
        let init = Init::new("cooler-panel");
        let json = serde_json::to_string(&init).unwrap();
        let back: Init = serde_json::from_str(&json).unwrap();
        assert_eq!(back.ext_id, "cooler-panel");
        assert_eq!(back.protocol_major, PROTOCOL_MAJOR);
    }
}
