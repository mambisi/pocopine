#![cfg(not(target_arch = "wasm32"))]
//! RFC-123 Phase 4 — one `pocopine.realtime.session` span per WebSocket,
//! a child of the upgrade's `pocopine.http.request`, with a short
//! `pocopine.realtime.message` span per inbound frame handled and per
//! outbound data frame delivered.

use futures_util::{SinkExt, Stream, StreamExt};
use pocopine_observe::test_support::SpanCapture;
use pocopine_realtime::{Control, Frame, FrameKind, WsGateway, routes};
use pocopine_server::{RequestEventOptions, Server};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::{Error as TungError, Message as WsMessage};

async fn spawn_with_request_layer(gateway: WsGateway) -> String {
    let app = Server::new(routes(gateway))
        .request_events(RequestEventOptions::default())
        .try_finalize()
        .expect("finalize");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("ws://{addr}/__pocopine/ws/v1")
}

async fn next_frame<S>(ws: &mut S) -> Frame
where
    S: Stream<Item = Result<WsMessage, TungError>> + Unpin,
{
    loop {
        let message = tokio::time::timeout(std::time::Duration::from_secs(5), ws.next())
            .await
            .expect("timed out waiting for a frame")
            .expect("stream ended")
            .expect("ws transport error");
        match message {
            WsMessage::Binary(data) => return Frame::decode(&data).expect("decode frame"),
            WsMessage::Close(_) => panic!("server closed unexpectedly"),
            _ => {}
        }
    }
}

async fn next_control<S>(ws: &mut S) -> Control
where
    S: Stream<Item = Result<WsMessage, TungError>> + Unpin,
{
    let frame = next_frame(ws).await;
    assert_eq!(frame.kind, FrameKind::Control);
    Control::decode(&frame.payload).expect("decode control")
}

#[test]
fn session_and_message_spans_hang_from_the_upgrade_request() {
    pocopine_server::__reset_for_test();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let capture = SpanCapture::new();

    capture.run(|| {
        rt.block_on(async {
            let url = spawn_with_request_layer(WsGateway::local().allow_all_topics()).await;
            let (mut ws, _resp) = connect_async(url).await.unwrap();
            assert!(matches!(next_control(&mut ws).await, Control::Hello { .. }));

            ws.send(WsMessage::Binary(Frame::subscribe(1, "topic:1").encode()))
                .await
                .unwrap();
            let topic_ref = match next_control(&mut ws).await {
                Control::SubscribeAck { topic_ref, .. } => topic_ref,
                other => panic!("expected SubscribeAck, got {other:?}"),
            };
            ws.send(WsMessage::Binary(
                Frame::data(1, topic_ref, 0, &b"hello"[..]).encode(),
            ))
            .await
            .unwrap();
            loop {
                let frame = next_frame(&mut ws).await;
                if frame.kind == FrameKind::Data {
                    assert_eq!(frame.seq, 1);
                    break;
                }
            }
            ws.close(None).await.unwrap();
            // Let the server side observe the close and finish its loop.
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        });
    });

    let request = capture
        .spans_named("pocopine.http.request")
        .into_iter()
        .find(|s| s.field("http.response.status_code") == Some("101"))
        .expect("the upgrade request span");
    let session = capture.span("pocopine.realtime.session");
    assert_eq!(session.parent, Some(request.id));
    assert_eq!(session.field("otel.kind"), Some("server"));
    assert!(session.field("pocopine.realtime.session_id").is_some());
    assert_eq!(session.field("otel.status_code"), Some("OK"));
    assert!(session.closed, "the session span closed with the socket");

    let opened = capture
        .events()
        .into_iter()
        .find(|e| e.field("message") == Some("ws session opened"))
        .expect("ws session opened");
    assert_eq!(
        opened.ancestry(),
        ["pocopine.http.request", "pocopine.realtime.session"]
    );

    let messages = capture.spans_named("pocopine.realtime.message");
    let inbound: Vec<_> = messages
        .iter()
        .filter(|m| m.field("pocopine.message.direction") == Some("in"))
        .collect();
    let kinds: Vec<&str> = inbound
        .iter()
        .filter_map(|m| m.field("pocopine.message.kind"))
        .collect();
    assert_eq!(kinds, ["subscribe", "data"], "{inbound:?}");
    for m in &inbound {
        assert_eq!(m.parent, Some(session.id));
        assert_eq!(m.field("otel.status_code"), Some("OK"));
        assert!(m.field("pocopine.message.bytes").is_some());
    }
    let outbound: Vec<_> = messages
        .iter()
        .filter(|m| m.field("pocopine.message.direction") == Some("out"))
        .collect();
    // Every outbound frame — Hello, SubscribeAck, the data echo — has a span
    // that closed with the socket write's result.
    assert!(outbound.len() >= 3, "{outbound:?}");
    for m in &outbound {
        assert_eq!(m.field("otel.status_code"), Some("OK"), "{m:?}");
        assert!(m.closed);
    }
    let data: Vec<_> = outbound
        .iter()
        .filter(|m| m.field("pocopine.message.kind") == Some("data"))
        .collect();
    assert_eq!(data.len(), 1, "{outbound:?}");
    assert_eq!(
        data[0].parent,
        Some(session.id),
        "delivered by the pump, under the session"
    );
    assert_eq!(data[0].field("pocopine.message.seq"), Some("1"));
    // A control reply sent while handling an inbound frame nests under that
    // frame's span (an answer), never outside the session.
    let subscribe_in = inbound
        .iter()
        .find(|m| m.field("pocopine.message.kind") == Some("subscribe"))
        .unwrap();
    let ack = outbound
        .iter()
        .find(|m| m.parent == Some(subscribe_in.id))
        .expect("SubscribeAck under the inbound subscribe span");
    assert_eq!(ack.field("pocopine.message.kind"), Some("control"));
}
