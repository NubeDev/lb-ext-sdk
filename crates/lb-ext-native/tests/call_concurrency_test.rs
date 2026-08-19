//! The wire must not serialise calls (native-child-concurrency scope).
//!
//! The host (`lb-supervisor`'s `Conn`) is fully multiplexed: it registers a waiter, writes ONE
//! frame, releases the write lock, and routes every reply back **by `id`**. It pipelines by
//! construction and tolerates any reply order. So the only thing that can still serialise a native
//! extension is this crate's own `serve` loop — and it did: read → `await` the handler → reply, one
//! frame at a time.
//!
//! The cost was measured on a live node (ext-esr): `esr.site.list` reads the extension's OWN
//! SurrealDB table, touches no customer database, and `pg_stat_activity` was idle throughout.
//!
//! | `esr.site.list`                       | Time     |
//! |---------------------------------------|----------|
//! | on an idle sidecar                    | 0.02 s   |
//! | while one `esr.site.overview` is live | **7.55 s** |
//! | immediately after that call returns   | 0.02 s   |
//!
//! A 20 ms local read taking 7.55 s against an idle database is a queue, not a slow query.
//!
//! These tests are written to FAIL against the sequential loop. Each states what it was
//! revert-checked against, because a concurrency test that passes either way proves nothing.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use lb_ext_native::frame::{read_frame, write_frame};
use lb_ext_native::wire::{CallParams, Method, Reply, Request};
use lb_ext_native::{serve, Tools};
use tokio::io::duplex;

/// How long the deliberately slow verb takes. Large enough that a serialised loop cannot possibly
/// deliver the fast reply first, small enough to keep the suite quick.
const SLOW: Duration = Duration::from_millis(600);

/// A two-verb extension standing in for the real shape: one slow verb (`site.overview` — a customer
/// database query) and one cheap local read (`site.list` — this extension's own store).
///
/// `call` takes `&self`: the trait is shared-by-reference so `serve` can dispatch N calls against
/// one `Tools` at once. `peak` records the highest number of handlers ever in flight together,
/// which is the property under test — a reply-order assertion alone can be satisfied by luck.
struct SlowAndFast {
    inflight: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
}

impl SlowAndFast {
    fn new() -> (Self, Arc<AtomicUsize>) {
        let peak = Arc::new(AtomicUsize::new(0));
        (
            Self {
                inflight: Arc::new(AtomicUsize::new(0)),
                peak: Arc::clone(&peak),
            },
            peak,
        )
    }
}

impl Tools for SlowAndFast {
    fn tools(&self) -> Vec<String> {
        vec!["site.overview".into(), "site.list".into()]
    }

    async fn call(&self, tool: &str, input: &str) -> Result<String, String> {
        let now = self.inflight.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(now, Ordering::SeqCst);

        let out = match tool {
            // The slow customer-database verb.
            "site.overview" => {
                tokio::time::sleep(SLOW).await;
                Ok(format!(r#"{{"slow":{input}}}"#))
            }
            // The cheap LOCAL store read. It must not wait behind the verb above.
            "site.list" => Ok(format!(r#"{{"fast":{input}}}"#)),
            other => Err(format!("unknown tool: {other}")),
        };

        self.inflight.fetch_sub(1, Ordering::SeqCst);
        out
    }
}

/// Frame one `call` request for `tool`.
fn call_frame(id: u64, tool: &str, input: &str) -> Vec<u8> {
    let params = serde_json::to_string(&CallParams {
        tool: tool.into(),
        input: input.into(),
        caller: None,
    })
    .unwrap();
    serde_json::to_vec(&Request {
        id,
        method: Method::Call,
        params,
    })
    .unwrap()
}

/// **The regression test.** Two calls back to back, the slow one FIRST. The fast reply must arrive
/// first, and each reply must carry its own `id`.
///
/// Revert-checked: against the sequential `serve` loop this fails on the first assertion — reply 1
/// is the slow call's, because the loop awaits the handler before reading the next frame.
#[tokio::test]
async fn a_slow_call_does_not_block_a_fast_one_behind_it() {
    let (host, child) = duplex(64 * 1024);
    let (child_r, child_w) = tokio::io::split(child);
    let (tools, peak) = SlowAndFast::new();
    let server = tokio::spawn(async move { serve(child_r, child_w, tools).await });
    let (mut host_r, mut host_w) = tokio::io::split(host);

    // Both frames go out before either reply is read — exactly what the multiplexed host does.
    write_frame(&mut host_w, &call_frame(1, "site.overview", "{}"))
        .await
        .unwrap();
    write_frame(&mut host_w, &call_frame(2, "site.list", "{}"))
        .await
        .unwrap();

    let first: Reply = serde_json::from_slice(&read_frame(&mut host_r).await.unwrap()).unwrap();
    let second: Reply = serde_json::from_slice(&read_frame(&mut host_r).await.unwrap()).unwrap();

    assert_eq!(
        first.id, 2,
        "the CHEAP call must answer first; it arrived after the slow one, so the wire is still \
         serialised — this is the head-of-line blocking that made a 0.02 s local read take 7.55 s"
    );
    assert_eq!(second.id, 1, "the slow call answers second");

    // Replies are correlated by id, so each must carry its OWN payload — an out-of-order wire is
    // only safe if the bodies did not get swapped with the ids.
    assert!(
        first.result.as_deref().unwrap().contains("fast"),
        "id 2 must carry the fast verb's body: {first:?}"
    );
    assert!(
        second.result.as_deref().unwrap().contains("slow"),
        "id 1 must carry the slow verb's body: {second:?}"
    );

    // Order alone could be luck. Overlap is the actual property.
    assert!(
        peak.load(Ordering::SeqCst) >= 2,
        "handlers never overlapped (peak in-flight = {}) — the loop is still one-at-a-time",
        peak.load(Ordering::SeqCst)
    );

    drop(host_w);
    drop(host_r);
    server.await.unwrap().unwrap();
}

/// Many cheap calls queued behind one slow call must ALL answer before it — the dashboard case,
/// where one multi-second verb made every tile on the page look broken.
///
/// Revert-checked: sequential `serve` answers id 0 first and fails immediately.
#[tokio::test]
async fn one_slow_call_does_not_stall_a_page_of_cheap_ones() {
    let (host, child) = duplex(64 * 1024);
    let (child_r, child_w) = tokio::io::split(child);
    let (tools, peak) = SlowAndFast::new();
    let server = tokio::spawn(async move { serve(child_r, child_w, tools).await });
    let (mut host_r, mut host_w) = tokio::io::split(host);

    const CHEAP: u64 = 12;

    write_frame(&mut host_w, &call_frame(0, "site.overview", "{}"))
        .await
        .unwrap();
    for id in 1..=CHEAP {
        write_frame(&mut host_w, &call_frame(id, "site.list", "{}"))
            .await
            .unwrap();
    }

    let mut order = Vec::new();
    for _ in 0..=CHEAP {
        let r: Reply = serde_json::from_slice(&read_frame(&mut host_r).await.unwrap()).unwrap();
        assert!(r.error.is_none(), "no call should fail: {r:?}");
        order.push(r.id);
    }

    assert_eq!(
        order.last().copied(),
        Some(0),
        "the slow call must answer LAST; got {order:?}"
    );
    let mut cheap = order[..CHEAP as usize].to_vec();
    cheap.sort_unstable();
    assert_eq!(
        cheap,
        (1..=CHEAP).collect::<Vec<_>>(),
        "every cheap call must be answered exactly once, each with its own id: {order:?}"
    );
    assert!(
        peak.load(Ordering::SeqCst) > 1,
        "handlers never overlapped — still serialised"
    );

    drop(host_w);
    drop(host_r);
    server.await.unwrap().unwrap();
}

/// `shutdown` must DRAIN, not cancel. A call already in flight has to be answered before `serve`
/// returns, or a cooperative stop silently drops work the host is still waiting on — which the host
/// would surface as a transport error rather than a result.
///
/// Revert-checked: passes trivially on the sequential loop (nothing is ever in flight across a
/// frame read), so it is a guard on the NEW behaviour rather than a demonstration of the bug.
#[tokio::test]
async fn shutdown_drains_calls_already_in_flight() {
    let (host, child) = duplex(64 * 1024);
    let (child_r, child_w) = tokio::io::split(child);
    let (tools, _peak) = SlowAndFast::new();
    let server = tokio::spawn(async move { serve(child_r, child_w, tools).await });
    let (mut host_r, mut host_w) = tokio::io::split(host);

    write_frame(&mut host_w, &call_frame(1, "site.overview", "{}"))
        .await
        .unwrap();
    // Shut down while that call is still running.
    tokio::time::sleep(Duration::from_millis(50)).await;
    write_frame(
        &mut host_w,
        &serde_json::to_vec(&Request {
            id: 2,
            method: Method::Shutdown,
            params: String::new(),
        })
        .unwrap(),
    )
    .await
    .unwrap();

    let mut seen = Vec::new();
    for _ in 0..2 {
        let r: Reply = serde_json::from_slice(&read_frame(&mut host_r).await.unwrap()).unwrap();
        seen.push((r.id, r.result.clone()));
    }

    let call = seen.iter().find(|(id, _)| *id == 1).expect(
        "the in-flight call was DROPPED by shutdown — a cooperative stop must drain, not cancel",
    );
    assert!(
        call.1.as_deref().unwrap().contains("slow"),
        "the drained call must carry its real result: {call:?}"
    );
    assert!(
        seen.iter().any(|(id, _)| *id == 2),
        "shutdown must reply ok"
    );

    drop(host_w);
    drop(host_r);
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("serve must return after a drained shutdown")
        .unwrap()
        .unwrap();
}

/// `init` and `health` must stay ordered relative to the loop even while a call is in flight —
/// health is the liveness poll, so a stalled reply is read by the host as a dead child. This is the
/// direct benefit of fanning out ONLY `call`.
#[tokio::test]
async fn health_answers_while_a_call_is_in_flight() {
    let (host, child) = duplex(64 * 1024);
    let (child_r, child_w) = tokio::io::split(child);
    let (tools, _peak) = SlowAndFast::new();
    let server = tokio::spawn(async move { serve(child_r, child_w, tools).await });
    let (mut host_r, mut host_w) = tokio::io::split(host);

    write_frame(&mut host_w, &call_frame(1, "site.overview", "{}"))
        .await
        .unwrap();
    write_frame(
        &mut host_w,
        &serde_json::to_vec(&Request {
            id: 2,
            method: Method::Health,
            params: String::new(),
        })
        .unwrap(),
    )
    .await
    .unwrap();

    let first: Reply = serde_json::from_slice(&read_frame(&mut host_r).await.unwrap()).unwrap();
    assert_eq!(
        first.id, 2,
        "health must answer immediately, not behind a multi-second call — a late health reply is \
         read by the host as a dead child and gets the sidecar restarted"
    );
    assert_eq!(first.result.as_deref(), Some("\"ok\""));

    let second: Reply = serde_json::from_slice(&read_frame(&mut host_r).await.unwrap()).unwrap();
    assert_eq!(second.id, 1);

    drop(host_w);
    drop(host_r);
    server.await.unwrap().unwrap();
}

/// The concurrency bound is real: with more calls in flight than [`MAX_INFLIGHT_CALLS`], the peak
/// overlap never exceeds the cap — and every call is still answered, because the bound queues work
/// rather than rejecting it.
///
/// This is what stops a page from converting its tile count into database load: the cap is set from
/// `ros-core`'s per-connection pool (`max_connections: 5`), so concurrent verbs cannot outnumber the
/// pool badly enough to turn a fast read back into a slow one.
///
/// Revert-checked: with the semaphore removed, `peak` reaches the full 20 and the first assertion
/// fails.
#[tokio::test]
async fn concurrency_is_bounded_and_nothing_is_rejected() {
    use lb_ext_native::serve::MAX_INFLIGHT_CALLS;

    let (host, child) = duplex(256 * 1024);
    let (child_r, child_w) = tokio::io::split(child);
    let (tools, peak) = SlowAndFast::new();
    let server = tokio::spawn(async move { serve(child_r, child_w, tools).await });
    let (mut host_r, mut host_w) = tokio::io::split(host);

    // Every call is the SLOW verb, so they genuinely pile up against the cap.
    const BURST: u64 = 20;
    assert!(
        BURST as usize > MAX_INFLIGHT_CALLS,
        "burst must exceed the cap"
    );
    for id in 0..BURST {
        write_frame(&mut host_w, &call_frame(id, "site.overview", "{}"))
            .await
            .unwrap();
    }

    let mut ids = Vec::new();
    for _ in 0..BURST {
        let r: Reply = serde_json::from_slice(&read_frame(&mut host_r).await.unwrap()).unwrap();
        assert!(
            r.error.is_none(),
            "the bound must queue, never reject: {r:?}"
        );
        ids.push(r.id);
    }

    assert!(
        peak.load(Ordering::SeqCst) <= MAX_INFLIGHT_CALLS,
        "concurrency is UNBOUNDED: {} handlers ran at once against a cap of {MAX_INFLIGHT_CALLS}",
        peak.load(Ordering::SeqCst)
    );
    assert!(
        peak.load(Ordering::SeqCst) > 1,
        "the cap must not have serialised the wire"
    );

    ids.sort_unstable();
    assert_eq!(
        ids,
        (0..BURST).collect::<Vec<_>>(),
        "every call answered exactly once, each with its own id"
    );

    drop(host_w);
    drop(host_r);
    server.await.unwrap().unwrap();
}
