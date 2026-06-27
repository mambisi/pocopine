#![cfg(not(target_arch = "wasm32"))]
//! End-to-end gateway tests: boot the axum router on an ephemeral port and
//! drive it with a real WebSocket client (tokio-tungstenite). Host-only (axum /
//! tokio): the wasm CI gate runs `--all-targets`, so this guard keeps it empty
//! on wasm32 rather than failing to resolve the host-only dev-deps.

use std::sync::{Arc, Mutex};

use bytes::Bytes;
use futures_util::{SinkExt, Stream, StreamExt};
use pocopine_auth::{AuthFuture, AuthProvider, AuthUser, RequestAuthExt};
use pocopine_core::server::RequestContext;
use pocopine_events::Topic;
use pocopine_realtime::client::{ClientSession, SessionEvent};
use pocopine_realtime::{
    Control, Frame, FrameKind, GatewayConfig, InboundData, Reaction, SubprotocolHandler, TopicSeq,
    WsGateway, routes, routes_with_auth,
};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::{Error as TungError, Message as WsMessage};

/// Boot `gateway` on a random local port; return the ws:// URL.
async fn spawn(gateway: WsGateway) -> String {
    spawn_app(routes(gateway)).await
}

async fn spawn_app(app: axum::Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("ws://{addr}/__pocopine/ws/v1")
}

struct QueryTokenAuth;

impl AuthProvider for QueryTokenAuth {
    fn authenticate<'a>(&'a self, ctx: &'a RequestContext) -> AuthFuture<'a, Option<AuthUser>> {
        Box::pin(async move {
            Ok((ctx.bearer_token() == Some("ok token")).then(|| AuthUser::new("u1")))
        })
    }
}

/// Read the next binary frame, skipping ping/pong/text. Each read is bounded by
/// a timeout so a regression that stops the expected traffic fails loudly with a
/// clear message instead of hanging until the whole CI job is killed.
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
        Control::SubscribeDenied { topic, reason } => {
            assert_eq!(topic, "topic:1");
            assert_eq!(reason, "forbidden");
        }
        other => panic!("expected SubscribeDenied, got {other:?}"),
    }
}

#[tokio::test]
async fn routes_with_auth_accepts_browser_access_token_query() {
    let gateway = WsGateway::local().with_access(|ctx, _topic| {
        assert!(
            !ctx.uri()
                .query()
                .unwrap_or_default()
                .contains("access_token"),
            "route helper must scrub the bearer token from downstream context"
        );
        if ctx.require_user().is_ok() {
            pocopine_realtime::TopicAccess::ReadWrite
        } else {
            pocopine_realtime::TopicAccess::Denied
        }
    });
    let url = spawn_app(routes_with_auth(gateway, QueryTokenAuth)).await;
    let (mut ws, _resp) = connect_async(format!("{url}?room=1&access_token=ok%20token"))
        .await
        .unwrap();
    assert!(matches!(next_control(&mut ws).await, Control::Hello { .. }));

    ws.send(send_bytes(Frame::subscribe(1, "topic:1")))
        .await
        .unwrap();
    assert!(matches!(
        next_control(&mut ws).await,
        Control::SubscribeAck { .. }
    ));
}

#[tokio::test]
async fn routes_with_auth_denies_when_browser_access_token_is_missing() {
    let gateway = WsGateway::local().with_access(|ctx, _topic| {
        if ctx.require_user().is_ok() {
            pocopine_realtime::TopicAccess::ReadWrite
        } else {
            pocopine_realtime::TopicAccess::Denied
        }
    });
    let url = spawn_app(routes_with_auth(gateway, QueryTokenAuth)).await;
    let (mut ws, _resp) = connect_async(url).await.unwrap();
    assert!(matches!(next_control(&mut ws).await, Control::Hello { .. }));

    ws.send(send_bytes(Frame::subscribe(1, "topic:1")))
        .await
        .unwrap();
    assert!(matches!(
        next_control(&mut ws).await,
        Control::SubscribeDenied { .. }
    ));
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

#[tokio::test]
async fn write_policy_allows_subscribe_but_denies_publish() {
    // Read-only: any topic may be joined, none may be published to.
    let gateway = WsGateway::local()
        .allow_all_topics()
        .with_write_policy(|_, _| false);
    let url = spawn(gateway).await;
    let (mut ws, _resp) = connect_async(url).await.unwrap();
    assert!(matches!(next_control(&mut ws).await, Control::Hello { .. }));

    // Joining is allowed by the (separate) topic policy.
    ws.send(send_bytes(Frame::subscribe(1, "topic:1")))
        .await
        .unwrap();
    let topic_ref = match next_control(&mut ws).await {
        Control::SubscribeAck { topic_ref, .. } => topic_ref,
        other => panic!("expected SubscribeAck, got {other:?}"),
    };

    // Publishing is refused by the write policy — the relay returns an error
    // instead of echoing the frame back.
    ws.send(send_bytes(Frame::data(1, topic_ref, 0, &b"x"[..])))
        .await
        .unwrap();
    match next_control(&mut ws).await {
        Control::Error { message, .. } => assert!(
            message.contains("forbidden"),
            "expected a forbidden write, got {message:?}"
        ),
        other => panic!("expected a forbidden error, got {other:?}"),
    }
}

/// A toy handler proving both [`Reaction`] paths: a `reply:`-prefixed echo back
/// to the sender and a `bcast:`-prefixed message fanned out to every subscriber.
struct PrefixHandler;

#[async_trait::async_trait]
impl SubprotocolHandler for PrefixHandler {
    async fn on_data(
        &self,
        inbound: InboundData<'_>,
    ) -> Result<Reaction, pocopine_realtime::WsError> {
        let mut reaction = Reaction::new();
        reaction.reply([b"reply:", &inbound.payload[..]].concat());
        reaction.broadcast([b"bcast:", &inbound.payload[..]].concat());
        Ok(reaction)
    }
}

#[tokio::test]
async fn handler_replies_to_sender_and_broadcasts_to_all() {
    const SUB: u64 = 7;
    let gateway = WsGateway::local()
        .allow_all_topics()
        .with_handler(SUB, Arc::new(PrefixHandler));
    let url = spawn(gateway).await;

    // Two clients on the same topic.
    let (mut a, _ra) = connect_async(url.clone()).await.unwrap();
    let (mut b, _rb) = connect_async(url).await.unwrap();
    assert!(matches!(next_control(&mut a).await, Control::Hello { .. }));
    assert!(matches!(next_control(&mut b).await, Control::Hello { .. }));
    for ws in [&mut a, &mut b] {
        ws.send(send_bytes(Frame::subscribe(SUB, "room:1")))
            .await
            .unwrap();
        assert!(matches!(
            next_control(ws).await,
            Control::SubscribeAck { .. }
        ));
    }

    // A sends one Data frame; the handler intercepts it (no raw relay).
    a.send(send_bytes(Frame::data(SUB, 1, 0, &b"hi"[..])))
        .await
        .unwrap();

    // A sees BOTH its direct reply (seq 0) and the broadcast (seq 1), in either
    // order; B sees only the broadcast.
    let f1 = next_data(&mut a).await;
    let f2 = next_data(&mut a).await;
    let mut a_payloads = [f1.payload_str().unwrap(), f2.payload_str().unwrap()];
    a_payloads.sort_unstable();
    assert_eq!(a_payloads, ["bcast:hi", "reply:hi"]);

    let on_b = next_data(&mut b).await;
    assert_eq!(on_b.payload_str().unwrap(), "bcast:hi");
    assert_eq!(on_b.seq, 1, "broadcast is a normal sequenced Data frame");
    assert_eq!(on_b.subprotocol_id, SUB);
}

/// Records topic active/idle lifecycle transitions; `on_data` is a no-op.
struct LifecycleRecorder {
    events: Arc<Mutex<Vec<String>>>,
}

#[async_trait::async_trait]
impl SubprotocolHandler for LifecycleRecorder {
    async fn on_data(
        &self,
        _inbound: InboundData<'_>,
    ) -> Result<Reaction, pocopine_realtime::WsError> {
        Ok(Reaction::new())
    }

    fn on_topic_active(&self, topic: &Topic) {
        self.events
            .lock()
            .unwrap()
            .push(format!("active:{}", topic.as_str()));
    }

    fn on_topic_idle(&self, topic: &Topic) {
        self.events
            .lock()
            .unwrap()
            .push(format!("idle:{}", topic.as_str()));
    }
}

#[tokio::test]
async fn topic_active_idle_fires_on_first_and_last_subscriber() {
    const SUB: u64 = 5;
    let events = Arc::new(Mutex::new(Vec::new()));
    let gateway = WsGateway::local().allow_all_topics().with_handler(
        SUB,
        Arc::new(LifecycleRecorder {
            events: events.clone(),
        }),
    );
    let url = spawn(gateway).await;

    // Two clients join the same topic; active fires once (0→1), not per client.
    let (mut a, _ra) = connect_async(url.clone()).await.unwrap();
    let (mut b, _rb) = connect_async(url).await.unwrap();
    assert!(matches!(next_control(&mut a).await, Control::Hello { .. }));
    assert!(matches!(next_control(&mut b).await, Control::Hello { .. }));
    let mut refs = Vec::new();
    for ws in [&mut a, &mut b] {
        ws.send(send_bytes(Frame::subscribe(SUB, "room:1")))
            .await
            .unwrap();
        match next_control(ws).await {
            Control::SubscribeAck { topic_ref, .. } => refs.push(topic_ref),
            other => panic!("expected SubscribeAck, got {other:?}"),
        }
    }
    // The lifecycle hook fires before the ack is sent, so by now it has run.
    assert_eq!(*events.lock().unwrap(), vec!["active:room:1"]);

    // A unsubscribes (2→1): no idle yet.
    a.send(send_bytes(Frame::unsubscribe(refs[0])))
        .await
        .unwrap();
    assert!(matches!(
        next_control(&mut a).await,
        Control::Unsubscribed { .. }
    ));
    assert_eq!(*events.lock().unwrap(), vec!["active:room:1"]);

    // B unsubscribes (1→0): idle fires.
    b.send(send_bytes(Frame::unsubscribe(refs[1])))
        .await
        .unwrap();
    assert!(matches!(
        next_control(&mut b).await,
        Control::Unsubscribed { .. }
    ));
    assert_eq!(
        *events.lock().unwrap(),
        vec!["active:room:1", "idle:room:1"]
    );
}

/// The wasm client's session state machine ([`ClientSession`]) drives the real
/// gateway end to end: it must turn the live Hello/SubscribeAck/Data frames into
/// the right events and produce addressable Subscribe/Data frames the server
/// accepts. Proves the browser client's protocol half interoperates with the
/// host server (the wasm WebSocket I/O shell is the only untested layer above).
#[tokio::test]
async fn client_session_drives_the_gateway() {
    let url = spawn(WsGateway::local().allow_all_topics()).await;
    let (mut ws, _) = connect_async(&url).await.unwrap();
    let mut session = ClientSession::new();

    // Hello -> Connected.
    let hello = next_frame(&mut ws).await;
    match session.on_frame(&hello).unwrap() {
        Some(SessionEvent::Connected { heartbeat_ms, .. }) => assert!(heartbeat_ms > 0),
        other => panic!("expected Connected, got {other:?}"),
    }
    assert!(session.session_id().is_some());

    // Subscribe -> SubscribeAck binds the topic_ref.
    let topic = "room:1";
    ws.send(send_bytes(session.subscribe(topic, 1)))
        .await
        .unwrap();
    let ack = next_frame(&mut ws).await;
    let topic_ref = match session.on_frame(&ack).unwrap() {
        Some(SessionEvent::Subscribed {
            topic: acked,
            topic_ref,
        }) => {
            assert_eq!(acked, topic);
            topic_ref
        }
        other => panic!("expected Subscribed, got {other:?}"),
    };
    assert!(topic_ref > 0);

    // Before the ack there was no ref; now data() can address the topic. The
    // default relay echoes the publish back to this subscriber.
    let frame = session
        .data(topic, 1, Bytes::from_static(b"ping"))
        .expect("topic_ref is bound after the ack");
    ws.send(send_bytes(frame)).await.unwrap();

    let data = next_data(&mut ws).await;
    match session.on_frame(&data).unwrap() {
        Some(SessionEvent::Data {
            topic: from,
            payload,
        }) => {
            assert_eq!(from, topic);
            assert_eq!(payload, Bytes::from_static(b"ping"));
        }
        other => panic!("expected Data, got {other:?}"),
    }
}
