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
//! **`call` fans out; the three control methods do not** (native-child-concurrency scope). The read
//! loop spawns each `call` onto its own task and goes straight back to reading, so a multi-second
//! verb no longer stalls every verb behind it. `init` / `health` / `shutdown` are answered on the
//! loop itself, in order — `health` in particular must answer within the host's liveness window, and
//! a health reply queued behind a slow call is read by the host as a **dead child**.
//!
//! This is safe because the host is already multiplexed. `lb-supervisor`'s `Conn` registers a waiter,
//! holds its write lock for exactly one frame, and routes every reply back **by `id`** through a
//! pending map — it pipelines by construction and never assumes reply order. (Verified against
//! `lb/rust/crates/supervisor/src/conn.rs` before this change; if the host had assumed ordering, the
//! fix would have had to change shape.) Out-of-order replies on this wire are therefore correct, not
//! merely tolerated.
//!
//! Three invariants hold the concurrency together; each is a corruption or availability bug if
//! broken:
//!
//! 1. **Exactly one writer.** Replies are funnelled through an mpsc channel to a single writer task,
//!    so two handlers finishing at once cannot interleave the bytes of their frames. A frame is
//!    written whole or not at all. (A `Mutex` over the write half would also serialise, but the
//!    channel additionally decouples a slow host read from the handler that produced the reply.)
//! 2. **Bounded concurrency.** A [`Semaphore`] caps calls in flight at [`MAX_INFLIGHT_CALLS`], so a
//!    dashboard issuing twenty tiles at once cannot open twenty database pools. The permit is taken
//!    **inside** the spawned task, so acquiring it never blocks the read loop — an extension at its
//!    cap still answers `health` instantly and still accepts `shutdown`.
//! 3. **`shutdown` drains, it does not cancel.** On `shutdown` the loop stops reading, waits for
//!    every in-flight call to finish and its reply to be written, and only then returns. Cancelling
//!    instead would drop work the host is still waiting on, which it would surface as a transport
//!    error rather than a result.
//!
//! [`Tools`] is therefore shared by reference: `call` takes `&self`, and the trait requires `Sync`.
//! An extension holding real mutable state uses interior mutability, which is the honest place for
//! it — the wire should not be the thing serialising an extension's database queries.

use std::sync::Arc;

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, Semaphore};

use crate::descriptor::ToolDescriptor;
use crate::frame::{read_frame, write_frame};
use crate::handshake::InitReply;
use crate::wire::{CallParams, Caller, Method, Reply, Request};

/// The host addresses a tool by its bare name (the `<ext>.` prefix is stripped host-side before the
/// `call` reaches the child), passes opaque-JSON `input`, and expects opaque-JSON back. An `Err` is
/// surfaced to the host as the reply `error` (→ `SupervisorError::Child`), never a panic.
/// The most calls this crate will run at once against one extension.
///
/// **Why 8.** The bound exists so a page cannot convert its tile count into database load. The
/// number is taken from the thing actually being protected: a customer connection's pool is built
/// with `max_connections: 5` (`ros-core`'s `registry`), and a real page fans out to a handful of
/// verbs across at most two or three connections. 8 keeps a normal dashboard entirely unqueued
/// while capping a pathological caller well below the point where concurrent verbs would start
/// contending for pool slots and turn a fast read back into a slow one.
///
/// It is a ceiling on *concurrency*, never on correctness: a call over the cap waits for a permit
/// and is then served normally. Nothing is rejected, and `health` is never behind it.
pub const MAX_INFLIGHT_CALLS: usize = 8;

/// What a native extension implements. See the method docs; note `call` takes `&self`.
pub trait Tools: Send + Sync + 'static {
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
    /// **Takes `&self`.** The serve loop runs several calls against one `Tools` at once, so a tool
    /// that genuinely needs mutable state holds it behind interior mutability (a `Mutex`, an
    /// atomic, a connection pool) rather than making the wire the thing that serialises it. Almost
    /// every extension already only reads shared handles here, so this costs it nothing.
    fn call(
        &self,
        tool: &str,
        input: &str,
    ) -> impl std::future::Future<Output = Result<String, String>> + Send;

    /// The self-declared contract for each tool — title, group, input JSON Schema, external-effect
    /// flag — reported alongside [`tools`](Tools::tools) in the `init` handshake so the host's
    /// `tools.catalog` can serve typed schemas instead of bare names.
    ///
    /// **Default:** one [`ToolDescriptor::name_only`] per entry in [`tools`](Tools::tools) — exactly
    /// what the host synthesised on its own before the handshake could carry more, so an existing
    /// extension recompiles against this SDK with no source change and no behaviour change.
    ///
    /// [`tools`](Tools::tools) remains the dispatch allowlist; overriding this only enriches. Derive
    /// the schemas from the args structs the tool already parses via [`crate::schema_for`] (feature
    /// `schemars`) so a declaration cannot drift from the parser.
    fn descriptors(&self) -> Vec<ToolDescriptor> {
        self.tools()
            .into_iter()
            .map(ToolDescriptor::name_only)
            .collect()
    }

    /// Run `tool` with `input`, given the authorized [`Caller`] the host stamped into the frame
    /// (`None` on an old-host frame). Override this to enforce per-caller row visibility — attribute
    /// the row filter to `caller.sub`, or name it as the `subject` of a delegated reach verb this
    /// extension is granted to call (native-caller-identity scope).
    ///
    /// **Default:** ignore the caller and forward to [`call`](Tools::call). So the caller field is
    /// purely opt-in: an identity-unaware extension needs no change, and a new SDK does not force a
    /// behavioural change on an existing one. `Send` bound on the future matches `call`.
    fn call_with_caller(
        &self,
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
/// `call` is spawned; `init` / `health` / `shutdown` are answered inline and stay ordered. Returns
/// `Ok(())` on a clean `shutdown` (after draining in-flight calls) or when the host closes the
/// stream (EOF is the host going away — a normal stop, not an error). Returns `Err` only on a real
/// I/O failure writing a reply.
pub async fn serve<R, W, T>(mut reader: R, writer: W, tools: T) -> std::io::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin + Send + 'static,
    T: Tools,
{
    let tools = Arc::new(tools);
    let limit = Arc::new(Semaphore::new(MAX_INFLIGHT_CALLS));

    // Invariant 1: ONE writer. Every reply — spawned or inline — goes through this channel, so two
    // handlers finishing at once can never interleave the bytes of their frames.
    let (tx, mut rx) = mpsc::unbounded_channel::<Reply>();
    let writer_task = tokio::spawn(async move {
        let mut writer = writer;
        while let Some(reply) = rx.recv().await {
            let bytes = match serde_json::to_vec(&reply) {
                Ok(b) => b,
                // A reply we cannot even serialize has no useful frame; the id is still owed an
                // answer, so send the failure rather than dropping the caller into a hang.
                Err(e) => serde_json::to_vec(&Reply::err(reply.id, format!("bad reply json: {e}")))
                    .unwrap_or_default(),
            };
            if bytes.is_empty() {
                continue;
            }
            if let Err(e) = write_frame(&mut writer, &bytes).await {
                return Err(e);
            }
        }
        Ok(())
    });

    // Handles for in-flight calls, so `shutdown` can DRAIN rather than cancel (invariant 3).
    let mut inflight: Vec<tokio::task::JoinHandle<()>> = Vec::new();

    loop {
        // Reap finished handles so a long-lived extension does not accumulate them.
        inflight.retain(|h| !h.is_finished());

        let body = match read_frame(&mut reader).await {
            Ok(b) => b,
            // EOF / closed stream: the host is gone. A clean stop, not a failure.
            Err(_) => break,
        };
        let req: Request = match serde_json::from_slice(&body) {
            Ok(r) => r,
            Err(e) => {
                // A frame we can't parse has no id to correlate; reply on id 0 and keep serving.
                let _ = tx.send(Reply::err(0, format!("bad request json: {e}")));
                continue;
            }
        };

        match req.method {
            Method::Init => {
                // An extension that declares nothing yields the default `descriptors()` — one
                // name-only entry per tool, which carries no more than `tools` already does. Omit it
                // in that case so its frame is byte-identical to a pre-descriptor SDK's, and so
                // "descriptors absent" keeps meaning "nothing declared" on the host side.
                let descriptors = tools.descriptors();
                let init = if descriptors.iter().all(ToolDescriptor::is_name_only) {
                    InitReply::new(tools.tools())
                } else {
                    InitReply::with_descriptors(tools.tools(), descriptors)
                };
                let json = serde_json::to_string(&init).unwrap_or_else(|_| "{}".into());
                let _ = tx.send(Reply::ok(req.id, json));
            }
            Method::Health => {
                // Answered on the loop, so it is never queued behind a call. This is the whole
                // reason only `call` fans out.
                let _ = tx.send(Reply::ok(req.id, "\"ok\""));
            }
            Method::Call => {
                // Spawn and go straight back to reading — the line does not wait for the handler.
                let tools = Arc::clone(&tools);
                let limit = Arc::clone(&limit);
                let tx = tx.clone();
                let id = req.id;
                let params = req.params;
                inflight.push(tokio::spawn(async move {
                    // Invariant 2: the permit is acquired HERE, inside the task, never on the read
                    // loop. At the cap, calls queue among themselves while the wire stays live.
                    //
                    // `acquire_owned` fails only if the semaphore is closed; it never is (the `Arc`
                    // outlives every task), so treat that impossible case as "proceed unbounded"
                    // rather than dropping a caller's reply and hanging it.
                    let _permit = Semaphore::acquire_owned(limit).await.ok();
                    let reply = match dispatch_call(&*tools, &params).await {
                        Ok(out) => Reply::ok(id, out),
                        Err(msg) => Reply::err(id, msg),
                    };
                    let _ = tx.send(reply);
                }));
            }
            Method::Shutdown => {
                // Drain, don't cancel. Every call already accepted is answered before we go, so a
                // cooperative stop never silently discards work the host is waiting on. The
                // shutdown reply is queued LAST so it cannot overtake a drained call's reply.
                for handle in inflight.drain(..) {
                    let _ = handle.await;
                }
                let _ = tx.send(Reply::ok(req.id, "\"ok\""));
                break;
            }
        }
    }

    // EOF path: the host is gone, but a call it already accepted may still be running. Drain it for
    // the same reason as shutdown — the handler may have side effects worth completing, and the
    // reply is cheap to write into a closed pipe (the writer task surfaces that as its I/O error).
    for handle in inflight.drain(..) {
        let _ = handle.await;
    }

    // Dropping the last sender ends the writer task; awaiting it both flushes every queued reply
    // and surfaces a genuine write failure, which is the only `Err` this function returns.
    drop(tx);
    match writer_task.await {
        Ok(result) => result,
        // The writer task itself panicked or was aborted — report it rather than claiming success.
        Err(e) => Err(std::io::Error::other(format!("writer task failed: {e}"))),
    }
}

/// Parse a `call`'s [`CallParams`] and dispatch it through [`Tools::call_with_caller`], mapping a
/// parse failure to a child error string. `caller` is `None` on an old-host frame; the default of
/// `call_with_caller` forwards to `call`, so a caller-unaware extension is unaffected.
async fn dispatch_call<T: Tools>(tools: &T, params: &str) -> Result<String, String> {
    let call: CallParams =
        serde_json::from_str(params).map_err(|e| format!("bad call params: {e}"))?;
    tools
        .call_with_caller(&call.tool, &call.input, call.caller)
        .await
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
        async fn call(&self, tool: &str, input: &str) -> Result<String, String> {
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
            async fn call(&self, _tool: &str, _input: &str) -> Result<String, String> {
                Ok("\"anon\"".into())
            }
            async fn call_with_caller(
                &self,
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
