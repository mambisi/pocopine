//! End-to-end collab over the realtime gateway: boot the axum router with a
//! [`CollabSync`] handler registered, connect real WebSocket clients, and prove
//! the Yjs sync handshake catches a fresh peer up and live edits converge.

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, Stream, StreamExt};
use pocopine_collab::{COLLAB_SUBPROTOCOL, CollabDocument, CollabMessage, CollabSync};
use pocopine_realtime::{Control, Frame, FrameKind, WsGateway, routes};
use tokio::time::timeout;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::{Error as TungError, Message as WsMessage};

/// Quiet window after which the handshake/broadcast chatter is considered drained.
const IDLE: Duration = Duration::from_millis(400);

/// Boot a collab-enabled gateway on a random port; return its ws:// URL.
async fn spawn() -> String {
    let gateway = WsGateway::local()
        .allow_all_topics()
        .with_handler(COLLAB_SUBPROTOCOL, Arc::new(CollabSync::new()));
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

/// Send the opening SyncStep1 (this peer's state vector).
async fn open_sync(ws: &mut Ws, topic_ref: u64, doc: &CollabDocument) {
    let msg = CollabMessage::SyncStep1(doc.state_vector().into());
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
/// until the connection is quiet for [`IDLE`]: reply to the server's SyncStep1
/// with the diff it is missing, and apply SyncStep2/Update messages locally.
/// CRDT idempotency makes the replayed/echoed duplicates harmless.
async fn drive(ws: &mut Ws, topic_ref: u64, doc: &CollabDocument) {
    while let Ok(frame) = timeout(IDLE, next_frame(ws)).await {
        if frame.kind != FrameKind::Data {
            continue;
        }
        match CollabMessage::decode(&frame.payload).unwrap() {
            CollabMessage::SyncStep1(sv) => {
                let diff = doc.diff(&sv).unwrap();
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
            CollabMessage::SyncStep2(update) | CollabMessage::Update(update) => {
                doc.apply_update(&update).unwrap();
            }
        }
    }
}

#[tokio::test]
async fn two_clients_converge_over_the_gateway() {
    let url = spawn().await;
    let topic = "collab:doc1";

    // Client A joins with a local edit and syncs it up to the server.
    let (mut a, _) = connect_async(&url).await.unwrap();
    let a_ref = join(&mut a, topic).await;
    let doc_a = CollabDocument::new();
    doc_a.insert_text("body", 0, "alpha ");
    open_sync(&mut a, a_ref, &doc_a).await;
    drive(&mut a, a_ref, &doc_a).await;

    // Client B joins fresh; the handshake catches it up to A's edit.
    let (mut b, _) = connect_async(&url).await.unwrap();
    let b_ref = join(&mut b, topic).await;
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
