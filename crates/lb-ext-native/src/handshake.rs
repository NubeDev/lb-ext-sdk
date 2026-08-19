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

use crate::descriptor::ToolDescriptor;

/// Major version of the native sidecar wire protocol. Bumping this breaks every native extension
/// built against an older `lb-ext-native` — a deliberate, rare act, mirroring `lb_sdk::WORLD_MAJOR`.
/// The child announces it in [`InitReply`]; the host compares against its own and refuses a mismatch.
pub const PROTOCOL_MAJOR: u64 = 0;

/// The child's reply to the host's `init` request: the wire major it speaks and the tools it serves.
/// Serialized to JSON and returned as the `init` reply's `result` string.
///
/// `tools` and [`descriptors`](Self::descriptors) describe the same set from two angles. `tools` is
/// and remains the **dispatch allowlist** — the host rejects an unknown-tool call against it.
/// `descriptors` is optional enrichment joined onto that list by name; a descriptor naming a tool the
/// child did not declare is dropped host-side with a warning, never a boot failure. Replacing `tools`
/// with `descriptors` outright was rejected: it forces a [`PROTOCOL_MAJOR`] bump and a flag-day for
/// every published extension, for no functional gain.
///
/// `PartialEq` but not `Eq` — descriptors carry `serde_json::Value` schemas.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InitReply {
    /// The wire protocol major this child was built against — [`PROTOCOL_MAJOR`].
    pub protocol_major: u64,
    /// The tools this child implements, so the host can reject a dispatch for an unknown tool early.
    #[serde(default)]
    pub tools: Vec<String>,
    /// Per-tool self-declared contracts (title, group, input schema, external-effect flag).
    ///
    /// **Additive and optional**: absent on every child built before this field existed, and omitted
    /// entirely when empty, so a name-only child's `init` frame is byte-identical to what it sent
    /// before. A host that finds it absent falls back to [`ToolDescriptor::name_only`] over `tools`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub descriptors: Vec<ToolDescriptor>,
}

impl InitReply {
    /// Build the child's `init` reply, stamping the compiled-in [`PROTOCOL_MAJOR`].
    ///
    /// Declares no descriptors — see [`with_descriptors`](Self::with_descriptors).
    pub fn new(tools: impl IntoIterator<Item = String>) -> Self {
        Self {
            protocol_major: PROTOCOL_MAJOR,
            tools: tools.into_iter().collect(),
            descriptors: Vec::new(),
        }
    }

    /// Build the child's `init` reply carrying both the dispatch allowlist and the per-tool
    /// descriptors. This is what [`serve`](crate::serve) sends, joining [`Tools::tools`] and
    /// [`Tools::descriptors`].
    ///
    /// [`Tools::tools`]: crate::Tools::tools
    /// [`Tools::descriptors`]: crate::Tools::descriptors
    pub fn with_descriptors(
        tools: impl IntoIterator<Item = String>,
        descriptors: impl IntoIterator<Item = ToolDescriptor>,
    ) -> Self {
        Self {
            descriptors: descriptors.into_iter().collect(),
            ..Self::new(tools)
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

    /// The old-host/old-child cell of the compatibility matrix: an `init` frame written before
    /// `descriptors` existed must still parse, landing on an empty vec (not an error, not a `None`
    /// the host has to special-case).
    #[test]
    fn old_frame_without_descriptors_parses_as_empty() {
        let old = r#"{"protocol_major":0,"tools":["echo"]}"#;
        let init: InitReply = serde_json::from_str(old).unwrap();
        assert_eq!(init.tools, vec!["echo".to_string()]);
        assert!(init.descriptors.is_empty());
    }

    /// A child that declares nothing must put the *same bytes* on the wire as before this field was
    /// added — that is what makes "bit-identical old behaviour" a property rather than a hope.
    #[test]
    fn empty_descriptors_are_omitted_from_the_wire() {
        let json = serde_json::to_string(&InitReply::new(["echo".into()])).unwrap();
        assert_eq!(json, r#"{"protocol_major":0,"tools":["echo"]}"#);
    }

    #[test]
    fn descriptors_round_trip_with_their_schemas() {
        let res = InitReply::with_descriptors(
            ["point.write".into()],
            [ToolDescriptor::new("point.write")
                .group("points")
                .input_schema(serde_json::json!({"type": "object"}))
                .emits_external(true)],
        );
        let back: InitReply = serde_json::from_str(&serde_json::to_string(&res).unwrap()).unwrap();
        assert_eq!(back, res);
        assert_eq!(back.descriptors[0].group, "points");
        assert!(back.descriptors[0].emits_external);
    }

    /// An unknown field is ignored by `serde` — the new-ext/old-host cell. Proven here so the claim
    /// is checked rather than assumed about a host we cannot import.
    #[test]
    fn unknown_future_fields_are_ignored() {
        let future = r#"{"protocol_major":0,"tools":["echo"],"descriptors":[],"whatever":42}"#;
        let init: InitReply = serde_json::from_str(future).unwrap();
        assert_eq!(init.tools, vec!["echo".to_string()]);
    }
}
