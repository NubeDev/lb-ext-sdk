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
use crate::wire::{CallParams, Caller, Method, Reply, Request};

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
    ///
    /// This is the caller-agnostic entry point. An extension that must enforce per-caller row
    /// visibility overrides [`call_with_caller`](Tools::call_with_caller) instead — the default of
    /// that method forwards here, so an extension that does NOT care about identity only implements
    /// `call` and is unaffected by the additive `caller` frame field (native-caller-identity scope).
    fn call(
        &mut self,
        tool: &str,
        input: &str,
    ) -> impl std::future::Future<Output = Result<String, String>> + Send;

    /// Run `tool` with `input`, given the authorized [`Caller`] the host stamped into the frame
    /// (`None` on an old-host frame). Override this to enforce per-caller row visibility — attribute
    /// the row filter to `caller.sub`, or name it as the `subject` of a delegated reach verb this
    /// extension is granted to call (native-caller-identity scope).
    ///
    /// **Default:** ignore the caller and forward to [`call`](Tools::call). So the caller field is
    /// purely opt-in: an identity-unaware extension needs no change, and a new SDK does not force a
    /// behavioural change on an existing one. `Send` bound on the future matches `call`.
    fn call_with_caller(
        &mut self,
        tool: &str,
        input: &str,
        caller: Option<Caller>,
    ) -> impl std::future::Future<Output = Result<String, String>> + Send {
        // `caller` intentionally unused in the default — an identity-unaware extension gets exactly
        // the old behaviour. Named `_caller` bindings would break the override signature match, so we
        // consume it explicitly to keep the parameter documented and warning-free.
        let _ = &caller;
        self.call(tool, input)
    }
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

/// Parse a `call`'s [`CallParams`] and dispatch it through [`Tools::call_with_caller`], mapping a
/// parse failure to a child error string. `caller` is `None` on an old-host frame; the default of
/// `call_with_caller` forwards to `call`, so a caller-unaware extension is unaffected.
async fn dispatch_call<T: Tools>(tools: &mut T, params: &str) -> Result<String, String> {
    let call: CallParams =
        serde_json::from_str(params).map_err(|e| format!("bad call params: {e}"))?;
    tools
        .call_with_caller(&call.tool, &call.input, call.caller)
        .await
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
            caller: None,
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

    /// An extension that overrides `call_with_caller` receives the frame caller and can act on it —
    /// here it echoes the caller's `sub` (or `"anon"` when the frame carried none). Proves the caller
    /// projection survives serialize → `serve` dispatch → the verb layer, in-process.
    #[tokio::test]
    async fn call_with_caller_delivers_the_frame_caller() {
        struct WhoAmI;
        impl Tools for WhoAmI {
            fn tools(&self) -> Vec<String> {
                vec!["whoami".into()]
            }
            async fn call(&mut self, _tool: &str, _input: &str) -> Result<String, String> {
                Ok("\"anon\"".into())
            }
            async fn call_with_caller(
                &mut self,
                _tool: &str,
                _input: &str,
                caller: Option<Caller>,
            ) -> Result<String, String> {
                Ok(format!(
                    "{:?}",
                    caller.map(|c| c.sub).unwrap_or_else(|| "anon".into())
                ))
            }
        }

        let (host, child) = duplex(64 * 1024);
        let (child_r, child_w) = tokio::io::split(child);
        let server = tokio::spawn(async move { serve(child_r, child_w, WhoAmI).await });
        let (mut host_r, mut host_w) = tokio::io::split(host);

        let params = serde_json::to_string(&CallParams {
            tool: "whoami".into(),
            input: "{}".into(),
            caller: Some(Caller {
                sub: "user:ana".into(),
                ws: "acme".into(),
                role: "member".into(),
                delegated: false,
                admin: false,
            }),
        })
        .unwrap();
        let bytes = serde_json::to_vec(&Request {
            id: 1,
            method: Method::Call,
            params,
        })
        .unwrap();
        write_frame(&mut host_w, &bytes).await.unwrap();
        let body = read_frame(&mut host_r).await.unwrap();
        let reply: Reply = serde_json::from_slice(&body).unwrap();
        assert!(
            reply.result.as_deref().unwrap().contains("user:ana"),
            "verb layer must see the frame caller: {reply:?}"
        );
        drop(host_w);
        drop(host_r);
        server.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn unknown_tool_is_a_child_error() {
        let params = serde_json::to_string(&CallParams {
            tool: "nope".into(),
            input: "{}".into(),
            caller: None,
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
            caller: None,
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
