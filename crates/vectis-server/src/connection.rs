use axum::extract::ws::{Message as WsMessage, WebSocket};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use vectis_crdt::encoding::decode_vector_clock;
use vectis_protocol::{ProtocolError, ProtocolMessage, decode_message, encode_message};

use crate::state::ServerState;

pub(crate) async fn handle_socket(
    socket: WebSocket,
    state: ServerState,
    room_hint: Option<String>,
) {
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
        Some(Ok(_)) => Err(ProtocolError::UnknownTag(0)),
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

    let (actor, color, resume_token, sync_message) = state
        .join_room(
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
                state
                    .process_client_frame(&room_name, actor, frame.to_vec(), &client_tx)
                    .await;
            }
            Ok(WsMessage::Close(_)) => break,
            Ok(WsMessage::Text(_) | WsMessage::Ping(_) | WsMessage::Pong(_)) => {}
            Err(_) => break,
        }
    }

    state.leave_room(&room_name, actor).await;
    writer.abort();
}

pub(crate) fn send_error(sender: &mpsc::UnboundedSender<Vec<u8>>, message: &str) {
    let _ = sender.send(encode_message(&ProtocolMessage::Error {
        message: message.to_string(),
    }));
}
