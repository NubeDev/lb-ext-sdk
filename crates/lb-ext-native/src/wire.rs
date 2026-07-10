//! The request/response shapes on the stdio wire.
//!
//! JSON in / JSON out, mirroring the WASM tier's `tool.call` exactly (the obvious dual). Richer typed
//! records are deliberately avoided so the boundary stays stable while individual tool schemas evolve
//! — the schemas are validated host-side, never baked into this ABI.

use serde::{Deserialize, Serialize};

/// A tool dispatch from host to child: invoke `name` with `input_json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub name: String,
    pub input_json: String,
}

/// The child's reply: the tool's JSON output, or a [`ToolError`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Response {
    Ok { output_json: String },
    Err { error: ToolError },
}

/// The tool error shape — kept identical to the WASM world's `tool-error` variant so a tool behaves
/// the same whichever tier hosts it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolError {
    /// The input JSON was malformed or failed the tool's contract.
    BadInput(String),
    /// The tool ran but failed.
    Failed(String),
}

impl Response {
    pub fn ok(output_json: impl Into<String>) -> Self {
        Self::Ok {
            output_json: output_json.into(),
        }
    }
    pub fn failed(msg: impl Into<String>) -> Self {
        Self::Err {
            error: ToolError::Failed(msg.into()),
        }
    }
    pub fn bad_input(msg: impl Into<String>) -> Self {
        Self::Err {
            error: ToolError::BadInput(msg.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_round_trips() {
        let req = Request {
            name: "series.read".into(),
            input_json: r#"{"id":1}"#.into(),
        };
        let back: Request = serde_json::from_str(&serde_json::to_string(&req).unwrap()).unwrap();
        assert_eq!(back.name, "series.read");
    }

    #[test]
    fn ok_and_error_responses_tag_distinctly() {
        let ok = serde_json::to_string(&Response::ok("[]")).unwrap();
        assert!(ok.contains("\"ok\""));
        let err = serde_json::to_string(&Response::failed("boom")).unwrap();
        assert!(err.contains("\"err\"") && err.contains("failed"));
    }
}
