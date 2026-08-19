//! Generate a tool's `input_schema` from the very struct the tool parses (feature `schemars`).
//!
//! A declared schema is only worth trusting if it cannot drift from the parser. An extension already
//! has one authoritative description of a tool's input — the `serde` args struct `call` deserializes
//! into — so the SDK's answer to "how do I write the schema?" is *don't*: derive
//! [`schemars::JsonSchema`] alongside `Deserialize` on that same struct and generate it.
//!
//! ```ignore
//! #[derive(serde::Deserialize, schemars::JsonSchema)]
//! #[serde(rename_all = "camelCase")]
//! struct WriteArgs { point: String, value: f64, read_back: Option<bool> }
//!
//! ToolDescriptor::new("point.write")
//!     .group("points")
//!     .input_schema(lb_ext_native::schema_for::<WriteArgs>())
//!     .emits_external(true)
//! ```
//!
//! Hand-authored `serde_json::json!` schemas remain equally valid — this helper is sugar, not a
//! requirement, and the feature is off by default so an extension that doesn't want the dependency
//! pays nothing.
//!
//! ## Why draft-07, and why `$schema` is stripped
//!
//! The consumers of these schemas are form builders running `ajv` in a browser. `ajv`'s default
//! instance speaks draft-07; handed a document declaring `$schema: ".../draft/2020-12/schema"` it
//! refuses to compile rather than degrading. So this helper generates draft-07 and removes the
//! `$schema` key entirely, leaving the consumer's own default dialect to apply. The alternative —
//! emitting 2020-12 and making every consumer instantiate the matching validator — pushes a
//! coordination burden onto every downstream UI for no gain at the shapes tool args actually take.

use serde_json::Value;

/// Generate a JSON Schema for `T` suitable for [`ToolDescriptor::input_schema`].
///
/// [`ToolDescriptor::input_schema`]: crate::ToolDescriptor::input_schema
pub fn schema_for<T: schemars::JsonSchema>() -> Value {
    let schema = schemars::generate::SchemaSettings::draft07()
        .into_generator()
        .into_root_schema_for::<T>();
    let mut value =
        serde_json::to_value(schema).unwrap_or_else(|_| Value::Object(Default::default()));
    if let Value::Object(map) = &mut value {
        // See the module doc: the dialect declaration is what breaks a default `ajv`, not the shape.
        map.remove("$schema");
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(schemars::JsonSchema)]
    #[serde(rename_all = "camelCase")]
    #[allow(dead_code)]
    struct WriteArgs {
        point: String,
        value: f64,
        read_back: Option<bool>,
    }

    #[test]
    fn generates_an_object_schema_with_the_serde_field_names() {
        let schema = schema_for::<WriteArgs>();
        assert_eq!(schema["type"], "object");
        let props = schema["properties"].as_object().unwrap();
        assert!(props.contains_key("point"));
        assert!(props.contains_key("value"));
        // `#[serde(rename_all = "camelCase")]` must reach the schema — the whole point of deriving on
        // the parsing struct is that the schema describes the wire the parser accepts.
        assert!(props.contains_key("readBack"), "got: {props:?}");
    }

    #[test]
    fn required_reflects_non_optional_fields() {
        let schema = schema_for::<WriteArgs>();
        let required: Vec<&str> = schema["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(required.contains(&"point"));
        assert!(required.contains(&"value"));
        assert!(!required.contains(&"readBack"));
    }

    /// A default `ajv` rejects a document whose `$schema` names a dialect it wasn't built for; the
    /// helper strips it so the consumer's default dialect applies.
    #[test]
    fn dialect_declaration_is_stripped() {
        assert!(schema_for::<WriteArgs>().get("$schema").is_none());
    }
}
