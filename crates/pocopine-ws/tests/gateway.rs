//! End-to-end gateway tests: boot the axum router on an ephemeral port and
//! drive it with a real WebSocket client (tokio-tungstenite).

use futures_util::{SinkExt, Stream, StreamExt};
use pocopine_ws::{Control, Frame, FrameKind, GatewayConfig, TopicSeq, WsGateway, routes};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::{Error as TungError, Message as WsMessage};

/// Boot `gateway` on a random local port; return the ws:// URL.
async fn spawn(gateway: WsGateway) -> String {
    let app = routes(gateway);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("ws://{addr}/__pocopine/ws/v1")
}

/// Read the next binary frame, skipping ping/pong/text.
async fn next_frame<S>(ws: &mut S) -> Frame
where
    S: Stream<Item = Result<WsMessage, TungError>> + Unpin,
{
    loop {
        match ws
            .next()
            .await
            .expect("stream ended")
            .expect("ws transport error")
        {
            WsMessage::Binary(data) => return Frame::decode(&data).expect("decode frame"),
            WsMessage::Close(_) => panic!("server closed unexpectedly"),
            _ => {}
        }
    }
}

/// Read the next frame and decode it as a control body.
async fn next_control<S>(ws: &mut S) -> Control
where
    S: Stream<Item = Result<WsMessage, TungError>> + Unpin,
{
    let frame = next_frame(ws).await;
    assert_eq!(frame.kind, FrameKind::Control, "expected a control frame");
    Control::decode(&frame.payload).expect("decode control")
}

/// Read frames until a Data frame arrives (skipping control frames).
async fn next_data<S>(ws: &mut S) -> Frame
where
    S: Stream<Item = Result<WsMessage, TungError>> + Unpin,
{
    loop {
        let frame = next_frame(ws).await;
        if frame.kind == FrameKind::Data {
            return frame;
        }
    }
}

fn send_bytes(frame: Frame) -> WsMessage {
    WsMessage::Binary(frame.encode())
}

#[tokio::test]
async fn subscribe_then_publish_echoes_with_seq() {
    let url = spawn(WsGateway::local().allow_all_topics()).await;
    let (mut ws, _resp) = connect_async(url).await.unwrap();

    assert!(matches!(next_control(&mut ws).await, Control::Hello { .. }));

    ws.send(send_bytes(Frame::subscribe(1, "topic:1")))
        .await
        .unwrap();
    let topic_ref = match next_control(&mut ws).await {
        Control::SubscribeAck { topic, topic_ref } => {
            assert_eq!(topic, "topic:1");
            topic_ref
        }
        other => panic!("expected SubscribeAck, got {other:?}"),
    };

    ws.send(send_bytes(Frame::data(1, topic_ref, 0, &b"hello"[..])))
        .await
        .unwrap();
    let echo = next_data(&mut ws).await;
    assert_eq!(echo.kind, FrameKind::Data);
    assert_eq!(echo.topic_ref, topic_ref);
    assert_eq!(echo.subprotocol_id, 1);
    assert_eq!(echo.seq, 1, "first delivered message has seq 1");
    assert_eq!(echo.payload_str().unwrap(), "hello");
}

#[tokio::test]
async fn heartbeat_is_acked() {
    let url = spawn(WsGateway::local().allow_all_topics()).await;
    let (mut ws, _resp) = connect_async(url).await.unwrap();
    assert!(matches!(next_control(&mut ws).await, Control::Hello { .. }));

    let heartbeat = Control::Heartbeat { acks: vec![] }.into_frame().unwrap();
    ws.send(send_bytes(heartbeat)).await.unwrap();
    assert!(matches!(
        next_control(&mut ws).await,
        Control::HeartbeatAck {}
    ));
}

#[tokio::test]
async fn default_deny_rejects_subscribe() {
    // No allow_* call: the gateway denies every topic.
    let url = spawn(WsGateway::local()).await;
    let (mut ws, _resp) = connect_async(url).await.unwrap();
    assert!(matches!(next_control(&mut ws).await, Control::Hello { .. }));

    ws.send(send_bytes(Frame::subscribe(1, "topic:1")))
        .await
        .unwrap();
    match next_control(&mut ws).await {
        Control::Error { code, .. } => assert_eq!(code, "forbidden_topic"),
        other => panic!("expected forbidden error, got {other:?}"),
    }
}

#[tokio::test]
async fn resume_replays_messages_after_last_seq() {
    let url = spawn(WsGateway::local().allow_all_topics()).await;

    // Connection 1: subscribe and publish two messages.
    let (mut ws1, _r1) = connect_async(url.clone()).await.unwrap();
    assert!(matches!(
        next_control(&mut ws1).await,
        Control::Hello { .. }
    ));
    ws1.send(send_bytes(Frame::subscribe(1, "topic:1")))
        .await
        .unwrap();
    let topic_ref = match next_control(&mut ws1).await {
        Control::SubscribeAck { topic_ref, .. } => topic_ref,
        other => panic!("expected SubscribeAck, got {other:?}"),
    };
    ws1.send(send_bytes(Frame::data(1, topic_ref, 0, &b"m1"[..])))
        .await
        .unwrap();
    assert_eq!(next_data(&mut ws1).await.seq, 1);
    ws1.send(send_bytes(Frame::data(1, topic_ref, 0, &b"m2"[..])))
        .await
        .unwrap();
    assert_eq!(next_data(&mut ws1).await.seq, 2);
    drop(ws1); // disconnect

    // Connection 2: resume the topic after seq 1 — expect the seq-2 replay.
    let (mut ws2, _r2) = connect_async(url).await.unwrap();
    assert!(matches!(
        next_control(&mut ws2).await,
        Control::Hello { .. }
    ));
    let resume = Control::Resume {
        session_id: "any".into(),
        topics: vec![TopicSeq {
            topic: "topic:1".into(),
            last_seq: 1,
            subprotocol_id: 1,
        }],
    };
    ws2.send(send_bytes(resume.into_frame().unwrap()))
        .await
        .unwrap();

    let replayed = next_data(&mut ws2).await;
    assert_eq!(
        replayed.seq, 2,
        "resume replays only messages after last_seq"
    );
    assert_eq!(replayed.payload_str().unwrap(), "m2");
}

#[tokio::test]
async fn subscription_cap_rejects_extra_topics() {
    let config = GatewayConfig {
        max_subscriptions: 1,
        ..GatewayConfig::default()
    };
    let url = spawn(WsGateway::local().allow_all_topics().with_config(config)).await;
    let (mut ws, _resp) = connect_async(url).await.unwrap();
    assert!(matches!(next_control(&mut ws).await, Control::Hello { .. }));

    // First subscription is accepted.
    ws.send(send_bytes(Frame::subscribe(1, "topic:1")))
        .await
        .unwrap();
    assert!(matches!(
        next_control(&mut ws).await,
        Control::SubscribeAck { .. }
    ));

    // Second distinct topic exceeds the per-connection cap.
    ws.send(send_bytes(Frame::subscribe(1, "topic:2")))
        .await
        .unwrap();
    match next_control(&mut ws).await {
        Control::Error { code, .. } => assert_eq!(code, "too_many_subscriptions"),
        other => panic!("expected too_many_subscriptions, got {other:?}"),
    }
}
