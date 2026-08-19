//! The descriptor handshake, proven through the real serve loop over an in-memory duplex.
//!
//! The unit tests in `handshake.rs` prove the *shape* round-trips. These prove the *behaviour*: what
//! a declaring child and a non-declaring child actually put on the wire when `serve` answers `init`.
//! Both halves of the compatibility matrix are asserted here, because "an existing extension is
//! unaffected" is the claim this slice rests on and it is only true if the frame says so.

use lb_ext_native::{serve, InitReply, Method, Reply, Request, ToolDescriptor, Tools};
use tokio::io::duplex;

/// An extension that declares nothing beyond its tool names — i.e. every extension written against
/// the SDK before `descriptors()` existed, which inherits the default trait body.
struct Plain;
impl Tools for Plain {
    fn tools(&self) -> Vec<String> {
        vec!["echo".into()]
    }
    async fn call(&self, _tool: &str, input: &str) -> Result<String, String> {
        Ok(input.to_string())
    }
}

/// An extension that declares a full contract for one of its two tools.
struct Declaring;
impl Tools for Declaring {
    fn tools(&self) -> Vec<String> {
        vec!["point.read".into(), "point.write".into()]
    }
    fn descriptors(&self) -> Vec<ToolDescriptor> {
        vec![
            ToolDescriptor::new("point.read")
                .title("Read point")
                .group("points"),
            ToolDescriptor::new("point.write")
                .title("Write point")
                .group("points")
                .input_schema(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "point": { "type": "string", "x-lb": { "widget": "select", "source": "point.list" } },
                        "value": { "type": "number" },
                    },
                    "required": ["point", "value"],
                }))
                .emits_external(true),
        ]
    }
    async fn call(&self, _tool: &str, _input: &str) -> Result<String, String> {
        Ok("null".into())
    }
}

/// Ask a child for its `init` reply over a real duplex-backed `serve` loop, returning the raw
/// `result` string so a test can assert on the bytes as well as the parse.
async fn init_frame<T: Tools + 'static>(tools: T) -> String {
    let (host, child) = duplex(64 * 1024);
    let (child_r, child_w) = tokio::io::split(child);
    let server = tokio::spawn(async move { serve(child_r, child_w, tools).await });
    let (mut host_r, mut host_w) = tokio::io::split(host);

    let bytes = serde_json::to_vec(&Request {
        id: 0,
        method: Method::Init,
        params: String::new(),
    })
    .unwrap();
    lb_ext_native::frame::write_frame(&mut host_w, &bytes)
        .await
        .unwrap();
    let body = lb_ext_native::frame::read_frame(&mut host_r).await.unwrap();
    let reply: Reply = serde_json::from_slice(&body).unwrap();

    // Both host halves must drop for the child to observe EOF, or `server.await` hangs.
    drop(host_w);
    drop(host_r);
    server.await.unwrap().unwrap();
    reply.result.unwrap()
}

/// The old-extension cell of the matrix. A `Tools` impl that never mentions descriptors must emit
/// the exact frame it emitted before this field existed — not a name-only list, not an empty array.
#[tokio::test]
async fn a_non_declaring_extension_emits_the_pre_descriptor_frame() {
    let result = init_frame(Plain).await;
    assert_eq!(result, r#"{"protocol_major":0,"tools":["echo"]}"#);

    let init: InitReply = serde_json::from_str(&result).unwrap();
    assert!(init.descriptors.is_empty());
}

/// The default trait body still answers on its own terms — a host that asks the trait (rather than
/// reading the frame) gets one name-only descriptor per tool, which is what it would have built.
#[test]
fn the_default_descriptors_body_is_name_only_per_tool() {
    assert_eq!(Plain.descriptors(), vec![ToolDescriptor::name_only("echo")],);
}

/// The new-extension cell: schemas, groups, titles and the external-effect flag survive the loop.
#[tokio::test]
async fn a_declaring_extension_ships_its_schemas_over_the_wire() {
    let init: InitReply = serde_json::from_str(&init_frame(Declaring).await).unwrap();

    // `tools` is untouched — it remains the dispatch allowlist regardless of what is declared.
    assert_eq!(init.tools, vec!["point.read", "point.write"]);
    assert_eq!(init.descriptors.len(), 2);

    let write = init
        .descriptors
        .iter()
        .find(|d| d.name == "point.write")
        .expect("point.write descriptor");
    assert_eq!(write.title, "Write point");
    assert_eq!(write.group, "points");
    assert!(write.emits_external);

    let schema = write.input_schema.as_ref().expect("input_schema");
    assert_eq!(schema["properties"]["value"]["type"], "number");
    // The `x-lb` picker hint is opaque passthrough — the SDK must not normalise or drop it.
    assert_eq!(schema["properties"]["point"]["x-lb"]["widget"], "select");

    // A partially-declared sibling keeps its absent fields absent rather than gaining empty ones.
    let read = init
        .descriptors
        .iter()
        .find(|d| d.name == "point.read")
        .expect("point.read descriptor");
    assert!(read.input_schema.is_none());
    assert!(!read.emits_external);
}

/// A mixed list still ships: one enriched descriptor is enough to make the whole list worth sending,
/// so a partially-declared extension does not silently lose the declarations it did write.
#[tokio::test]
async fn one_declared_tool_carries_the_whole_list() {
    let raw = init_frame(Declaring).await;
    assert!(
        raw.contains("\"descriptors\""),
        "declared descriptors must reach the wire: {raw}"
    );
}
