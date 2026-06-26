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
use vectis_crdt::causal_buffer::CausalBuffer;
use vectis_crdt::document::Document;
use vectis_crdt::encoding::{decode_update, decode_vector_clock, encode_snapshot};
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
    clients: HashMap<ActorId, ClientState>,
    next_actor: u64,
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

    let (room_name, state_vector) = match hello {
        Ok(ProtocolMessage::ClientHello { room, state_vector }) => {
            let room = if room.is_empty() {
                room_hint.unwrap_or_else(|| "demo".to_string())
            } else {
                room
            };
            (room, state_vector)
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

    let (actor, color, snapshot) = join_room(
        &state,
        &room_name,
        client_tx.clone(),
        decode_vector_clock(&state_vector),
    )
    .await;

    let _ = client_tx.send(encode_message(&ProtocolMessage::ServerWelcome {
        actor,
        color,
    }));
    let _ = client_tx.send(encode_message(&ProtocolMessage::Snapshot {
        bytes: snapshot,
    }));

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
    sender: mpsc::UnboundedSender<Vec<u8>>,
    version: VectorClock,
) -> (ActorId, u32, Vec<u8>) {
    let mut rooms = state.rooms.lock().await;
    let room = rooms
        .entry(room_name.to_string())
        .or_insert_with(RoomState::new);

    let actor = ActorId(room.next_actor);
    room.next_actor += 1;
    let color = actor_color(actor);
    let snapshot = encode_snapshot(&room.doc);
    room.clients.insert(actor, ClientState { sender, version });
    println!(
        "room={room_name} actor={} joined clients={}",
        actor.0,
        room.clients.len()
    );
    (actor, color, snapshot)
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
                    client.version = room.doc.version.clone();
                }

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
        ProtocolMessage::StateVector { bytes } => {
            let version = decode_vector_clock(&bytes);
            let mut rooms = state.rooms.lock().await;
            if let Some(room) = rooms.get_mut(room_name)
                && let Some(client) = room.clients.get_mut(&actor)
            {
                client.version = version;
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
            clients: HashMap::new(),
            next_actor: 1,
        }
    }
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}
