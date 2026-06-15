//! The axum WebSocket route and per-connection session loop (RFC 073 §6, §8–§11).
//!
//! `routes(gateway)` mounts a single upgrade endpoint, mirroring
//! `pocopine_live::routes`. Each upgraded connection runs [`run_session`],
//! which sends a [`Control::Hello`], then multiplexes:
//!
//! - inbound frames (Subscribe / Unsubscribe / Data / Heartbeat / Resume),
//! - a bounded outbound queue drained by a writer task,
//! - per-topic pump tasks forwarding fan-out messages as Data frames,
//! - a heartbeat watchdog that closes zombie connections.
//!
//! The session loop NEVER blocks on the outbound queue: control/ack frames are
//! enqueued with `try_send`, and a full queue (a slow/stuck consumer) closes
//! the connection. This keeps the watchdog branch of the `select!` always
//! reachable, so a peer that holds the socket open but stops reading is still
//! reaped.

use std::collections::HashMap;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{FromRequestParts, State};
use axum::http::Request;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use futures_util::{SinkExt, StreamExt};
use pocopine_auth::RequestContext;
use pocopine_events::Topic;
use pocopine_observe::LOG_TARGET;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{Instant, MissedTickBehavior, interval};

use super::control::Control;
use super::error::WsError;
use super::fanout::TopicStream;
use super::frame::{Frame, FrameKind};
use super::gateway::{GatewayConfig, WS_PROTOCOL_V1, WsGateway};
use super::handler::InboundData;

/// Mount path for the gateway upgrade endpoint.
pub const WS_STREAM_PATH: &str = "/__pocopine/ws/v1";

/// Build the axum router for the gateway. Mirrors `pocopine_live::routes`:
/// the [`WsGateway`] is the router state, reachable in the handler via
/// `State<WsGateway>`.
pub fn routes(gateway: WsGateway) -> Router {
    Router::new()
        .route(WS_STREAM_PATH, get(upgrade_handler))
        .with_state(gateway)
}

async fn upgrade_handler(State(gateway): State<WsGateway>, request: Request<Body>) -> Response {
    let config = gateway.config();
    // Build the auth context from request parts BEFORE upgrading; the upgraded
    // socket task no longer has the original request. `RequestContext` is not
    // an axum extractor (no `FromRequestParts` impl) — assemble it by hand,
    // exactly as pocopine-live's SSE handler does.
    let (mut parts, _body) = request.into_parts();
    let ctx = RequestContext::from_parts(
        parts.method.clone(),
        parts.uri.clone(),
        parts.headers.clone(),
        parts.extensions.clone(),
    );
    match WebSocketUpgrade::from_request_parts(&mut parts, &()).await {
        // Cap message/frame size at the transport layer so an oversized message
        // is rejected before the whole payload is buffered (the in-handler
        // `max_frame_bytes` check is then a cheap second guard).
        Ok(upgrade) => upgrade
            .protocols([WS_PROTOCOL_V1])
            .max_message_size(config.max_frame_bytes)
            .max_frame_size(config.max_frame_bytes)
            .on_upgrade(move |socket| run_session(socket, gateway, ctx)),
        Err(rejection) => rejection.into_response(),
    }
}

/// A live topic subscription owned by a session.
struct TopicEntry {
    topic: Topic,
    name: String,
    pump: JoinHandle<()>,
}

/// Per-connection state machine.
struct Session {
    session_id: String,
    gateway: WsGateway,
    ctx: RequestContext,
    out: mpsc::Sender<Message>,
    config: GatewayConfig,
    topics: HashMap<u64, TopicEntry>,
    topic_by_name: HashMap<String, u64>,
    next_topic_ref: u64,
    last_heartbeat: Instant,
    /// Set when an outbound enqueue fails (full queue / writer gone); the
    /// session loop closes the connection at the next boundary.
    should_close: bool,
}

/// Drive one upgraded WebSocket connection to completion.
async fn run_session(socket: WebSocket, gateway: WsGateway, ctx: RequestContext) {
    let config = gateway.config();
    let hb_ms = config.heartbeat_interval_ms.max(1);
    let (mut sink, mut stream) = socket.split();
    let (out_tx, mut out_rx) = mpsc::channel::<Message>(config.outbound_queue.max(1));

    // Writer task: drains the bounded outbound queue to the socket.
    let writer = tokio::spawn(async move {
        while let Some(msg) = out_rx.recv().await {
            if sink.send(msg).await.is_err() {
                break;
            }
        }
        let _ = sink.close().await;
    });

    let session_id = gateway.next_session_id();
    tracing::info!(target: LOG_TARGET, session = %session_id, "ws session opened");

    let hello = Control::Hello {
        session_id: session_id.clone(),
        heartbeat_interval_ms: hb_ms,
        protocol: WS_PROTOCOL_V1.to_string(),
    };
    if !send_control(&out_tx, &hello).await {
        writer.abort();
        return;
    }

    let mut session = Session {
        session_id,
        gateway,
        ctx,
        out: out_tx,
        config,
        topics: HashMap::new(),
        topic_by_name: HashMap::new(),
        next_topic_ref: 1,
        last_heartbeat: Instant::now(),
        should_close: false,
    };

    let mut watchdog = interval(Duration::from_millis(u64::from(hb_ms)));
    watchdog.set_missed_tick_behavior(MissedTickBehavior::Delay);
    watchdog.tick().await; // discard the immediate first tick

    loop {
        tokio::select! {
            inbound = stream.next() => match inbound {
                Some(Ok(Message::Binary(data))) => {
                    if let Err(err) = session.handle_message(&data).await {
                        tracing::warn!(
                            target: LOG_TARGET,
                            session = %session.session_id,
                            error = %err,
                            "ws frame rejected"
                        );
                        session.send(&Control::error("bad_frame", err.to_string()));
                    }
                }
                Some(Ok(Message::Close(_))) | None => break,
                Some(Ok(Message::Text(_))) => {
                    session.send(&Control::error("bad_frame", "binary frames only"));
                }
                Some(Ok(Message::Ping(_))) | Some(Ok(Message::Pong(_))) => {}
                Some(Err(_)) => break,
            },
            _ = watchdog.tick() => {
                if session.is_zombie() {
                    tracing::warn!(
                        target: LOG_TARGET,
                        session = %session.session_id,
                        "ws connection zombied (missed heartbeats)"
                    );
                    break;
                }
            }
        }

        if session.should_close {
            tracing::warn!(
                target: LOG_TARGET,
                session = %session.session_id,
                "ws outbound queue full; closing slow consumer"
            );
            break;
        }
    }

    session.shutdown();
    writer.abort();
    tracing::info!(target: LOG_TARGET, session = %session.session_id, "ws session closed");
}

impl Session {
    fn is_zombie(&self) -> bool {
        // Clamp the interval the same way the watchdog period is clamped, so a
        // configured interval of 0 cannot make every fresh connection a zombie.
        let interval_ms = u64::from(self.config.heartbeat_interval_ms.max(1));
        let grace = u64::from(self.config.zombie_grace.max(1));
        self.last_heartbeat.elapsed() > Duration::from_millis(interval_ms * grace)
    }

    /// Enqueue a control frame WITHOUT blocking. A full queue (slow consumer)
    /// or a closed writer marks the session for close; it never parks the
    /// session loop, so the heartbeat watchdog stays live.
    fn send(&mut self, control: &Control) -> bool {
        match control.into_frame() {
            Ok(frame) => match self.out.try_send(Message::Binary(frame.encode())) {
                Ok(()) => true,
                Err(_) => {
                    self.should_close = true;
                    false
                }
            },
            Err(_) => false,
        }
    }

    async fn handle_message(&mut self, data: &[u8]) -> Result<(), WsError> {
        if data.len() > self.config.max_frame_bytes {
            return Err(WsError::TooLarge {
                size: data.len(),
                max: self.config.max_frame_bytes,
            });
        }
        let frame = Frame::decode(data)?;
        match frame.kind {
            FrameKind::Control => self.handle_control(&frame).await,
            FrameKind::Subscribe => self.handle_subscribe(&frame).await,
            FrameKind::Unsubscribe => self.handle_unsubscribe(&frame),
            FrameKind::Data => self.handle_data(&frame).await,
        }
    }

    async fn handle_control(&mut self, frame: &Frame) -> Result<(), WsError> {
        match Control::decode(&frame.payload)? {
            Control::Heartbeat { .. } => {
                self.last_heartbeat = Instant::now();
                self.send(&Control::HeartbeatAck {});
                Ok(())
            }
            Control::Resume { topics, .. } => {
                for entry in topics {
                    self.open_topic(
                        &entry.topic,
                        entry.subprotocol_id,
                        Some(entry.last_seq),
                        true,
                    )
                    .await;
                }
                Ok(())
            }
            _ => Err(WsError::protocol("unexpected control frame from client")),
        }
    }

    async fn handle_subscribe(&mut self, frame: &Frame) -> Result<(), WsError> {
        let topic_name = frame.payload_str()?.to_string();
        self.open_topic(&topic_name, frame.subprotocol_id, None, false)
            .await;
        Ok(())
    }

    fn handle_unsubscribe(&mut self, frame: &Frame) -> Result<(), WsError> {
        if let Some(entry) = self.topics.remove(&frame.topic_ref) {
            entry.pump.abort();
            self.topic_by_name.remove(&entry.name);
            self.send(&Control::Unsubscribed { topic: entry.name });
        }
        Ok(())
    }

    async fn handle_data(&mut self, frame: &Frame) -> Result<(), WsError> {
        let (topic, name) = match self.topics.get(&frame.topic_ref) {
            Some(entry) => (entry.topic.clone(), entry.name.clone()),
            None => {
                return Err(WsError::protocol(format!(
                    "data for unknown topic_ref {}",
                    frame.topic_ref
                )));
            }
        };
        // Re-check join authorization on every inbound frame (RFC 073 §10.1).
        if !self.gateway.authorize(&self.ctx, &topic) {
            return Err(WsError::forbidden(name));
        }
        // Publish authorization is a second, finer gate (read-only connections
        // may join but not write); handlers receive it, the relay enforces it.
        let can_write = self.gateway.authorize_write(&self.ctx, &topic);

        // A registered sub-protocol handler (e.g. collab) intercepts the frame
        // and runs stateful server logic; every other sub-protocol is a pure
        // publish-to-fan-out relay.
        if let Some(handler) = self.gateway.handler(frame.subprotocol_id) {
            let reaction = handler
                .on_data(InboundData {
                    topic: &topic,
                    payload: &frame.payload,
                    can_write,
                })
                .await?;
            for payload in reaction.replies {
                // Out-of-band reply to THIS connection only (seq 0; the
                // per-subscription seq is reserved for fanned-out Data frames).
                // Non-blocking enqueue keeps the heartbeat watchdog live.
                let reply = Frame::data(frame.subprotocol_id, frame.topic_ref, 0, payload);
                if self.out.try_send(Message::Binary(reply.encode())).is_err() {
                    self.should_close = true;
                    break;
                }
            }
            for payload in reaction.broadcasts {
                self.gateway.fanout().publish(&topic, payload).await?;
            }
            return Ok(());
        }

        // Default relay: a Data frame IS a publish, so it requires write
        // authorization (the handler path delegates this gate to the handler).
        if !can_write {
            return Err(WsError::forbidden(name));
        }
        self.gateway
            .fanout()
            .publish(&topic, frame.payload.clone())
            .await?;
        Ok(())
    }

    /// Resolve, authorize, subscribe, spawn the pump, and acknowledge a topic.
    /// `after = Some(_)` (a Resume) replays from that cursor and emits
    /// [`Control::Resumed`]; `after = None` (a fresh Subscribe) emits
    /// [`Control::SubscribeAck`]. A re-subscribe to a live topic replaces it,
    /// but only AFTER the new subscription is confirmed gap-free, so a gapped
    /// Resume cannot tear down a healthy subscription.
    async fn open_topic(
        &mut self,
        topic_name: &str,
        subprotocol_id: u64,
        after: Option<u64>,
        resumed: bool,
    ) {
        if topic_name.len() > self.config.max_topic_bytes {
            self.send(&Control::error("bad_topic", "topic name too long"));
            return;
        }
        let topic = match self.gateway.resolve(topic_name) {
            Ok(topic) => topic,
            Err(err) => {
                self.send(&Control::error("bad_topic", err.to_string()));
                return;
            }
        };
        if !self.gateway.authorize(&self.ctx, &topic) {
            self.send(&Control::error(
                "forbidden_topic",
                format!("topic '{topic_name}' denied"),
            ));
            return;
        }

        // Per-connection subscription cap (new topics only; re-subscribe to an
        // already-joined topic is always allowed).
        let already_subscribed = self.topic_by_name.contains_key(topic_name);
        if !already_subscribed && self.topics.len() >= self.config.max_subscriptions {
            self.send(&Control::error(
                "too_many_subscriptions",
                "subscription limit reached",
            ));
            return;
        }

        // Open the new subscription BEFORE tearing down any existing one.
        let stream = match self.gateway.fanout().subscribe(&topic, after).await {
            Ok(stream) => stream,
            Err(err) => {
                self.send(&Control::error("subscribe_failed", err.to_string()));
                return;
            }
        };
        if stream.gap() {
            // Unreplayable cursor: tell the client to re-subscribe fresh and
            // leave any existing healthy subscription untouched.
            self.send(&Control::gap(topic_name, "cursor_not_replayable"));
            return;
        }

        // Commit: replace any existing subscription to this topic.
        if let Some(old_ref) = self.topic_by_name.remove(topic_name)
            && let Some(entry) = self.topics.remove(&old_ref)
        {
            entry.pump.abort();
        }

        let topic_ref = self.next_topic_ref;
        self.next_topic_ref += 1;

        let pump = tokio::spawn(pump_topic(
            stream,
            self.out.clone(),
            subprotocol_id,
            topic_ref,
            topic_name.to_string(),
        ));
        self.topics.insert(
            topic_ref,
            TopicEntry {
                topic,
                name: topic_name.to_string(),
                pump,
            },
        );
        self.topic_by_name.insert(topic_name.to_string(), topic_ref);

        let ack = if resumed {
            Control::Resumed {
                topic: topic_name.to_string(),
            }
        } else {
            Control::SubscribeAck {
                topic: topic_name.to_string(),
                topic_ref,
            }
        };
        self.send(&ack);
    }

    fn shutdown(&mut self) {
        for (_, entry) in self.topics.drain() {
            entry.pump.abort();
        }
        self.topic_by_name.clear();
    }
}

/// Forward one topic's fan-out messages to the connection as Data frames, each
/// carrying its per-subscription `seq`.
async fn pump_topic(
    mut stream: TopicStream,
    out: mpsc::Sender<Message>,
    subprotocol_id: u64,
    topic_ref: u64,
    topic_name: String,
) {
    loop {
        match stream.next().await {
            Ok(Some((seq, payload))) => {
                let frame = Frame::data(subprotocol_id, topic_ref, seq, payload);
                if out.send(Message::Binary(frame.encode())).await.is_err() {
                    break; // connection closed
                }
            }
            Ok(None) => break, // topic source closed cleanly
            Err(err) => {
                // Any source error (lag, gap, backend/connection loss) tells the
                // client to re-subscribe fresh from its cursor; never die silent.
                let reason = match err {
                    WsError::Lagged(_) => "subscription_lagged",
                    WsError::Gap => "cursor_not_replayable",
                    _ => "source_error",
                };
                let _ = send_control(&out, &Control::gap(&topic_name, reason)).await;
                break;
            }
        }
    }
}

/// Encode and enqueue a control frame (awaiting backpressure); returns whether
/// it was queued. Used by the per-topic pump tasks and the pre-loop Hello, where
/// blocking on the queue is safe; the session loop uses [`Session::send`].
async fn send_control(out: &mpsc::Sender<Message>, control: &Control) -> bool {
    match control.into_frame() {
        Ok(frame) => out.send(Message::Binary(frame.encode())).await.is_ok(),
        Err(_) => false,
    }
}
