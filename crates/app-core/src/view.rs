use crate::stroke::AppPoint;

#[derive(Clone, Debug, PartialEq)]
pub struct StrokeView {
    pub color: u32,
    pub width: f32,
    pub opacity: f32,
    pub points: Vec<AppPoint>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppStats {
    pub actor: Option<u64>,
    pub visible_strokes: usize,
    pub undo_depth: usize,
    pub frames_sent: u32,
    pub frames_received: u32,
    pub bytes_sent: usize,
    pub bytes_received: usize,
    pub status: String,
}
