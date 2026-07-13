#![cfg(not(target_arch = "wasm32"))]
//! End-to-end collab over the realtime gateway: boot the axum router with a
//! [`CollabSync`] handler registered, connect real WebSocket clients, and prove
//! the Yjs sync handshake catches a fresh peer up and live edits converge.
//! Host-only (axum / tokio): guarded so the wasm `--all-targets` CI gate stays
//! green without the host-only dev-deps.

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, Stream, StreamExt};
use pocopine_collab::{
    COLLAB_SUBPROTOCOL, CollabDocument, CollabHello, CollabMessage, CollabStore,
    CompatibilityIdentity, MemoryCollabStore, WsGatewayCollabExt,
};
use pocopine_realtime::{Control, Fanout, Frame, FrameKind, LocalFanout, WsGateway, routes};
use tokio::time::{Instant, timeout};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::{Error as TungError, Message as WsMessage};

/// Quiet window after which the handshake/broadcast chatter is considered drained.
const IDLE: Duration = Duration::from_millis(400);
const FINGERPRINT: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn compatibility() -> CompatibilityIdentity {
    CompatibilityIdentity::new(1, FINGERPRINT).unwrap()
}

fn topic(document_key: &str) -> String {
    compatibility().namespace_topic(document_key)
}

/// Boot a single collab-enabled gateway on a fresh in-process fan-out; return
/// its ws:// URL. Wired via the `with_collab` helper (the handler shares the
/// gateway's own fan-out by construction).
async fn spawn() -> String {
    let fanout: Arc<dyn Fanout> = Arc::new(LocalFanout::new());
    serve_gateway(
        WsGateway::new(fanout)
            .allow_all_topics()
            .with_collab(compatibility()),
    )
    .await
}

/// Boot a collab-enabled gateway on a CALLER-PROVIDED fan-out and store, so two
/// gateways can share one fan-out + store to simulate two web replicas behind a
/// single Redis bus. Wired via `with_collab_store`.
async fn spawn_replica(fanout: Arc<dyn Fanout>, store: Arc<dyn CollabStore>) -> String {
    serve_gateway(
        WsGateway::new(fanout)
            .allow_all_topics()
            .with_collab_store(compatibility(), store),
    )
    .await
}

/// Serve `gateway` on a random local port; return its ws:// URL.
async fn serve_gateway(gateway: WsGateway) -> String {
    let app = routes(gateway);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("ws://{addr}/__pocopine/ws/v1")
}

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn next_frame<S>(ws: &mut S) -> Frame
where
    S: Stream<Item = Result<WsMessage, TungError>> + Unpin,
{
    loop {
        match ws
            .next()
            .await
            .expect("stream ended")
            .expect("transport error")
        {
            WsMessage::Binary(data) => return Frame::decode(&data).expect("decode frame"),
            WsMessage::Close(_) => panic!("server closed unexpectedly"),
            _ => {}
        }
    }
}

fn send(frame: Frame) -> WsMessage {
    WsMessage::Binary(frame.encode())
}

/// Read the Hello and subscribe to `topic`, returning the assigned topic_ref.
async fn join(ws: &mut Ws, topic: &str) -> u64 {
    match next_frame(ws).await {
        f if f.kind == FrameKind::Control => {
            assert!(matches!(
                Control::decode(&f.payload).unwrap(),
                Control::Hello { .. }
            ));
        }
        other => panic!("expected Hello, got {other:?}"),
    }
    ws.send(send(Frame::subscribe(COLLAB_SUBPROTOCOL, topic)))
        .await
        .unwrap();
    let frame = next_frame(ws).await;
    match Control::decode(&frame.payload).unwrap() {
        Control::SubscribeAck { topic_ref, .. } => topic_ref,
        other => panic!("expected SubscribeAck, got {other:?}"),
    }
}

/// Send the opening compatibility hello (including this peer's state vector).
async fn open_sync(ws: &mut Ws, topic_ref: u64, doc: &CollabDocument) {
    let msg = CollabMessage::Hello(CollabHello::new(compatibility(), doc.state_vector(), true));
    ws.send(send(Frame::data(
        COLLAB_SUBPROTOCOL,
        topic_ref,
        0,
        msg.encode(),
    )))
    .await
    .unwrap();
}

/// Process every inbound collab Data frame — exactly as a real client would —
/// until the connection is quiet for [`IDLE`]: validate/reply to the server hello
/// with the diff it is missing, and apply SyncStep2/Update messages locally.
/// CRDT idempotency makes the replayed/echoed duplicates harmless.
async fn drive(ws: &mut Ws, topic_ref: u64, doc: &CollabDocument) {
    while let Ok(frame) = timeout(IDLE, next_frame(ws)).await {
        if frame.kind != FrameKind::Data {
            continue;
        }
        match CollabMessage::decode(&frame.payload).unwrap() {
            CollabMessage::Hello(hello) => {
                assert_eq!(hello.compatibility(), &compatibility());
                if hello.requests_sync_step2() {
                    let diff = doc.diff(hello.state_vector()).unwrap();
                    let reply = CollabMessage::SyncStep2(diff.into());
                    ws.send(send(Frame::data(
                        COLLAB_SUBPROTOCOL,
                        topic_ref,
                        0,
                        reply.encode(),
                    )))
                    .await
                    .unwrap();
                }
            }
            CollabMessage::SyncStep2(update) | CollabMessage::Update(update) => {
                doc.apply_update(&update).unwrap();
            }
            // Ephemeral presence — never applied to the document.
            CollabMessage::Awareness(_) => {}
        }
    }
}

/// Assert that no document Data frame crosses the socket during one quiet
/// window. Control errors/heartbeats are allowed and ignored.
async fn assert_no_document_frame(ws: &mut Ws) {
    let deadline = Instant::now() + IDLE;
    loop {
        let now = Instant::now();
        if now >= deadline {
            return;
        }
        match timeout(deadline - now, next_frame(ws)).await {
            Err(_) => return,
            Ok(frame) if frame.kind == FrameKind::Data => {
                panic!("unnegotiated subscription received collab Data: {frame:?}")
            }
            Ok(_) => {}
        }
    }
}

#[tokio::test]
async fn two_clients_converge_over_the_gateway() {
    let url = spawn().await;
    let topic = topic("doc1");

    // Client A joins with a local edit and syncs it up to the server.
    let (mut a, _) = connect_async(&url).await.unwrap();
    let a_ref = join(&mut a, &topic).await;
    let doc_a = CollabDocument::new();
    doc_a.insert_text("body", 0, "alpha ");
    open_sync(&mut a, a_ref, &doc_a).await;
    drive(&mut a, a_ref, &doc_a).await;

    // Client B joins fresh; the handshake catches it up to A's edit.
    let (mut b, _) = connect_async(&url).await.unwrap();
    let b_ref = join(&mut b, &topic).await;
    let doc_b = CollabDocument::new();
    open_sync(&mut b, b_ref, &doc_b).await;
    drive(&mut b, b_ref, &doc_b).await;
    assert!(
        doc_b.text("body").contains("alpha"),
        "B should catch up to A's edit, got {:?}",
        doc_b.text("body")
    );

    // A makes a further edit and broadcasts just the delta; B receives it live.
    let before = doc_a.state_vector();
    let end = doc_a.text("body").chars().count() as u32;
    doc_a.insert_text("body", end, "beta");
    let delta = doc_a.diff(&before).unwrap();
    a.send(send(Frame::data(
        COLLAB_SUBPROTOCOL,
        a_ref,
        0,
        CollabMessage::Update(delta.into()).encode(),
    )))
    .await
    .unwrap();

    drive(&mut b, b_ref, &doc_b).await;
    assert!(
        doc_b.text("body").contains("beta"),
        "B should receive A's live update, got {:?}",
        doc_b.text("body")
    );
}

#[tokio::test]
async fn pre_hello_and_mismatched_subscribers_receive_no_document_frames() {
    let url = spawn().await;
    let topic = topic("outbound-gate");

    // Establish one compatible writer so the room has real live updates.
    let (mut writer, _) = connect_async(&url).await.unwrap();
    let writer_ref = join(&mut writer, &topic).await;
    let writer_doc = CollabDocument::new();
    open_sync(&mut writer, writer_ref, &writer_doc).await;
    drive(&mut writer, writer_ref, &writer_doc).await;

    // This socket subscribes to the *correct* namespaced topic but deliberately
    // withholds hello. Its fan-out pump must remain paused.
    let (mut blocked, _) = connect_async(&url).await.unwrap();
    let blocked_ref = join(&mut blocked, &topic).await;

    let before = writer_doc.state_vector();
    writer_doc.insert_text("body", 0, "before-hello");
    let update = writer_doc.diff(&before).unwrap();
    writer
        .send(send(Frame::data(
            COLLAB_SUBPROTOCOL,
            writer_ref,
            0,
            CollabMessage::Update(update.into()).encode(),
        )))
        .await
        .unwrap();
    assert_no_document_frame(&mut blocked).await;

    // A mismatched hello on that correct topic is rejected and must leave the
    // same pump closed; another real room update still cannot cross it.
    let mismatched = CompatibilityIdentity::new(
        compatibility().protocol_version(),
        "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
    )
    .unwrap();
    let mismatch_hello = CollabMessage::Hello(CollabHello::new(
        mismatched,
        CollabDocument::new().state_vector(),
        true,
    ));
    blocked
        .send(send(Frame::data(
            COLLAB_SUBPROTOCOL,
            blocked_ref,
            0,
            mismatch_hello.encode(),
        )))
        .await
        .unwrap();

    let before = writer_doc.state_vector();
    let end = writer_doc.text("body").chars().count() as u32;
    writer_doc.insert_text("body", end, "-after-mismatch");
    let update = writer_doc.diff(&before).unwrap();
    writer
        .send(send(Frame::data(
            COLLAB_SUBPROTOCOL,
            writer_ref,
            0,
            CollabMessage::Update(update.into()).encode(),
        )))
        .await
        .unwrap();
    assert_no_document_frame(&mut blocked).await;
}

#[tokio::test]
async fn two_replicas_converge_through_a_shared_fanout() {
    // The multi-process guarantee the C-series exists for: two gateways (two web
    // replicas) on ONE shared fan-out + store — exactly how `with_collab_store`
    // wires a Redis deployment. A client on replica 1 edits; a client on replica
    // 2, which never saw that client, converges via the shared bus.
    let fanout: Arc<dyn Fanout> = Arc::new(LocalFanout::new());
    let store: Arc<dyn CollabStore> = Arc::new(MemoryCollabStore::new());
    let url1 = spawn_replica(fanout.clone(), store.clone()).await;
    let url2 = spawn_replica(fanout.clone(), store.clone()).await;
    let topic = topic("replicated");

    // A on replica 1 authors an edit and syncs it up; replica 1 publishes it to
    // the shared fan-out, where replica 2's apply loop folds it in.
    let (mut a, _) = connect_async(&url1).await.unwrap();
    let a_ref = join(&mut a, &topic).await;
    let doc_a = CollabDocument::new();
    doc_a.insert_text("body", 0, "shared-state");
    open_sync(&mut a, a_ref, &doc_a).await;
    drive(&mut a, a_ref, &doc_a).await;

    // B on replica 2 joins fresh. Replica 2 only begins folding the topic when B
    // subscribes, so poll the handshake a few times until its apply loop has
    // caught replica 2 up across the process boundary.
    let (mut b, _) = connect_async(&url2).await.unwrap();
    let b_ref = join(&mut b, &topic).await;
    let doc_b = CollabDocument::new();
    let mut converged = false;
    for _ in 0..20 {
        open_sync(&mut b, b_ref, &doc_b).await;
        drive(&mut b, b_ref, &doc_b).await;
        if doc_b.text("body").contains("shared-state") {
            converged = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        converged,
        "B on replica 2 should converge to replica 1's edit, got {:?}",
        doc_b.text("body")
    );
}
