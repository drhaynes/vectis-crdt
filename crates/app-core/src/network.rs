#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    AliceToBob,
    BobToAlice,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PacketStatus {
    Inflight,
    Delivered,
    Queued,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PacketView {
    pub id: u32,
    pub direction: Direction,
    pub bytes: usize,
    pub progress: f64,
    pub opacity: f64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WireEntry {
    pub id: Option<u32>,
    pub direction: Direction,
    pub bytes: usize,
    pub hex: String,
    pub status: PacketStatus,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NetworkState {
    pub delay_ms: u32,
    pub disconnected: bool,
}

pub(crate) const MAX_LOG: usize = 10;

pub(crate) struct QueuedPacket {
    pub(crate) direction: Direction,
    pub(crate) payload: Vec<u8>,
}

pub(crate) struct InflightPacket {
    pub(crate) id: u32,
    pub(crate) start_time: f64,
    pub(crate) duration: f64,
    pub(crate) direction: Direction,
    pub(crate) payload: Vec<u8>,
    pub(crate) bytes: usize,
}

pub(crate) fn packet_progress(packet: &InflightPacket, now_ms: f64) -> f64 {
    ((now_ms - packet.start_time) / packet.duration).clamp(0.0, 1.0)
}

pub(crate) fn packet_opacity(progress: f64) -> f64 {
    if progress < 0.15 {
        progress / 0.15
    } else if progress > 0.85 {
        (1.0 - progress) / 0.15
    } else {
        1.0
    }
}

pub(crate) fn hex_prefix(payload: &[u8]) -> String {
    payload
        .iter()
        .take(7)
        .map(|byte| format!("{:02x}", byte))
        .collect::<Vec<_>>()
        .join(" ")
}
