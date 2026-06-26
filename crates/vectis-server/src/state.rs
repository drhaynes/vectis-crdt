use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{Mutex, mpsc};
use vectis_crdt::awareness::decode_cursor;
use vectis_crdt::encoding::{decode_update, decode_vector_clock};
use vectis_crdt::types::{ActorId, VectorClock};
use vectis_protocol::{ProtocolMessage, decode_message};

use crate::color::actor_color;
use crate::connection::send_error;
use crate::room::{RoomState, SenderFrame, UpdateRecipients};

type Rooms = Arc<Mutex<HashMap<String, RoomState>>>;

#[derive(Clone)]
pub(crate) struct ServerState {
    rooms: Rooms,
}

impl ServerState {
    pub(crate) fn new() -> Self {
        Self {
            rooms: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub(crate) async fn join_room(
        &self,
        room_name: &str,
        resume_token: String,
        sender: mpsc::UnboundedSender<Vec<u8>>,
        version: VectorClock,
    ) -> (ActorId, u32, String, ProtocolMessage) {
        let mut rooms = self.rooms.lock().await;
        let room = rooms
            .entry(room_name.to_string())
            .or_insert_with(RoomState::new);

        let (actor, resume_token) = room.resolve_session(resume_token);
        let color = actor_color(actor);
        let sync_message = room.sync_message(&version);
        room.insert_client(actor, sender, version);
        println!(
            "room={room_name} actor={} joined clients={}",
            actor.0,
            room.client_count()
        );
        (actor, color, resume_token, sync_message)
    }

    pub(crate) async fn process_client_frame(
        &self,
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
                    let mut rooms = self.rooms.lock().await;
                    let Some(room) = rooms.get_mut(room_name) else {
                        send_error(sender, "room no longer exists");
                        return;
                    };
                    match room.apply_update(actor, ops) {
                        Ok(recipients) => recipients,
                        Err(err) => {
                            send_error(sender, &err);
                            return;
                        }
                    }
                };

                send_update_recipients(recipients, &frame);
            }
            ProtocolMessage::StateVector { bytes } => {
                let version = decode_vector_clock(&bytes);
                let mvv_recipients = {
                    let mut rooms = self.rooms.lock().await;
                    let Some(room) = rooms.get_mut(room_name) else {
                        send_error(sender, "room no longer exists");
                        return;
                    };
                    room.update_client_version(actor, version)
                };
                send_frames(mvv_recipients);
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
                    let rooms = self.rooms.lock().await;
                    let Some(room) = rooms.get(room_name) else {
                        send_error(sender, "room no longer exists");
                        return;
                    };
                    room.recipients_except(actor)
                };

                for recipient in recipients {
                    let _ = recipient.send(frame.clone());
                }
            }
            _ => send_error(sender, "unexpected client frame"),
        }
    }

    pub(crate) async fn leave_room(&self, room_name: &str, actor: ActorId) {
        let mut rooms = self.rooms.lock().await;
        if let Some(room) = rooms.get_mut(room_name) {
            room.remove_client(actor);
            println!(
                "room={room_name} actor={} left clients={}",
                actor.0,
                room.client_count()
            );
        }
    }
}

fn send_update_recipients(recipients: UpdateRecipients, update_frame: &[u8]) {
    for recipient in recipients.update {
        let _ = recipient.send(update_frame.to_vec());
    }
    send_frames(recipients.mvv);
}

fn send_frames(frames: Vec<SenderFrame>) {
    for (recipient, frame) in frames {
        let _ = recipient.send(frame);
    }
}
