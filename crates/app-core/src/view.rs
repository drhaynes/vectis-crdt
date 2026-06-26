use crate::stroke::AppPoint;

#[derive(Clone, Debug, PartialEq)]
pub struct StrokeView {
    pub color: u32,
    pub width: f32,
    pub opacity: f32,
    pub points: Vec<AppPoint>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AppStats {
    pub alice_visible: usize,
    pub bob_visible: usize,
    pub alice_queued: usize,
    pub bob_queued: usize,
    pub alice_undo_depth: usize,
    pub bob_undo_depth: usize,
    pub total_packets: u32,
    pub total_bytes: usize,
    pub alice_bytes: usize,
    pub bob_bytes: usize,
}
