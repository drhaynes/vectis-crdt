use vectis_crdt::stroke::StrokePoint;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AppPoint {
    pub x: f32,
    pub y: f32,
    pub pressure: f32,
}

impl AppPoint {
    pub fn new(x: f32, y: f32, pressure: f32) -> Self {
        Self { x, y, pressure }
    }
}

pub(crate) struct LiveStroke {
    pub(crate) color: u32,
    pub(crate) points: Vec<AppPoint>,
}

impl LiveStroke {
    pub(crate) fn new(color: u32, point: AppPoint) -> Self {
        Self {
            color,
            points: vec![point],
        }
    }
}

pub(crate) fn stroke_point_from_app(point: &AppPoint) -> StrokePoint {
    StrokePoint::new(point.x, point.y, point.pressure)
}

pub(crate) fn app_point_from_stroke(point: &StrokePoint) -> AppPoint {
    AppPoint::new(point.x, point.y, point.pressure)
}
