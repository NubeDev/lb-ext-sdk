//! What an extension declares about ONE tool: the self-described contract the host folds into its
//! registry and serves from `tools.catalog`.
//!
//! The host's registry has always modelled this shape — `ToolDescriptor { name, title, group,
//! input_schema, emits_external, result }` — but the `init` handshake could only carry bare names
//! ([`crate::InitReply::tools`]), so every extension tool reached a UI schema-less and every schema
//! consumer (command palette, forms builder, dashboard write-action builder) degraded to a free-text
//! argument box. This type is the child half of closing that gap: an extension returns descriptors
//! from [`Tools::descriptors`](crate::Tools::descriptors) and the host registers them verbatim.
//!
//! Two properties are load-bearing:
//!
//! - **Absence is valid.** Every field but `name` is optional on the wire. A descriptor carrying only
//!   a name is exactly what the host synthesised before this existed, so an extension that declares
//!   nothing keeps its current behaviour bit-for-bit ([`ToolDescriptor::name_only`]).
//! - **Self-declared, not enforced.** The host does not validate a call's `input` against
//!   `input_schema` before dispatch — the extension's own `serde` parse stays the authority, the same
//!   trust model as `emits_external`. A tool that lies about its schema breaks only its own form UX.
//!   Catalog schemas are a UI affordance, never a security boundary.
//!
//! Names here are **bare** (`point.write`, not `modbus.point.write`); the host qualifies with the
//! extension id when it serves the catalog, exactly as it does for [`crate::InitReply::tools`].

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One tool's self-declared contract, as reported in the `init` handshake.
///
/// Build with [`name_only`](Self::name_only) for a bare name, or [`new`](Self::new) plus the
/// consuming builder methods for a full declaration:
///
/// ```
/// use lb_ext_native::ToolDescriptor;
///
/// let d = ToolDescriptor::new("point.write")
///     .title("Write point")
///     .group("points")
///     .input_schema(serde_json::json!({
///         "type": "object",
///         "properties": { "point": { "type": "string" }, "value": { "type": "number" } },
///         "required": ["point", "value"],
///     }))
///     .emits_external(true);
/// assert_eq!(d.name, "point.write");
/// ```
///
/// `PartialEq` but deliberately **not** `Eq`: `input_schema`/`result` hold `serde_json::Value`,
/// whose float variant has no total equality.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ToolDescriptor {
    /// The bare tool name, matching an entry in [`crate::InitReply::tools`]. A descriptor whose name
    /// is not in that list is dropped host-side with a warning — `tools` stays the dispatch allowlist.
    pub name: String,
    /// Human label for a picker row. Empty means "fall back to the name".
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    /// Grouping key for a picker's section headers (e.g. `points`, `devices`). Empty means ungrouped.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub group: String,
    /// JSON Schema for the call's `input`, or `None` for "unknown shape, render free text".
    /// Derive it from the same struct the tool parses via [`crate::schema_for`] so it cannot drift.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<Value>,
    /// True when running this tool transmits an effect off the node (a field-bus write, an outbound
    /// message). Drives the undo classifier's irreversible class; the host trusts the declaration.
    #[serde(default, skip_serializing_if = "core::ops::Not::not")]
    pub emits_external: bool,
    /// JSON Schema for the tool's output, or `None`. Advisory only — nothing validates against it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
}

impl ToolDescriptor {
    /// A descriptor carrying nothing but the name — what the host synthesised for every extension
    /// tool before the handshake could carry more. The fallback for an old child, and the honest
    /// declaration for a tool whose input really is opaque.
    pub fn name_only(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Self::default()
        }
    }

    /// True when this descriptor declares nothing the host could not have synthesised from the bare
    /// name alone. The `init` handshake uses this to omit an all-name-only descriptor list entirely,
    /// so a non-declaring extension's frame stays byte-identical to what it sent before descriptors
    /// existed — absence keeps meaning "nothing declared" rather than becoming noise on every frame.
    pub fn is_name_only(&self) -> bool {
        *self == Self::name_only(&self.name)
    }

    /// Start a full declaration. Chain the builder methods below to fill it in.
    pub fn new(name: impl Into<String>) -> Self {
        Self::name_only(name)
    }

    /// Set the human label shown in a picker row.
    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }

    /// Set the grouping key a picker sections rows by.
    pub fn group(mut self, group: impl Into<String>) -> Self {
        self.group = group.into();
        self
    }

    /// Set the input JSON Schema. Pair with [`crate::schema_for`] to generate it from the tool's own
    /// `serde` args struct rather than hand-writing a second source of truth.
    pub fn input_schema(mut self, schema: Value) -> Self {
        self.input_schema = Some(schema);
        self
    }

    /// Declare that this tool's effect leaves the node (see [`Self::emits_external`]).
    pub fn emits_external(mut self, emits: bool) -> Self {
        self.emits_external = emits;
        self
    }

    /// Set the advisory output JSON Schema.
    pub fn result(mut self, schema: Value) -> Self {
        self.result = Some(schema);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_only_declares_nothing_else() {
        let d = ToolDescriptor::name_only("echo");
        assert_eq!(d.name, "echo");
        assert!(d.title.is_empty());
        assert!(d.group.is_empty());
        assert!(d.input_schema.is_none());
        assert!(!d.emits_external);
        assert!(d.result.is_none());
    }

    /// Absence is the encoding for "not declared": a name-only descriptor must not put empty strings,
    /// nulls, or `false` on the wire, so an old host reading it sees the same minimal object it would
    /// have synthesised itself.
    #[test]
    fn name_only_serializes_to_just_the_name() {
        let json = serde_json::to_string(&ToolDescriptor::name_only("echo")).unwrap();
        assert_eq!(json, r#"{"name":"echo"}"#);
    }

    #[test]
    fn builder_round_trips_through_json() {
        let d = ToolDescriptor::new("point.write")
            .title("Write point")
            .group("points")
            .input_schema(serde_json::json!({"type": "object"}))
            .emits_external(true)
            .result(serde_json::json!({"type": "boolean"}));
        let back: ToolDescriptor =
            serde_json::from_str(&serde_json::to_string(&d).unwrap()).unwrap();
        assert_eq!(back, d);
    }

    /// Every field but `name` is optional inbound, so a descriptor written by an older or simpler
    /// declarer still parses.
    #[test]
    fn missing_fields_parse_as_absent() {
        let d: ToolDescriptor = serde_json::from_str(r#"{"name":"echo"}"#).unwrap();
        assert_eq!(d, ToolDescriptor::name_only("echo"));
    }
}
