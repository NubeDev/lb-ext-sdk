//! The child-side serve loop — the whole runtime a native extension needs.
//!
//! A native extension's `main` builds its [`Tools`] and calls [`serve`]; from there this crate owns
//! the stdio wire. It reads `Content-Length`-framed [`Request`]s from a reader (stdin in production),
//! answers each on a writer (stdout), and returns when the host sends [`Method::Shutdown`] or closes
//! the stream. The four control methods map exactly to lb's supervisor:
//!
//! - `init`     → reply the [`InitReply`] (protocol major + the tool list from [`Tools::tools`]).
//! - `health`   → reply `ok` immediately (liveness; the extension is single-threaded on this line).
//! - `call`     → parse [`CallParams`], dispatch [`Tools::call`], reply the tool's JSON or its error.
//! - `shutdown` → reply `ok`, then return so the caller can drain and exit.
//!
//! The loop is deliberately sequential: the control line is low-rate request/reply (lb's supervisor
//! writes one request and reads until the matching id — no pipelining), so a single reader keeps the
//! wire correct without a background task to race. An extension that needs concurrency does it inside
//! [`Tools::call`]; the wire stays one-at-a-time.

use tokio::io::{AsyncRead, AsyncWrite};

use crate::frame::{read_frame, write_frame};
use crate::handshake::InitReply;
use crate::wire::{CallParams, Method, Reply, Request};

/// What a native extension implements: the set of tools it serves and how to run one.
///
/// The host addresses a tool by its bare name (the `<ext>.` prefix is stripped host-side before the
/// `call` reaches the child), passes opaque-JSON `input`, and expects opaque-JSON back. An `Err` is
/// surfaced to the host as the reply `error` (→ `SupervisorError::Child`), never a panic.
pub trait Tools: Send {
    /// The tool names this extension serves. Reported in the `init` handshake so the host can reject
    /// an unknown-tool dispatch early. Order is not significant.
    fn tools(&self) -> Vec<String>;

    /// Run `tool` with opaque-JSON `input`, returning opaque-JSON output or a human error string.
    /// `async` via the returned future; a tool that blocks should offload, not stall the wire.
    fn call(
        &mut self,
        tool: &str,
        input: &str,
    ) -> impl std::future::Future<Output = Result<String, String>> + Send;
}

/// Serve the control wire on `reader`/`writer` until shutdown or EOF, dispatching to `tools`.
///
/// Returns `Ok(())` on a clean `shutdown` or when the host closes the stream (EOF is the host going
/// away — a normal stop, not an error). Returns `Err` only on a real I/O failure writing a reply.
pub async fn serve<R, W, T>(mut reader: R, mut writer: W, mut tools: T) -> std::io::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
    T: Tools,
{
    loop {
        let body = match read_frame(&mut reader).await {
            Ok(b) => b,
            // EOF / closed stream: the host is gone. A clean stop, not a failure.
            Err(_) => return Ok(()),
        };
        let req: Request = match serde_json::from_slice(&body) {
            Ok(r) => r,
            Err(e) => {
                // A frame we can't parse has no id to correlate; reply on id 0 and keep serving.
                reply(&mut writer, Reply::err(0, format!("bad request json: {e}"))).await?;
                continue;
            }
        };

        match req.method {
            Method::Init => {
                let init = InitReply::new(tools.tools());
                let json = serde_json::to_string(&init).unwrap_or_else(|_| "{}".into());
                reply(&mut writer, Reply::ok(req.id, json)).await?;
            }
            Method::Health => {
                reply(&mut writer, Reply::ok(req.id, "\"ok\"")).await?;
            }
            Method::Call => {
                let r = dispatch_call(&mut tools, &req.params).await;
                let reply = match r {
                    Ok(out) => Reply::ok(req.id, out),
                    Err(msg) => Reply::err(req.id, msg),
                };
                self::reply(&mut writer, reply).await?;
            }
            Method::Shutdown => {
                reply(&mut writer, Reply::ok(req.id, "\"ok\"")).await?;
                return Ok(());
            }
        }
    }
}

/// Parse a `call`'s [`CallParams`] and dispatch it, mapping a parse failure to a child error string.
async fn dispatch_call<T: Tools>(tools: &mut T, params: &str) -> Result<String, String> {
    let call: CallParams =
        serde_json::from_str(params).map_err(|e| format!("bad call params: {e}"))?;
    tools.call(&call.tool, &call.input).await
}

/// Frame and write one reply.
async fn reply<W: AsyncWrite + Unpin>(writer: &mut W, reply: Reply) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(&reply)?;
    write_frame(writer, &bytes).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::Request;
    use tokio::io::duplex;

    /// A tiny echo extension: one tool `echo` that returns its input unchanged.
    struct Echo;
    impl Tools for Echo {
        fn tools(&self) -> Vec<String> {
            vec!["echo".into()]
        }
        async fn call(&mut self, tool: &str, input: &str) -> Result<String, String> {
            match tool {
                "echo" => Ok(input.to_string()),
                other => Err(format!("unknown tool: {other}")),
            }
        }
    }

    /// Drive `serve` from the host side over an in-memory duplex: write a request, read the reply.
    async fn round_trip(reqs: Vec<Request>) -> Vec<Reply> {
        let (host, child) = duplex(64 * 1024);
        let (child_r, child_w) = tokio::io::split(child);
        let server = tokio::spawn(async move { serve(child_r, child_w, Echo).await });

        let (mut host_r, mut host_w) = tokio::io::split(host);
        let mut replies = Vec::new();
        for req in reqs {
            let bytes = serde_json::to_vec(&req).unwrap();
            write_frame(&mut host_w, &bytes).await.unwrap();
            let body = read_frame(&mut host_r).await.unwrap();
            replies.push(serde_json::from_slice(&body).unwrap());
        }
        // EOF → server returns. A `duplex` stays open until BOTH split halves of the host side are
        // dropped: dropping only `host_w` leaves `host_r` holding the pipe open, so the child's
        // `read_frame` never sees EOF and `server.await` hangs forever (only masked when a prior
        // `shutdown` already ended the loop, or the process exits before the await is reached). Drop
        // both halves so the write side genuinely closes and the child observes EOF.
        drop(host_w);
        drop(host_r);
        server.await.unwrap().unwrap();
        replies
    }

    #[tokio::test]
    async fn init_reports_protocol_major_and_tools() {
        let replies = round_trip(vec![Request {
            id: 0,
            method: Method::Init,
            params: String::new(),
        }])
        .await;
        let init: InitReply = serde_json::from_str(replies[0].result.as_ref().unwrap()).unwrap();
        assert_eq!(init.protocol_major, crate::PROTOCOL_MAJOR);
        assert_eq!(init.tools, vec!["echo".to_string()]);
    }

    #[tokio::test]
    async fn health_replies_ok() {
        let replies = round_trip(vec![Request {
            id: 3,
            method: Method::Health,
            params: String::new(),
        }])
        .await;
        assert_eq!(replies[0].id, 3);
        assert_eq!(replies[0].result.as_deref(), Some("\"ok\""));
    }

    #[tokio::test]
    async fn call_dispatches_to_the_tool() {
        let params = serde_json::to_string(&CallParams {
            tool: "echo".into(),
            input: r#"{"hello":true}"#.into(),
        })
        .unwrap();
        let replies = round_trip(vec![Request {
            id: 1,
            method: Method::Call,
            params,
        }])
        .await;
        assert_eq!(replies[0].result.as_deref(), Some(r#"{"hello":true}"#));
        assert!(replies[0].error.is_none());
    }

    #[tokio::test]
    async fn unknown_tool_is_a_child_error() {
        let params = serde_json::to_string(&CallParams {
            tool: "nope".into(),
            input: "{}".into(),
        })
        .unwrap();
        let replies = round_trip(vec![Request {
            id: 2,
            method: Method::Call,
            params,
        }])
        .await;
        assert!(replies[0].result.is_none());
        assert!(replies[0]
            .error
            .as_deref()
            .unwrap()
            .contains("unknown tool"));
    }

    #[tokio::test]
    async fn shutdown_replies_then_ends_the_loop() {
        // If shutdown didn't end the loop, `server.await` in round_trip would hang past the drop.
        let replies = round_trip(vec![Request {
            id: 9,
            method: Method::Shutdown,
            params: String::new(),
        }])
        .await;
        assert_eq!(replies[0].result.as_deref(), Some("\"ok\""));
    }

    #[tokio::test]
    async fn full_lifecycle_init_call_shutdown() {
        let call = serde_json::to_string(&CallParams {
            tool: "echo".into(),
            input: "42".into(),
        })
        .unwrap();
        let replies = round_trip(vec![
            Request {
                id: 0,
                method: Method::Init,
                params: String::new(),
            },
            Request {
                id: 1,
                method: Method::Call,
                params: call,
            },
            Request {
                id: 2,
                method: Method::Shutdown,
                params: String::new(),
            },
        ])
        .await;
        assert_eq!(replies.len(), 3);
        assert_eq!(replies[1].result.as_deref(), Some("42"));
    }
}
