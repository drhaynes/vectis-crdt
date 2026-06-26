use vectis_crdt::causal_buffer::CausalBuffer;
use vectis_crdt::document::Document;
use vectis_crdt::encoding::{decode_update, encode_update};
use vectis_crdt::stroke::{StrokeData, StrokeProperties, ToolKind};
use vectis_crdt::types::{ActorId, OpId};

use crate::event::AppEvent;
use crate::network::{
    Direction, InflightPacket, MAX_LOG, NetworkState, PacketStatus, PacketView, QueuedPacket,
    WireEntry, hex_prefix, packet_opacity, packet_progress,
};
use crate::peer::{ALICE_ACTOR, BOB_ACTOR, Peer};
use crate::stroke::{AppPoint, LiveStroke, app_point_from_stroke, stroke_point_from_app};
use crate::view::{AppStats, StrokeView};

struct PeerDoc {
    doc: Document,
    buffer: CausalBuffer,
}

impl PeerDoc {
    fn new(actor_id: u64) -> Self {
        Self {
            doc: Document::new(ActorId(actor_id)),
            buffer: CausalBuffer::new(),
        }
    }
}

pub struct DemoApp {
    alice: PeerDoc,
    bob: PeerDoc,
    network_delay: u32,
    disconnected: bool,
    queued: Vec<QueuedPacket>,
    inflight: Vec<InflightPacket>,
    next_packet_id: u32,
    total_packets: u32,
    total_bytes: usize,
    alice_bytes: usize,
    bob_bytes: usize,
    wire_log: Vec<WireEntry>,
    live_alice: Option<LiveStroke>,
    live_bob: Option<LiveStroke>,
}

impl Default for DemoApp {
    fn default() -> Self {
        Self::new()
    }
}

impl DemoApp {
    pub fn new() -> Self {
        Self {
            alice: PeerDoc::new(ALICE_ACTOR),
            bob: PeerDoc::new(BOB_ACTOR),
            network_delay: 500,
            disconnected: false,
            queued: Vec::new(),
            inflight: Vec::new(),
            next_packet_id: 0,
            total_packets: 0,
            total_bytes: 0,
            alice_bytes: 0,
            bob_bytes: 0,
            wire_log: Vec::new(),
            live_alice: None,
            live_bob: None,
        }
    }

    pub fn begin_stroke(&mut self, peer: Peer, point: AppPoint, color: u32) {
        *self.live_mut(peer) = Some(LiveStroke::new(color, point));
    }

    pub fn extend_stroke(&mut self, peer: Peer, point: AppPoint) {
        if let Some(stroke) = self.live_mut(peer) {
            stroke.points.push(point);
        }
    }

    pub fn end_stroke(&mut self, peer: Peer) -> Vec<AppEvent> {
        let stroke = self.live_mut(peer).take();
        if let Some(stroke) = stroke
            && stroke.points.len() >= 2
        {
            return self.commit(peer, stroke);
        }
        Vec::new()
    }

    pub fn cancel_stroke(&mut self, peer: Peer) {
        *self.live_mut(peer) = None;
    }

    pub fn set_network_delay(&mut self, delay_ms: u32) {
        self.network_delay = delay_ms;
    }

    pub fn toggle_disconnect(&mut self) {
        self.disconnected = !self.disconnected;
    }

    pub fn reconnect_and_sync(&mut self) {
        self.disconnected = false;
        self.sync_now();
    }

    pub fn undo(&mut self, peer: Peer) -> Vec<AppEvent> {
        if self.peer_mut(peer).doc.undo_last_stroke().is_some() {
            self.flush(peer)
        } else {
            Vec::new()
        }
    }

    pub fn clear_all(&mut self) -> Vec<AppEvent> {
        self.queued.clear();
        self.inflight.clear();
        self.total_packets = 0;
        self.total_bytes = 0;
        self.alice_bytes = 0;
        self.bob_bytes = 0;
        self.wire_log.clear();
        self.live_alice = None;
        self.live_bob = None;
        self.alice = PeerDoc::new(ALICE_ACTOR);
        self.bob = PeerDoc::new(BOB_ACTOR);
        vec![AppEvent::ClearPackets]
    }

    pub fn tick(&mut self, now_ms: f64) -> Vec<AppEvent> {
        let mut delivered = Vec::new();
        for (idx, packet) in self.inflight.iter().enumerate() {
            if packet_progress(packet, now_ms) >= 1.0 {
                delivered.push(idx);
            }
        }

        let mut events = Vec::new();
        for idx in delivered.into_iter().rev() {
            let packet = self.inflight.remove(idx);
            self.apply_to_target(packet.direction, &packet.payload);
            self.total_packets += 1;
            self.mark_log(packet.id, PacketStatus::Delivered);
            events.push(AppEvent::PacketDelivered { id: packet.id });
        }
        events
    }

    pub fn packet_views(&self, now_ms: f64) -> Vec<PacketView> {
        self.inflight
            .iter()
            .map(|packet| {
                let progress = packet_progress(packet, now_ms);
                PacketView {
                    id: packet.id,
                    direction: packet.direction,
                    bytes: packet.bytes,
                    progress,
                    opacity: packet_opacity(progress),
                }
            })
            .collect()
    }

    pub fn set_packet_start_time(&mut self, id: u32, start_time: f64) {
        if let Some(packet) = self.inflight.iter_mut().find(|packet| packet.id == id) {
            packet.start_time = start_time;
        }
    }

    pub fn strokes(&self, peer: Peer) -> Vec<StrokeView> {
        let peer_doc = self.peer(peer);
        peer_doc
            .doc
            .visible_stroke_ids()
            .into_iter()
            .filter_map(|id| {
                let (data, props) = peer_doc.doc.get_stroke(&id)?;
                Some(StrokeView {
                    color: props.color.value,
                    width: props.stroke_width.value,
                    opacity: props.opacity.value,
                    points: data.points.iter().map(app_point_from_stroke).collect(),
                })
            })
            .collect()
    }

    pub fn live_stroke(&self, peer: Peer) -> Option<StrokeView> {
        self.live(peer).as_ref().map(|stroke| StrokeView {
            color: stroke.color,
            width: 3.0,
            opacity: 0.65,
            points: stroke.points.clone(),
        })
    }

    pub fn stats(&self) -> AppStats {
        AppStats {
            alice_visible: self.alice.doc.visible_stroke_ids().len(),
            bob_visible: self.bob.doc.visible_stroke_ids().len(),
            alice_queued: self
                .queued
                .iter()
                .filter(|p| p.direction == Direction::AliceToBob)
                .count(),
            bob_queued: self
                .queued
                .iter()
                .filter(|p| p.direction == Direction::BobToAlice)
                .count(),
            alice_undo_depth: self.alice.doc.undo_depth(),
            bob_undo_depth: self.bob.doc.undo_depth(),
            total_packets: self.total_packets,
            total_bytes: self.total_bytes,
            alice_bytes: self.alice_bytes,
            bob_bytes: self.bob_bytes,
        }
    }

    pub fn network_state(&self) -> NetworkState {
        NetworkState {
            delay_ms: self.network_delay,
            disconnected: self.disconnected,
        }
    }

    pub fn wire_log(&self) -> &[WireEntry] {
        &self.wire_log
    }

    fn commit(&mut self, peer: Peer, stroke: LiveStroke) -> Vec<AppEvent> {
        let points = stroke
            .points
            .iter()
            .map(stroke_point_from_app)
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let data = StrokeData::new(points, ToolKind::Pen);
        let props = StrokeProperties::new(stroke.color, 3.0, 1.0, OpId::ZERO);
        self.peer_mut(peer).doc.insert_stroke(data, props);
        self.flush(peer)
    }

    fn flush(&mut self, peer: Peer) -> Vec<AppEvent> {
        let ops = self.peer_mut(peer).doc.take_pending_ops();
        if ops.is_empty() {
            return Vec::new();
        }
        self.send_packet(peer, encode_update(&ops))
    }

    fn send_packet(&mut self, from_peer: Peer, payload: Vec<u8>) -> Vec<AppEvent> {
        let direction = match from_peer {
            Peer::Alice => Direction::AliceToBob,
            Peer::Bob => Direction::BobToAlice,
        };
        let hex = hex_prefix(&payload);
        let bytes = payload.len();

        self.total_bytes += bytes;
        match from_peer {
            Peer::Alice => self.alice_bytes += bytes,
            Peer::Bob => self.bob_bytes += bytes,
        }

        if self.disconnected {
            self.queued.push(QueuedPacket { direction, payload });
            self.log_entry(direction, bytes, hex, PacketStatus::Queued, None);
            return Vec::new();
        }

        self.next_packet_id += 1;
        let id = self.next_packet_id;
        let duration = f64::from(self.network_delay.max(30));
        self.log_entry(direction, bytes, hex, PacketStatus::Inflight, Some(id));
        self.inflight.push(InflightPacket {
            id,
            start_time: 0.0,
            duration,
            direction,
            payload,
            bytes,
        });

        vec![AppEvent::PacketCreated {
            id,
            direction,
            bytes,
        }]
    }

    fn sync_now(&mut self) {
        let queued = std::mem::take(&mut self.queued);
        for packet in queued {
            self.apply_to_target(packet.direction, &packet.payload);
            self.total_packets += 1;
        }

        for entry in &mut self.wire_log {
            if entry.status == PacketStatus::Queued {
                entry.status = PacketStatus::Delivered;
            }
        }
    }

    fn apply_to_target(&mut self, direction: Direction, payload: &[u8]) {
        let peer = match direction {
            Direction::AliceToBob => &mut self.bob,
            Direction::BobToAlice => &mut self.alice,
        };

        if let Ok(ops) = decode_update(payload) {
            for op in ops {
                let _ = peer.doc.apply_remote_buffered(op, &mut peer.buffer);
            }
        }
    }

    fn log_entry(
        &mut self,
        direction: Direction,
        bytes: usize,
        hex: String,
        status: PacketStatus,
        id: Option<u32>,
    ) {
        self.wire_log.insert(
            0,
            WireEntry {
                id,
                direction,
                bytes,
                hex,
                status,
            },
        );
        if self.wire_log.len() > MAX_LOG {
            self.wire_log.pop();
        }
    }

    fn mark_log(&mut self, id: u32, status: PacketStatus) {
        if let Some(entry) = self.wire_log.iter_mut().find(|entry| entry.id == Some(id)) {
            entry.status = status;
        }
    }

    fn peer(&self, peer: Peer) -> &PeerDoc {
        match peer {
            Peer::Alice => &self.alice,
            Peer::Bob => &self.bob,
        }
    }

    fn peer_mut(&mut self, peer: Peer) -> &mut PeerDoc {
        match peer {
            Peer::Alice => &mut self.alice,
            Peer::Bob => &mut self.bob,
        }
    }

    fn live(&self, peer: Peer) -> &Option<LiveStroke> {
        match peer {
            Peer::Alice => &self.live_alice,
            Peer::Bob => &self.live_bob,
        }
    }

    fn live_mut(&mut self, peer: Peer) -> &mut Option<LiveStroke> {
        match peer {
            Peer::Alice => &mut self.live_alice,
            Peer::Bob => &mut self.live_bob,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const ALICE_COLOR: u32 = 0xa78bfaff;

    fn draw_stroke(app: &mut DemoApp, peer: Peer) -> Vec<AppEvent> {
        app.begin_stroke(peer, AppPoint::new(0.0, 0.0, 1.0), ALICE_COLOR);
        app.extend_stroke(peer, AppPoint::new(10.0, 10.0, 1.0));
        app.end_stroke(peer)
    }

    #[test]
    fn drawing_creates_inflight_packet() {
        let mut app = DemoApp::new();
        let events = draw_stroke(&mut app, Peer::Alice);
        assert!(matches!(
            events.as_slice(),
            [AppEvent::PacketCreated { .. }]
        ));
        assert_eq!(app.stats().alice_visible, 1);
        assert_eq!(app.packet_views(0.0).len(), 1);
    }

    #[test]
    fn disconnected_draw_queues_packet() {
        let mut app = DemoApp::new();
        app.toggle_disconnect();
        let events = draw_stroke(&mut app, Peer::Alice);
        assert!(events.is_empty());
        assert_eq!(app.stats().alice_queued, 1);
        assert_eq!(app.wire_log()[0].status, PacketStatus::Queued);
    }

    #[test]
    fn reconnect_sync_converges() {
        let mut app = DemoApp::new();
        app.toggle_disconnect();
        draw_stroke(&mut app, Peer::Alice);
        draw_stroke(&mut app, Peer::Bob);
        app.reconnect_and_sync();
        let stats = app.stats();
        assert_eq!(stats.alice_visible, 2);
        assert_eq!(stats.bob_visible, 2);
        assert_eq!(stats.alice_queued, 0);
        assert_eq!(stats.bob_queued, 0);
    }

    #[test]
    fn undo_propagates_delete() {
        let mut app = DemoApp::new();
        draw_stroke(&mut app, Peer::Alice);
        app.tick(1_000.0);
        assert_eq!(app.stats().bob_visible, 1);
        let events = app.undo(Peer::Alice);
        assert!(matches!(
            events.as_slice(),
            [AppEvent::PacketCreated { .. }]
        ));
        app.tick(2_000.0);
        assert_eq!(app.stats().alice_visible, 0);
        assert_eq!(app.stats().bob_visible, 0);
    }

    #[test]
    fn clear_all_resets_state() {
        let mut app = DemoApp::new();
        draw_stroke(&mut app, Peer::Alice);
        let events = app.clear_all();
        assert_eq!(events, vec![AppEvent::ClearPackets]);
        assert_eq!(app.stats().alice_visible, 0);
        assert_eq!(app.stats().total_bytes, 0);
        assert!(app.wire_log().is_empty());
    }
}
