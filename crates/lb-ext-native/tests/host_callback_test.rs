//! The native host-callback client, exercised through the `lb-ext-native` facade over REAL HTTP.
//!
//! This is the SDK-side proof of the reverse call: a NATIVE (Tier-2) extension can call a host MCP
//! verb back over the child wire — `SidecarClient::from_env().call_tool("authz.check_scoped", args)` —
//! and get the JSON result. The client is re-exported from `lb-ext-native`, so a native extension
//! carries a single platform dependency for BOTH directions of the wire (host→child dispatch, and this
//! child→host callback). `authz.check_scoped` is used only as a representative granted verb; the client
//! is verb-agnostic and special-cases nothing.
//!
//! No mocks (CLAUDE §9 / testing §0): the code under test is the real `SidecarClient` making real
//! `reqwest` calls; the peer is a REAL `tokio` TCP server speaking the gateway's `/mcp/call` shape
//! (`{tool, args}` + `Authorization: Bearer`), not a hand-written fake of the client. The end-to-end
//! proof against a real `lb` gateway + real capability gate lives in lb's
//! `role/gateway/tests/native_callback_test.rs` (which consumes THIS published crate) — here we prove
//! the exported client's wire contract standalone, since the SDK repo does not depend on lb.
//!
//! Categories:
//!   - round-trip: a `call_tool` to an arbitrary granted verb (`authz.check_scoped`) sends the right
//!     wire shape + bearer token and decodes the JSON result;
//!   - deny: a real `403` from the endpoint maps to the distinct `CallError::Denied` (never conflated
//!     with a transport/other-HTTP failure), and no body is treated as a result.

use std::sync::Arc;

use lb_ext_native::{CallError, Config, SidecarClient};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// What the fake gateway captured from one request — enough to assert the client's wire contract.
#[derive(Debug, Clone, Default)]
struct Captured {
    path: String,
    authorization: Option<String>,
    body: Value,
}

/// A REAL one-request HTTP/1.1 server that plays the gateway's `/mcp/call` endpoint. It reads the
/// request, records what the client sent, and answers `status` with `resp_body`. Not a mock of the
/// client — a genuine HTTP peer the real `reqwest` client talks to. Returns `(base_url, captured)`.
async fn serve_once(
    status: u16,
    resp_body: &'static str,
) -> (String, Arc<tokio::sync::Mutex<Captured>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().unwrap();
    let captured = Arc::new(tokio::sync::Mutex::new(Captured::default()));
    let cap = captured.clone();
    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.expect("accept");
        // Read until we have headers + the full body (Content-Length framed, as reqwest sends JSON).
        let mut buf = Vec::new();
        let mut tmp = [0u8; 4096];
        loop {
            let n = sock.read(&mut tmp).await.expect("read");
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
            if let Some(done) = request_complete(&buf) {
                if done {
                    break;
                }
            }
        }
        let text = String::from_utf8_lossy(&buf);
        let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
        let mut lines = head.lines();
        let request_line = lines.next().unwrap_or_default();
        let path = request_line
            .split_whitespace()
            .nth(1)
            .unwrap_or("")
            .to_string();
        let authorization = lines
            .find(|l| l.to_ascii_lowercase().starts_with("authorization:"))
            .map(|l| l.splitn(2, ':').nth(1).unwrap_or("").trim().to_string());
        let parsed: Value = serde_json::from_str(body.trim()).unwrap_or(Value::Null);
        {
            let mut c = cap.lock().await;
            *c = Captured {
                path,
                authorization,
                body: parsed,
            };
        }
        let reason = if status == 200 { "OK" } else { "ERR" };
        let resp = format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{resp_body}",
            resp_body.len()
        );
        sock.write_all(resp.as_bytes()).await.expect("write resp");
        sock.flush().await.ok();
    });
    (format!("http://{addr}"), captured)
}

/// Have we read a full request (headers + Content-Length bytes)? `None` = headers not complete yet.
fn request_complete(buf: &[u8]) -> Option<bool> {
    let text = String::from_utf8_lossy(buf);
    let (head, body) = text.split_once("\r\n\r\n")?;
    let len: usize = head
        .lines()
        .find(|l| l.to_ascii_lowercase().starts_with("content-length:"))
        .and_then(|l| l.split(':').nth(1))
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0);
    Some(body.len() >= len)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn call_tool_sends_the_gateway_wire_shape_and_decodes_the_result() {
    // The endpoint answers a granted call with a real MCP result body.
    let (base, captured) = serve_once(200, r#"{"allow":true}"#).await;
    let client = SidecarClient::with_config(Config::new(&base, "child-jwt-abc", "acme", "care"));

    // A representative granted verb — the client is verb-agnostic; nothing special-cases it.
    let out = client
        .call_tool(
            "authz.check_scoped",
            json!({ "cap": "mcp:widget.list:call", "table": "widget", "id": "widget:1" }),
        )
        .await
        .expect("granted callback round-trips");
    assert_eq!(
        out,
        json!({ "allow": true }),
        "the JSON result is decoded verbatim"
    );

    // Assert the exact wire contract a native extension relies on: `POST /mcp/call`, bearer, `{tool,args}`.
    let c = captured.lock().await.clone();
    assert_eq!(c.path, "/mcp/call", "posts to the gateway MCP chokepoint");
    assert_eq!(
        c.authorization.as_deref(),
        Some("Bearer child-jwt-abc"),
        "authenticates with the injected child token, in the header (never the body)"
    );
    assert_eq!(
        c.body["tool"], "authz.check_scoped",
        "the verb rides `tool`"
    );
    assert_eq!(
        c.body["args"]["id"], "widget:1",
        "the JSON args ride `args`, untouched"
    );
    // The workspace is NEVER in the body — the host derives it from the token (the hard wall, §7).
    assert!(
        c.body.get("ws").is_none(),
        "workspace is not client-supplied"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn a_real_403_maps_to_the_distinct_denied_variant() {
    // The endpoint refuses at the capability/workspace gate — a real `403`, with no result body.
    let (base, _captured) = serve_once(403, "forbidden").await;
    let client = SidecarClient::with_config(Config::new(&base, "ungranted-jwt", "acme", "care"));

    let err = client
        .call_tool(
            "authz.check_scoped",
            json!({ "cap": "mcp:widget.list:call" }),
        )
        .await
        .expect_err("an ungranted callback must be refused");
    // The one status a sidecar must distinguish gets its own variant — not `Http`, not `Transport`.
    assert!(
        matches!(err, CallError::Denied),
        "a 403 is the distinct capability-deny outcome, got {err:?}"
    );
}
