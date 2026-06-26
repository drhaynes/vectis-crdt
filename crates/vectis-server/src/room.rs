use std::collections::HashMap;

use tokio::sync::mpsc;
use vectis_crdt::causal_buffer::CausalBuffer;
use vectis_crdt::document::{Document, Operation};
use vectis_crdt::encoding::{encode_snapshot, encode_state_vector, encode_update};
use vectis_crdt::gc::GcConfig;
use vectis_crdt::types::{ActorId, VectorClock};
use vectis_protocol::{ProtocolMessage, encode_message};

pub(crate) type Sender = mpsc::UnboundedSender<Vec<u8>>;
pub(crate) type SenderFrame = (Sender, Vec<u8>);

pub(crate) struct UpdateRecipients {
    pub(crate) update: Vec<Sender>,
    pub(crate) mvv: Vec<SenderFrame>,
}

pub(crate) struct RoomState {
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
    sender: Sender,
    version: VectorClock,
}

impl RoomState {
    pub(crate) fn new() -> Self {
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

    pub(crate) fn client_count(&self) -> usize {
        self.clients.len()
    }

    pub(crate) fn insert_client(&mut self, actor: ActorId, sender: Sender, version: VectorClock) {
        self.clients.insert(actor, ClientState { sender, version });
    }

    pub(crate) fn remove_client(&mut self, actor: ActorId) {
        self.clients.remove(&actor);
    }

    pub(crate) fn recipients_except(&self, actor: ActorId) -> Vec<Sender> {
        self.clients
            .iter()
            .filter_map(|(client_actor, client)| {
                if *client_actor == actor {
                    None
                } else {
                    Some(client.sender.clone())
                }
            })
            .collect()
    }

    pub(crate) fn resolve_session(&mut self, requested_token: String) -> (ActorId, String) {
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

    pub(crate) fn sync_message(&self, client_version: &VectorClock) -> ProtocolMessage {
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

    pub(crate) fn apply_update(
        &mut self,
        actor: ActorId,
        ops: Vec<Operation>,
    ) -> Result<UpdateRecipients, String> {
        for op in ops.clone() {
            if let Err(err) = self.doc.apply_remote_buffered(op, &mut self.buffer) {
                return Err(format!("server apply failed: {err}"));
            }
        }

        if let Some(client) = self.clients.get_mut(&actor) {
            for op in &ops {
                let op_id = op.id();
                client.version.advance(op_id.actor, op_id.lamport.0);
            }
        }
        self.op_log.extend(ops);

        let update = self.recipients_except(actor);
        let mvv = self.mvv_recipients();
        Ok(UpdateRecipients { update, mvv })
    }

    pub(crate) fn update_client_version(
        &mut self,
        actor: ActorId,
        version: VectorClock,
    ) -> Vec<SenderFrame> {
        if let Some(client) = self.clients.get_mut(&actor) {
            client.version = version;
        }
        self.mvv_recipients()
    }

    fn mvv_recipients(&mut self) -> Vec<SenderFrame> {
        self.compute_mvv_frame()
            .map(|frame| {
                self.clients
                    .values()
                    .map(|client| (client.sender.clone(), frame.clone()))
                    .collect()
            })
            .unwrap_or_default()
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

#[cfg(test)]
mod tests {
    use super::*;
    use vectis_crdt::encoding::decode_update;
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
        room.insert_client(actor, sender, VectorClock::new());

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
