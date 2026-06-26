use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::response::IntoResponse;
use axum::routing::get;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::{Mutex, mpsc};
use vectis_crdt::awareness::decode_cursor;
use vectis_crdt::causal_buffer::CausalBuffer;
use vectis_crdt::document::{Document, Operation};
use vectis_crdt::encoding::{
    decode_update, decode_vector_clock, encode_snapshot, encode_state_vector, encode_update,
};
use vectis_crdt::gc::GcConfig;
use vectis_crdt::types::{ActorId, VectorClock};
use vectis_protocol::{ProtocolMessage, decode_message, encode_message};

type Rooms = Arc<Mutex<HashMap<String, RoomState>>>;

#[derive(Clone)]
struct ServerState {
    rooms: Rooms,
}

struct RoomState {
    doc: Document,
    buffer: CausalBuffer,
    op_log: Vec<Operation>,
    op_log_base: VectorClock,
    clients: HashMap<ActorId, ClientState>,
    sessions: HashMap<String, ActorId>,
    next_actor: u64,
    next_token: u64,
}

struct ClientState {
    sender: mpsc::UnboundedSender<Vec<u8>>,
    version: VectorClock,
}

#[derive(serde::Deserialize)]
struct WsParams {
    room: Option<String>,
}

#[tokio::main]
async fn main() {
    let state = ServerState {
        rooms: Arc::new(Mutex::new(HashMap::new())),
    };

    let app = Router::new()
        .route("/", get(index))
        .route("/ws", get(ws_handler))
        .with_state(state);

    let addr: SocketAddr = std::env::var("VECTIS_ADDR")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or_else(|| SocketAddr::from(([127, 0, 0, 1], 3000)));

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("failed to bind vectis server");
    println!("vectis-server listening on http://{addr}");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .expect("server failed");
}

async fn index() -> &'static str {
    "vectis-server: connect WebSocket clients at /ws"
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<ServerState>,
    Query(params): Query<WsParams>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state, params.room))
}

async fn handle_socket(socket: WebSocket, state: ServerState, room_hint: Option<String>) {
    let (mut socket_tx, mut socket_rx) = socket.split();
    let (client_tx, mut client_rx) = mpsc::unbounded_channel::<Vec<u8>>();

    let writer = tokio::spawn(async move {
        while let Some(frame) = client_rx.recv().await {
            if socket_tx
                .send(WsMessage::Binary(frame.into()))
                .await
                .is_err()
            {
                break;
            }
        }
    });

    let hello = match socket_rx.next().await {
        Some(Ok(WsMessage::Binary(frame))) => decode_message(frame.as_ref()),
        Some(Ok(_)) => Err(vectis_protocol::ProtocolError::UnknownTag(0)),
        Some(Err(_)) | None => return,
    };

    let (room_name, resume_token, state_vector) = match hello {
        Ok(ProtocolMessage::ClientHello {
            room,
            resume_token,
            state_vector,
        }) => {
            let room = if room.is_empty() {
                room_hint.unwrap_or_else(|| "demo".to_string())
            } else {
                room
            };
            (room, resume_token, state_vector)
        }
        Ok(_) => {
            send_error(&client_tx, "first frame must be ClientHello");
            return;
        }
        Err(err) => {
            send_error(&client_tx, &format!("invalid hello: {err:?}"));
            return;
        }
    };

    let (actor, color, resume_token, sync_message) = join_room(
        &state,
        &room_name,
        resume_token,
        client_tx.clone(),
        decode_vector_clock(&state_vector),
    )
    .await;

    let _ = client_tx.send(encode_message(&ProtocolMessage::ServerWelcome {
        actor,
        color,
        resume_token,
    }));
    let _ = client_tx.send(encode_message(&sync_message));

    while let Some(message) = socket_rx.next().await {
        match message {
            Ok(WsMessage::Binary(frame)) => {
                process_client_frame(&state, &room_name, actor, frame.to_vec(), &client_tx).await;
            }
            Ok(WsMessage::Close(_)) => break,
            Ok(WsMessage::Text(_) | WsMessage::Ping(_) | WsMessage::Pong(_)) => {}
            Err(_) => break,
        }
    }

    leave_room(&state, &room_name, actor).await;
    writer.abort();
}

async fn join_room(
    state: &ServerState,
    room_name: &str,
    resume_token: String,
    sender: mpsc::UnboundedSender<Vec<u8>>,
    version: VectorClock,
) -> (ActorId, u32, String, ProtocolMessage) {
    let mut rooms = state.rooms.lock().await;
    let room = rooms
        .entry(room_name.to_string())
        .or_insert_with(RoomState::new);

    let (actor, resume_token) = room.resolve_session(resume_token);
    let color = actor_color(actor);
    let sync_message = room.sync_message(&version);
    room.clients.insert(actor, ClientState { sender, version });
    println!(
        "room={room_name} actor={} joined clients={}",
        actor.0,
        room.clients.len()
    );
    (actor, color, resume_token, sync_message)
}

async fn process_client_frame(
    state: &ServerState,
    room_name: &str,
    actor: ActorId,
    frame: Vec<u8>,
    sender: &mpsc::UnboundedSender<Vec<u8>>,
) {
    let message = match decode_message(&frame) {
        Ok(message) => message,
        Err(err) => {
            send_error(sender, &format!("protocol decode failed: {err:?}"));
            return;
        }
    };

    match message {
        ProtocolMessage::Update { bytes } => {
            let ops = match decode_update(&bytes) {
                Ok(ops) => ops,
                Err(err) => {
                    send_error(sender, &format!("update decode failed: {err}"));
                    return;
                }
            };
            if let Some(bad_actor) = ops
                .iter()
                .map(|op| op.id().actor)
                .find(|op_actor| *op_actor != actor)
            {
                send_error(
                    sender,
                    &format!(
                        "rejected update from actor {} containing op from actor {}",
                        actor.0, bad_actor.0
                    ),
                );
                return;
            }

            let recipients = {
                let mut rooms = state.rooms.lock().await;
                let Some(room) = rooms.get_mut(room_name) else {
                    send_error(sender, "room no longer exists");
                    return;
                };

                for op in ops.clone() {
                    if let Err(err) = room.doc.apply_remote_buffered(op, &mut room.buffer) {
                        send_error(sender, &format!("server apply failed: {err}"));
                        return;
                    }
                }
                if let Some(client) = room.clients.get_mut(&actor) {
                    for op in &ops {
                        let op_id = op.id();
                        client.version.advance(op_id.actor, op_id.lamport.0);
                    }
                }
                room.op_log.extend(ops);

                let update_recipients = room
                    .clients
                    .iter()
                    .filter_map(|(client_actor, client)| {
                        if *client_actor == actor {
                            None
                        } else {
                            Some(client.sender.clone())
                        }
                    })
                    .collect::<Vec<_>>();

                let mvv_frame = room.compute_mvv_frame();
                let mvv_recipients = mvv_frame
                    .as_ref()
                    .map(|frame| {
                        room.clients
                            .values()
                            .map(|client| (client.sender.clone(), frame.clone()))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();

                (update_recipients, mvv_recipients)
            };

            for recipient in recipients.0 {
                let _ = recipient.send(frame.clone());
            }
            for (recipient, mvv_frame) in recipients.1 {
                let _ = recipient.send(mvv_frame);
            }
        }
        ProtocolMessage::StateVector { bytes } => {
            let version = decode_vector_clock(&bytes);
            let mvv_recipients = {
                let mut rooms = state.rooms.lock().await;
                let Some(room) = rooms.get_mut(room_name) else {
                    send_error(sender, "room no longer exists");
                    return;
                };
                if let Some(client) = room.clients.get_mut(&actor) {
                    client.version = version;
                }
                room.compute_mvv_frame()
                    .map(|frame| {
                        room.clients
                            .values()
                            .map(|client| (client.sender.clone(), frame.clone()))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default()
            };
            for (recipient, mvv_frame) in mvv_recipients {
                let _ = recipient.send(mvv_frame);
            }
        }
        ProtocolMessage::Awareness { bytes } => {
            let Some(cursor) = decode_cursor(&bytes) else {
                send_error(sender, "malformed awareness frame");
                return;
            };
            if cursor.actor != actor {
                send_error(sender, "awareness actor does not match session actor");
                return;
            }
            let recipients = {
                let rooms = state.rooms.lock().await;
                let Some(room) = rooms.get(room_name) else {
                    send_error(sender, "room no longer exists");
                    return;
                };
                room.clients
                    .iter()
                    .filter_map(|(client_actor, client)| {
                        if *client_actor == actor {
                            None
                        } else {
                            Some(client.sender.clone())
                        }
                    })
                    .collect::<Vec<_>>()
            };
            for recipient in recipients {
                let _ = recipient.send(frame.clone());
            }
        }
        _ => send_error(sender, "unexpected client frame"),
    }
}

async fn leave_room(state: &ServerState, room_name: &str, actor: ActorId) {
    let mut rooms = state.rooms.lock().await;
    if let Some(room) = rooms.get_mut(room_name) {
        room.clients.remove(&actor);
        println!(
            "room={room_name} actor={} left clients={}",
            actor.0,
            room.clients.len()
        );
    }
}

fn send_error(sender: &mpsc::UnboundedSender<Vec<u8>>, message: &str) {
    let _ = sender.send(encode_message(&ProtocolMessage::Error {
        message: message.to_string(),
    }));
}

fn actor_color(actor: ActorId) -> u32 {
    const COLORS: [u32; 8] = [
        0xa78bfaff, 0x60a5fbff, 0x34d399ff, 0xfbbf24ff, 0xf87171ff, 0x22d3eeff, 0xfb7185ff,
        0xc084fcff,
    ];
    COLORS[(actor.0 as usize - 1) % COLORS.len()]
}

impl RoomState {
    fn new() -> Self {
        Self {
            doc: Document::new(ActorId(0)),
            buffer: CausalBuffer::new(),
            op_log: Vec::new(),
            op_log_base: VectorClock::new(),
            clients: HashMap::new(),
            sessions: HashMap::new(),
            next_actor: 1,
            next_token: 1,
        }
    }

    fn resolve_session(&mut self, requested_token: String) -> (ActorId, String) {
        if let Some(&actor) = self.sessions.get(&requested_token)
            && !requested_token.is_empty()
            && !self.clients.contains_key(&actor)
        {
            return (actor, requested_token);
        }

        let actor = ActorId(self.next_actor);
        self.next_actor += 1;
        let token = format!("r{}-s{}", actor.0, self.next_token);
        self.next_token += 1;
        self.sessions.insert(token.clone(), actor);
        (actor, token)
    }

    fn sync_message(&self, client_version: &VectorClock) -> ProtocolMessage {
        if client_version.dominates(&self.op_log_base) {
            let missing = self
                .op_log
                .iter()
                .filter(|op| client_version.get(op.id().actor) < op.id().lamport.0)
                .cloned()
                .collect::<Vec<_>>();
            ProtocolMessage::Update {
                bytes: encode_update(&missing),
            }
        } else {
            ProtocolMessage::Snapshot {
                bytes: encode_snapshot(&self.doc),
            }
        }
    }

    fn compute_mvv_frame(&mut self) -> Option<Vec<u8>> {
        let mut versions = self.clients.values().map(|client| client.version.clone());
        let mut mvv = versions.next()?;
        for version in versions {
            let actors = mvv
                .iter()
                .chain(version.iter())
                .map(|(actor, _)| actor)
                .collect::<std::collections::BTreeSet<_>>();
            let mut next = VectorClock::new();
            for actor in actors {
                next.advance(actor, mvv.get(actor).min(version.get(actor)));
            }
            mvv = next;
        }

        if let Some(result) = self
            .doc
            .update_min_version(mvv.clone(), &GcConfig::default())
            && result.tombstones_removed > 0
        {
            self.op_log.clear();
            self.op_log_base = self.doc.version.clone();
            println!(
                "server gc removed {} tombstones generation={}",
                result.tombstones_removed, result.generation
            );
        }

        Some(encode_message(&ProtocolMessage::Mvv {
            bytes: encode_state_vector(&mvv),
        }))
    }
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use vectis_crdt::stroke::{StrokeData, StrokePoint, StrokeProperties, ToolKind};
    use vectis_crdt::types::OpId;

    #[test]
    fn resume_token_reuses_inactive_actor() {
        let mut room = RoomState::new();
        let (actor, token) = room.resolve_session(String::new());
        assert_eq!(actor, ActorId(1));

        let (resumed_actor, resumed_token) = room.resolve_session(token.clone());
        assert_eq!(resumed_actor, actor);
        assert_eq!(resumed_token, token);
    }

    #[test]
    fn resume_token_does_not_collide_with_active_actor() {
        let mut room = RoomState::new();
        let (actor, token) = room.resolve_session(String::new());
        let (sender, _receiver) = mpsc::unbounded_channel();
        room.clients.insert(
            actor,
            ClientState {
                sender,
                version: VectorClock::new(),
            },
        );

        let (new_actor, new_token) = room.resolve_session(token);
        assert_ne!(new_actor, actor);
        assert_ne!(new_token, String::new());
    }

    #[test]
    fn sync_message_uses_op_log_delta_when_available() {
        let mut source = Document::new(ActorId(9));
        source.insert_stroke(
            StrokeData::new(vec![StrokePoint::basic(0.0, 0.0)].into(), ToolKind::Pen),
            StrokeProperties::new(0xa78bfaff, 3.0, 1.0, OpId::ZERO),
        );
        let ops = source.take_pending_ops();

        let mut room = RoomState::new();
        for op in ops.clone() {
            room.doc.apply_remote(op.clone());
            room.op_log.push(op);
        }

        let message = room.sync_message(&VectorClock::new());
        let ProtocolMessage::Update { bytes } = message else {
            panic!("expected update delta");
        };
        assert_eq!(decode_update(&bytes).unwrap().len(), ops.len());

        let mut caught_up = VectorClock::new();
        caught_up.advance(ActorId(9), ops[0].id().lamport.0);
        let message = room.sync_message(&caught_up);
        let ProtocolMessage::Update { bytes } = message else {
            panic!("expected empty update delta");
        };
        assert!(decode_update(&bytes).unwrap().is_empty());
    }
}
