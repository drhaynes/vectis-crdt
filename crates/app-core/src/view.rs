use crate::stroke::AppPoint;

#[derive(Clone, Debug, PartialEq)]
pub struct StrokeView {
    pub color: u32,
    pub width: f32,
    pub opacity: f32,
    pub points: Vec<AppPoint>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CursorView {
    pub actor: u64,
    pub color: u32,
    pub point: AppPoint,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppStats {
    pub actor: Option<u64>,
    pub resume_token: String,
    pub visible_strokes: usize,
    pub undo_depth: usize,
    pub remote_cursors: usize,
    pub gc_generation: u64,
    pub frames_sent: u32,
    pub frames_received: u32,
    pub bytes_sent: usize,
    pub bytes_received: usize,
    pub status: String,
}
