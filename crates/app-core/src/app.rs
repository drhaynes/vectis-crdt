use vectis_crdt::awareness::{AwarenessStore, CursorState, encode_cursor};
use vectis_crdt::causal_buffer::CausalBuffer;
use vectis_crdt::document::Document;
use vectis_crdt::encoding::{
    decode_snapshot, decode_update, decode_vector_clock, encode_state_vector, encode_update,
};
use vectis_crdt::gc::GcConfig;
use vectis_crdt::stroke::{StrokeData, StrokeProperties, ToolKind};
use vectis_crdt::types::{ActorId, OpId};
use vectis_protocol::{ProtocolMessage, decode_message, encode_message};

use crate::event::ClientEvent;
use crate::network::{ConnectionState, Direction, MAX_LOG, WireEntry, hex_prefix};
use crate::stroke::{AppPoint, LiveStroke, app_point_from_stroke, stroke_point_from_app};
use crate::view::{AppStats, CursorView, StrokeView};

const DEFAULT_COLOR: u32 = 0xa78bfaff;

pub struct ClientApp {
    room: String,
    resume_token: String,
    actor: Option<ActorId>,
    color: u32,
    doc: Document,
    buffer: CausalBuffer,
    awareness: AwarenessStore,
    live: Option<LiveStroke>,
    connected: bool,
    loaded: bool,
    frames_sent: u32,
    frames_received: u32,
    bytes_sent: usize,
    bytes_received: usize,
    wire_log: Vec<WireEntry>,
    status: String,
}

impl ClientApp {
    pub fn new(room: impl Into<String>) -> Self {
        Self {
            room: room.into(),
            resume_token: String::new(),
            actor: None,
            color: DEFAULT_COLOR,
            doc: Document::new(ActorId(0)),
            buffer: CausalBuffer::new(),
            awareness: AwarenessStore::new(),
            live: None,
            connected: false,
            loaded: false,
            frames_sent: 0,
            frames_received: 0,
            bytes_sent: 0,
            bytes_received: 0,
            wire_log: Vec::new(),
            status: "connecting".to_string(),
        }
    }

    pub fn room(&self) -> &str {
        &self.room
    }

    pub fn color(&self) -> u32 {
        self.color
    }

    pub fn resume_token(&self) -> &str {
        &self.resume_token
    }

    pub fn set_resume_token(&mut self, token: impl Into<String>) {
        self.resume_token = token.into();
    }

    pub fn set_connected(&mut self, connected: bool) {
        self.connected = connected;
        if connected {
            self.status = "connected".to_string();
        } else {
            self.loaded = false;
            self.live = None;
            self.status = "disconnected".to_string();
        }
    }

    pub fn hello_frame(&mut self) -> Vec<ClientEvent> {
        let state_vector = encode_state_vector(&self.doc.version);
        let frame = encode_message(&ProtocolMessage::ClientHello {
            room: self.room.clone(),
            resume_token: self.resume_token.clone(),
            state_vector,
        });
        self.record(Direction::Outbound, "hello", &frame);
        vec![ClientEvent::SendFrame(frame)]
    }

    pub fn receive_frame(&mut self, frame: &[u8]) -> Vec<ClientEvent> {
        self.record(Direction::Inbound, "frame", frame);

        let mut events = Vec::new();
        match decode_message(frame) {
            Ok(ProtocolMessage::ServerWelcome {
                actor,
                color,
                resume_token,
            }) => {
                let actor_changed = self.actor != Some(actor);
                self.actor = Some(actor);
                self.color = color;
                self.resume_token = resume_token;
                if actor_changed {
                    self.doc = Document::new(actor);
                    self.buffer = CausalBuffer::new();
                    self.loaded = false;
                }
                self.status = format!("actor {} assigned", actor.0);
            }
            Ok(ProtocolMessage::Snapshot { bytes }) => {
                let Some(actor) = self.actor else {
                    self.status = "snapshot arrived before welcome".to_string();
                    return events;
                };
                match decode_snapshot(&bytes, actor) {
                    Ok(doc) => {
                        self.doc = doc;
                        self.buffer = CausalBuffer::new();
                        self.loaded = true;
                        self.status = "ready".to_string();
                        events.extend(self.state_vector_frame());
                    }
                    Err(err) => {
                        self.status = format!("snapshot decode failed: {err}");
                    }
                }
            }
            Ok(ProtocolMessage::Update { bytes }) => match decode_update(&bytes) {
                Ok(ops) => {
                    for op in ops {
                        if let Err(err) = self.doc.apply_remote_buffered(op, &mut self.buffer) {
                            self.status = format!("update apply failed: {err}");
                            return events;
                        }
                    }
                    if !self.loaded {
                        self.loaded = true;
                        self.status = "ready".to_string();
                    } else {
                        self.status = "synced".to_string();
                    }
                    events.extend(self.state_vector_frame());
                }
                Err(err) => {
                    self.status = format!("update decode failed: {err}");
                }
            },
            Ok(ProtocolMessage::Error { message }) => {
                self.status = message;
            }
            Ok(ProtocolMessage::Mvv { bytes }) => {
                let mvv = decode_vector_clock(&bytes);
                if let Some(result) = self.doc.update_min_version(mvv, &GcConfig::default()) {
                    if result.tombstones_removed > 0 {
                        self.status =
                            format!("gc removed {} tombstones", result.tombstones_removed);
                    }
                }
            }
            Ok(ProtocolMessage::Awareness { bytes }) => {
                self.awareness.apply_bulk(&bytes);
            }
            Ok(ProtocolMessage::ClientHello { .. } | ProtocolMessage::StateVector { .. }) => {
                self.status = "unexpected server frame".to_string();
            }
            Err(err) => {
                self.status = format!("protocol decode failed: {err:?}");
            }
        }
        events
    }

    pub fn begin_stroke(&mut self, point: AppPoint) {
        if self.can_edit() {
            self.live = Some(LiveStroke::new(self.color, point));
        }
    }

    pub fn extend_stroke(&mut self, point: AppPoint) {
        if let Some(stroke) = &mut self.live {
            stroke.points.push(point);
        }
    }

    pub fn end_stroke(&mut self) -> Vec<ClientEvent> {
        let stroke = self.live.take();
        if let Some(stroke) = stroke
            && stroke.points.len() >= 2
        {
            return self.commit(stroke);
        }
        Vec::new()
    }

    pub fn cancel_stroke(&mut self) {
        self.live = None;
    }

    pub fn awareness_frame(&mut self, point: AppPoint, now_ms: u64) -> Vec<ClientEvent> {
        let Some(actor) = self.actor else {
            return Vec::new();
        };
        if !self.connected || !self.loaded {
            return Vec::new();
        }

        let cursor = CursorState::new(actor, point.x, point.y, now_ms, self.color);
        let bytes = encode_cursor(&cursor).to_vec();
        let frame = encode_message(&ProtocolMessage::Awareness { bytes });
        self.record(Direction::Outbound, "cursor", &frame);
        vec![ClientEvent::SendFrame(frame)]
    }

    pub fn undo(&mut self) -> Vec<ClientEvent> {
        if self.can_edit() && self.doc.undo_last_stroke().is_some() {
            self.flush()
        } else {
            Vec::new()
        }
    }

    pub fn strokes(&self) -> Vec<StrokeView> {
        self.doc
            .visible_stroke_ids()
            .into_iter()
            .filter_map(|id| {
                let (data, props) = self.doc.get_stroke(&id)?;
                Some(StrokeView {
                    color: props.color.value,
                    width: props.stroke_width.value,
                    opacity: props.opacity.value,
                    points: data.points.iter().map(app_point_from_stroke).collect(),
                })
            })
            .collect()
    }

    pub fn live_stroke(&self) -> Option<StrokeView> {
        self.live.as_ref().map(|stroke| StrokeView {
            color: stroke.color,
            width: 3.0,
            opacity: 0.65,
            points: stroke.points.clone(),
        })
    }

    pub fn cursors(&self) -> Vec<CursorView> {
        let local_actor = self.actor;
        self.awareness
            .all()
            .filter(|cursor| Some(cursor.actor) != local_actor)
            .map(|cursor| CursorView {
                actor: cursor.actor.0,
                color: cursor.color,
                point: AppPoint::new(cursor.x, cursor.y, 1.0),
            })
            .collect()
    }

    pub fn stats(&self) -> AppStats {
        AppStats {
            actor: self.actor.map(|actor| actor.0),
            resume_token: short_token(&self.resume_token),
            visible_strokes: self.doc.visible_stroke_ids().len(),
            undo_depth: self.doc.undo_depth(),
            remote_cursors: self.cursors().len(),
            gc_generation: self.doc.stats().gc_generation,
            frames_sent: self.frames_sent,
            frames_received: self.frames_received,
            bytes_sent: self.bytes_sent,
            bytes_received: self.bytes_received,
            status: self.status.clone(),
        }
    }

    pub fn connection_state(&self) -> ConnectionState {
        ConnectionState {
            connected: self.connected,
            loaded: self.loaded,
        }
    }

    pub fn wire_log(&self) -> &[WireEntry] {
        &self.wire_log
    }

    fn can_edit(&self) -> bool {
        self.connected && self.loaded && self.actor.is_some()
    }

    fn commit(&mut self, stroke: LiveStroke) -> Vec<ClientEvent> {
        if !self.can_edit() {
            return Vec::new();
        }
        let points = stroke
            .points
            .iter()
            .map(stroke_point_from_app)
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let data = StrokeData::new(points, ToolKind::Pen);
        let props = StrokeProperties::new(stroke.color, 3.0, 1.0, OpId::ZERO);
        self.doc.insert_stroke(data, props);
        self.flush()
    }

    fn flush(&mut self) -> Vec<ClientEvent> {
        let ops = self.doc.take_pending_ops();
        if ops.is_empty() {
            return Vec::new();
        }
        let update = encode_update(&ops);
        let frame = encode_message(&ProtocolMessage::Update { bytes: update });
        self.record(Direction::Outbound, "update", &frame);
        vec![ClientEvent::SendFrame(frame)]
    }

    fn state_vector_frame(&mut self) -> Vec<ClientEvent> {
        let bytes = encode_state_vector(&self.doc.version);
        let frame = encode_message(&ProtocolMessage::StateVector { bytes });
        self.record(Direction::Outbound, "state", &frame);
        vec![ClientEvent::SendFrame(frame)]
    }

    fn record(&mut self, direction: Direction, kind: &'static str, frame: &[u8]) {
        match direction {
            Direction::Outbound => {
                self.frames_sent += 1;
                self.bytes_sent += frame.len();
            }
            Direction::Inbound => {
                self.frames_received += 1;
                self.bytes_received += frame.len();
            }
        }

        self.wire_log.insert(
            0,
            WireEntry {
                direction,
                kind,
                bytes: frame.len(),
                hex: hex_prefix(frame),
            },
        );
        if self.wire_log.len() > MAX_LOG {
            self.wire_log.pop();
        }
    }
}

fn short_token(token: &str) -> String {
    if token.is_empty() {
        "none".to_string()
    } else {
        token.chars().take(12).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vectis_crdt::encoding::encode_snapshot;

    fn welcome(actor: u64) -> Vec<u8> {
        encode_message(&ProtocolMessage::ServerWelcome {
            actor: ActorId(actor),
            color: 0x60a5fbff,
            resume_token: format!("token-{actor}"),
        })
    }

    #[test]
    fn hello_uses_configured_room() {
        let mut app = ClientApp::new("demo-room");
        let events = app.hello_frame();
        let [ClientEvent::SendFrame(frame)] = events.as_slice() else {
            panic!("expected hello frame");
        };
        assert_eq!(
            decode_message(frame),
            Ok(ProtocolMessage::ClientHello {
                room: "demo-room".to_string(),
                resume_token: String::new(),
                state_vector: encode_state_vector(&Document::new(ActorId(0)).version),
            })
        );
    }

    #[test]
    fn snapshot_makes_client_ready() {
        let mut app = ClientApp::new("demo");
        app.set_connected(true);
        app.receive_frame(&welcome(7));
        let snapshot = encode_snapshot(&Document::new(ActorId(999)));
        app.receive_frame(&encode_message(&ProtocolMessage::Snapshot {
            bytes: snapshot,
        }));
        assert!(app.connection_state().loaded);
        assert_eq!(app.stats().actor, Some(7));
        assert_eq!(app.resume_token(), "token-7");
    }

    #[test]
    fn drawing_emits_update_frame() {
        let mut app = ClientApp::new("demo");
        app.set_connected(true);
        app.receive_frame(&welcome(7));
        let snapshot = encode_snapshot(&Document::new(ActorId(999)));
        app.receive_frame(&encode_message(&ProtocolMessage::Snapshot {
            bytes: snapshot,
        }));

        app.begin_stroke(AppPoint::new(0.0, 0.0, 1.0));
        app.extend_stroke(AppPoint::new(1.0, 1.0, 1.0));
        let events = app.end_stroke();
        assert!(matches!(events.as_slice(), [ClientEvent::SendFrame(_)]));
        assert_eq!(app.stats().visible_strokes, 1);
    }

    #[test]
    fn awareness_emits_cursor_frame() {
        let mut app = ClientApp::new("demo");
        app.set_connected(true);
        app.receive_frame(&welcome(7));
        let snapshot = encode_snapshot(&Document::new(ActorId(999)));
        app.receive_frame(&encode_message(&ProtocolMessage::Snapshot {
            bytes: snapshot,
        }));

        let events = app.awareness_frame(AppPoint::new(3.0, 4.0, 1.0), 12);
        assert!(matches!(events.as_slice(), [ClientEvent::SendFrame(_)]));
    }
}
