use crate::types::*;
use crate::rga::StrokeId;
use std::collections::HashMap;

// ─── StrokePoint ─────────────────────────────────────────────────────────────

/// A point in a stroke. Coordinates + pressure (for stylus).
/// Layout: 3 × f32 = 12 bytes — cache-line friendly.
#[derive(Debug, Clone, Copy)]
pub struct StrokePoint {
    pub x: f32,
    pub y: f32,
    /// 0.0–1.0; defaults to 1.0 for mouse/touch.
    pub pressure: f32,
}

impl StrokePoint {
    #[inline]
    pub fn new(x: f32, y: f32, pressure: f32) -> Self {
        Self { x, y, pressure }
    }

    #[inline]
    pub fn basic(x: f32, y: f32) -> Self {
        Self { x, y, pressure: 1.0 }
    }
}

// ─── Aabb ─────────────────────────────────────────────────────────────────────

/// Axis-Aligned Bounding Box. Computed once at stroke creation, updated after
/// point simplification. Never serialized to wire (derived from points).
///
/// Coordinates are in canvas space, no padding — callers add `stroke_width/2`
/// via [`Aabb::expanded`] before intersection tests.
#[derive(Debug, Clone, Copy)]
pub struct Aabb {
    pub min_x: f32,
    pub min_y: f32,
    pub max_x: f32,
    pub max_y: f32,
}

impl Aabb {
    /// An AABB that intersects everything. Used for empty/degenerate strokes
    /// so they are never wrongly culled.
    pub const INFINITE: Aabb = Aabb {
        min_x: f32::NEG_INFINITY,
        min_y: f32::NEG_INFINITY,
        max_x: f32::INFINITY,
        max_y: f32::INFINITY,
    };

    /// Compute a tight AABB from a slice of points.
    /// Returns `INFINITE` for empty slices (never cull).
    pub fn from_points(points: &[StrokePoint]) -> Self {
        if points.is_empty() {
            return Self::INFINITE;
        }
        let mut min_x = f32::INFINITY;
        let mut min_y = f32::INFINITY;
        let mut max_x = f32::NEG_INFINITY;
        let mut max_y = f32::NEG_INFINITY;
        for p in points {
            if p.x < min_x { min_x = p.x; }
            if p.y < min_y { min_y = p.y; }
            if p.x > max_x { max_x = p.x; }
            if p.y > max_y { max_y = p.y; }
        }
        Aabb { min_x, min_y, max_x, max_y }
    }

    /// Returns true if `self` overlaps with `other`.
    #[inline]
    pub fn intersects(&self, other: &Aabb) -> bool {
        self.min_x <= other.max_x
            && self.max_x >= other.min_x
            && self.min_y <= other.max_y
            && self.max_y >= other.min_y
    }

    /// Expand uniformly by `padding` on all sides.
    /// Use this to account for stroke width: `bounds.expanded(stroke_width / 2.0)`.
    #[inline]
    pub fn expanded(self, padding: f32) -> Aabb {
        Aabb {
            min_x: self.min_x - padding,
            min_y: self.min_y - padding,
            max_x: self.max_x + padding,
            max_y: self.max_y + padding,
        }
    }

    /// Transform this AABB by an affine [`Transform2D`].
    /// Returns the new AABB enclosing all four transformed corners.
    /// Used before culling when a stroke has a non-identity transform.
    pub fn transform(&self, t: &Transform2D) -> Aabb {
        let corners = [
            (self.min_x, self.min_y),
            (self.max_x, self.min_y),
            (self.min_x, self.max_y),
            (self.max_x, self.max_y),
        ];
        let mut min_x = f32::INFINITY;
        let mut min_y = f32::INFINITY;
        let mut max_x = f32::NEG_INFINITY;
        let mut max_y = f32::NEG_INFINITY;
        for (x, y) in corners {
            let tx = t.a * x + t.c * y + t.tx;
            let ty = t.b * x + t.d * y + t.ty;
            if tx < min_x { min_x = tx; }
            if ty < min_y { min_y = ty; }
            if tx > max_x { max_x = tx; }
            if ty > max_y { max_y = ty; }
        }
        Aabb { min_x, min_y, max_x, max_y }
    }
}

// ─── RDP — Ramer-Douglas-Peucker ─────────────────────────────────────────────

/// Perpendicular distance from point `p` to line segment `a`–`b`.
#[inline]
fn perp_dist(p: &StrokePoint, a: &StrokePoint, b: &StrokePoint) -> f32 {
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    let len_sq = dx * dx + dy * dy;
    if len_sq < 1e-10 {
        // Degenerate segment (a == b): fall back to point-to-point distance.
        let ex = p.x - a.x;
        let ey = p.y - a.y;
        return (ex * ex + ey * ey).sqrt();
    }
    // ||(b−a) × (a−p)|| / ||b−a||
    let cross = (dx * (a.y - p.y) - dy * (a.x - p.x)).abs();
    cross / len_sq.sqrt()
}

/// Compute which point indices to keep using Ramer-Douglas-Peucker.
/// Uses an **iterative (stack-based)** approach — no stack-overflow risk even
/// for strokes with tens of thousands of points.
///
/// Always retains index `0` and `n - 1`.
fn rdp_indices(points: &[StrokePoint], epsilon: f32) -> Vec<usize> {
    let n = points.len();
    if n <= 2 {
        return (0..n).collect();
    }

    let mut keep = vec![false; n];
    keep[0] = true;
    keep[n - 1] = true;

    // Stack of (start_inclusive, end_inclusive) ranges to process.
    let mut stack: Vec<(usize, usize)> = Vec::with_capacity(64);
    stack.push((0, n - 1));

    while let Some((start, end)) = stack.pop() {
        if end <= start + 1 {
            continue;
        }
        let a = &points[start];
        let b = &points[end];
        let mut max_dist = 0.0f32;
        let mut max_idx = start;

        for (idx, p) in points.iter().enumerate().take(end).skip(start + 1) {
            let d = perp_dist(p, a, b);
            if d > max_dist {
                max_dist = d;
                max_idx = idx;
            }
        }

        if max_dist > epsilon {
            keep[max_idx] = true;
            stack.push((start, max_idx));
            stack.push((max_idx, end));
        }
        // else: all intermediate points within epsilon — drop them.
    }

    keep.iter()
        .enumerate()
        .filter_map(|(i, &k)| if k { Some(i) } else { None })
        .collect()
}

// ─── ToolKind ────────────────────────────────────────────────────────────────

/// Tool kind that generated the stroke.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ToolKind {
    Pen     = 0,
    Eraser  = 1,
    Marker  = 2,
    Laser   = 3,
    Shape   = 4,
    Arrow   = 5,
}

impl ToolKind {
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Pen,
            1 => Self::Eraser,
            2 => Self::Marker,
            3 => Self::Laser,
            4 => Self::Shape,
            5 => Self::Arrow,
            _ => Self::Pen,
        }
    }
}

// ─── StrokeData ───────────────────────────────────────────────────────────────

/// Immutable stroke path data. Created once; if the user "edits" a stroke,
/// the old one is deleted and a new one created (simplifies the CRDT model).
///
/// `bounds` is a tight AABB computed from `points` — **not serialized to wire**
/// (recomputed on decode). Add `stroke_width / 2` via [`Aabb::expanded`] before
/// viewport intersection tests.
#[derive(Debug, Clone)]
pub struct StrokeData {
    /// Path points. Immutable after creation / simplification.
    pub points: Box<[StrokePoint]>,
    /// Tool type.
    pub tool: ToolKind,
    /// Tight AABB (no padding). Recomputed after simplification.
    pub bounds: Aabb,
}

impl StrokeData {
    /// Canonical constructor. Automatically computes `bounds` from `points`.
    pub fn new(points: Box<[StrokePoint]>, tool: ToolKind) -> Self {
        let bounds = Aabb::from_points(&points);
        Self { points, tool, bounds }
    }

    /// Simplify points in-place using Ramer-Douglas-Peucker.
    ///
    /// `epsilon`: maximum allowed perpendicular deviation in canvas units.
    /// Recommended values:
    /// - `0.5` for high-DPI displays and fine-grained stylus input
    /// - `1.0` for standard displays
    ///
    /// Returns the number of points removed. Recomputes `bounds` afterward.
    /// No-op for strokes with ≤ 2 points.
    ///
    /// **Typical reduction**: a 500-point freehand stroke simplifies to
    /// ~30–80 points (~88% reduction) with epsilon = 0.5, with no perceptible
    /// visual difference at normal zoom levels.
    pub fn simplify(&mut self, epsilon: f32) -> usize {
        let original = self.points.len();
        if original <= 2 {
            return 0;
        }
        let indices = rdp_indices(&self.points, epsilon);
        let kept = indices.len();
        if kept == original {
            return 0;
        }
        let new_points: Box<[StrokePoint]> = indices.iter().map(|&i| self.points[i]).collect();
        self.bounds = Aabb::from_points(&new_points);
        self.points = new_points;
        original - kept
    }
}

// ─── LwwRegister ─────────────────────────────────────────────────────────────

/// LWW (Last-Write-Wins) register.
/// `T` is the value; the timestamp determines the winner.
#[derive(Debug, Clone)]
pub struct LwwRegister<T: Clone> {
    pub value: T,
    pub timestamp: OpId,
}

impl<T: Clone> LwwRegister<T> {
    pub fn new(value: T, timestamp: OpId) -> Self {
        Self { value, timestamp }
    }

    /// Applies a new value only if `timestamp` is strictly greater.
    /// Returns true if the value changed.
    pub fn apply(&mut self, value: T, timestamp: OpId) -> bool {
        if timestamp > self.timestamp {
            self.value = value;
            self.timestamp = timestamp;
            true
        } else {
            false
        }
    }
}

// ─── Transform2D ─────────────────────────────────────────────────────────────

/// Affine 2D transform: `[a, b, c, d, tx, ty]`.
/// Equivalent to CSS `matrix()` / SVG `transform`.
#[derive(Debug, Clone, Copy)]
pub struct Transform2D {
    pub a: f32,
    pub b: f32,
    pub c: f32,
    pub d: f32,
    pub tx: f32,
    pub ty: f32,
}

impl Default for Transform2D {
    fn default() -> Self {
        // Identity matrix
        Self { a: 1.0, b: 0.0, c: 0.0, d: 1.0, tx: 0.0, ty: 0.0 }
    }
}

impl Transform2D {
    /// Returns true if this is the identity transform.
    #[inline]
    pub fn is_identity(&self) -> bool {
        self.a == 1.0
            && self.b == 0.0
            && self.c == 0.0
            && self.d == 1.0
            && self.tx == 0.0
            && self.ty == 0.0
    }
}

// ─── StrokeProperties ────────────────────────────────────────────────────────

/// Mutable properties of a stroke. Each field is an independent LWW-Register,
/// enabling granular concurrent merges without conflict.
///
/// Example: User A changes color while User B changes opacity → both changes
/// are preserved.
#[derive(Debug, Clone)]
pub struct StrokeProperties {
    /// RGBA packed as `0xRRGGBBAA`.
    pub color: LwwRegister<u32>,
    pub stroke_width: LwwRegister<f32>,
    /// 0.0–1.0
    pub opacity: LwwRegister<f32>,
    pub transform: LwwRegister<Transform2D>,
}

impl StrokeProperties {
    pub fn new(color: u32, stroke_width: f32, opacity: f32, id: OpId) -> Self {
        Self {
            color: LwwRegister::new(color, id),
            stroke_width: LwwRegister::new(stroke_width, id),
            opacity: LwwRegister::new(opacity, id),
            transform: LwwRegister::new(Transform2D::default(), id),
        }
    }
}

// ─── StrokeStore ─────────────────────────────────────────────────────────────

/// Stroke store: `StrokeId → (immutable data, mutable properties)`.
/// Separate from `RgaArray` to keep the ordering structure compact.
pub struct StrokeStore {
    /// `pub(crate)` — direct HashMap access bypasses all CRDT invariants.
    /// Use the provided methods instead.
    pub(crate) strokes: HashMap<StrokeId, (StrokeData, StrokeProperties)>,
}

impl StrokeStore {
    pub fn new() -> Self {
        Self { strokes: HashMap::new() }
    }

    pub fn insert(&mut self, id: StrokeId, data: StrokeData, props: StrokeProperties) {
        self.strokes.insert(id, (data, props));
    }

    pub fn get(&self, id: &StrokeId) -> Option<(&StrokeData, &StrokeProperties)> {
        self.strokes.get(id).map(|(d, p)| (d, p))
    }

    pub fn get_mut(&mut self, id: &StrokeId) -> Option<(&StrokeData, &mut StrokeProperties)> {
        self.strokes.get_mut(id).map(|(d, p)| (&*d, p))
    }

    pub fn get_data_mut(&mut self, id: &StrokeId) -> Option<&mut StrokeData> {
        self.strokes.get_mut(id).map(|(d, _)| d)
    }

    pub fn remove(&mut self, id: &StrokeId) -> Option<(StrokeData, StrokeProperties)> {
        self.strokes.remove(id)
    }

    pub fn contains(&self, id: &StrokeId) -> bool {
        self.strokes.contains_key(id)
    }
}

impl Default for StrokeStore {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lww_register_apply() {
        let id_a = OpId { lamport: LamportTs(1), actor: ActorId(1) };
        let id_b = OpId { lamport: LamportTs(2), actor: ActorId(1) };
        let id_c = OpId { lamport: LamportTs(1), actor: ActorId(1) };

        let mut reg = LwwRegister::new(10u32, id_a);
        assert!(reg.apply(20, id_b));
        assert_eq!(reg.value, 20);
        assert!(!reg.apply(5, id_c));
        assert_eq!(reg.value, 20);
    }

    #[test]
    fn transform2d_default_is_identity() {
        let t = Transform2D::default();
        assert!(t.is_identity());
        let t2 = Transform2D { a: 1.0, b: 0.0, c: 0.0, d: 1.0, tx: 1.0, ty: 0.0 };
        assert!(!t2.is_identity());
    }

    #[test]
    fn aabb_from_points() {
        let pts = vec![
            StrokePoint::basic(0.0, 0.0),
            StrokePoint::basic(10.0, 5.0),
            StrokePoint::basic(-2.0, 8.0),
        ];
        let b = Aabb::from_points(&pts);
        assert_eq!(b.min_x, -2.0);
        assert_eq!(b.min_y, 0.0);
        assert_eq!(b.max_x, 10.0);
        assert_eq!(b.max_y, 8.0);
    }

    #[test]
    fn aabb_intersects() {
        let a = Aabb { min_x: 0.0, min_y: 0.0, max_x: 10.0, max_y: 10.0 };
        let b = Aabb { min_x: 5.0, min_y: 5.0, max_x: 15.0, max_y: 15.0 };
        let c = Aabb { min_x: 20.0, min_y: 20.0, max_x: 30.0, max_y: 30.0 };
        assert!(a.intersects(&b));
        assert!(b.intersects(&a));
        assert!(!a.intersects(&c));
    }

    #[test]
    fn aabb_expanded() {
        let a = Aabb { min_x: 0.0, min_y: 0.0, max_x: 10.0, max_y: 10.0 };
        let e = a.expanded(2.0);
        assert_eq!(e.min_x, -2.0);
        assert_eq!(e.max_x, 12.0);
    }

    #[test]
    fn aabb_transform_identity_leaves_bounds_unchanged() {
        let a = Aabb { min_x: 1.0, min_y: 2.0, max_x: 5.0, max_y: 6.0 };
        let t = Transform2D::default();
        let b = a.transform(&t);
        assert!((b.min_x - 1.0).abs() < 1e-5);
        assert!((b.min_y - 2.0).abs() < 1e-5);
        assert!((b.max_x - 5.0).abs() < 1e-5);
        assert!((b.max_y - 6.0).abs() < 1e-5);
    }

    #[test]
    fn aabb_transform_translation() {
        let a = Aabb { min_x: 0.0, min_y: 0.0, max_x: 10.0, max_y: 10.0 };
        let t = Transform2D { a: 1.0, b: 0.0, c: 0.0, d: 1.0, tx: 5.0, ty: -3.0 };
        let b = a.transform(&t);
        assert!((b.min_x - 5.0).abs() < 1e-5);
        assert!((b.min_y - (-3.0)).abs() < 1e-5);
        assert!((b.max_x - 15.0).abs() < 1e-5);
        assert!((b.max_y - 7.0).abs() < 1e-5);
    }

    #[test]
    fn rdp_straight_line_simplified_to_endpoints() {
        // 100 collinear points — RDP should keep only 2 (first and last).
        let pts: Box<[StrokePoint]> = (0..100)
            .map(|i| StrokePoint::basic(i as f32, i as f32))
            .collect();
        let mut data = StrokeData::new(pts, ToolKind::Pen);
        let removed = data.simplify(0.5);
        assert_eq!(data.points.len(), 2, "straight line → 2 points");
        assert_eq!(removed, 98);
    }

    #[test]
    fn rdp_curve_keeps_shape() {
        // Sine wave — should keep many points to represent the curve.
        let pts: Box<[StrokePoint]> = (0..=360)
            .map(|i| {
                let t = (i as f32).to_radians();
                StrokePoint::basic(i as f32, t.sin() * 100.0)
            })
            .collect();
        let n = pts.len();
        let mut data = StrokeData::new(pts, ToolKind::Pen);
        let removed = data.simplify(1.0);
        assert!(data.points.len() < n / 2, "should reduce significantly");
        assert!(data.points.len() > 2, "should retain shape");
        assert_eq!(data.points.len() + removed, n);
    }

    #[test]
    fn rdp_two_points_unchanged() {
        let pts: Box<[StrokePoint]> = vec![
            StrokePoint::basic(0.0, 0.0),
            StrokePoint::basic(10.0, 10.0),
        ].into();
        let mut data = StrokeData::new(pts, ToolKind::Pen);
        assert_eq!(data.simplify(0.5), 0);
        assert_eq!(data.points.len(), 2);
    }

    #[test]
    fn stroke_data_bounds_computed_on_new() {
        let pts: Box<[StrokePoint]> = vec![
            StrokePoint::basic(3.0, 7.0),
            StrokePoint::basic(9.0, 2.0),
        ].into();
        let data = StrokeData::new(pts, ToolKind::Pen);
        assert_eq!(data.bounds.min_x, 3.0);
        assert_eq!(data.bounds.min_y, 2.0);
        assert_eq!(data.bounds.max_x, 9.0);
        assert_eq!(data.bounds.max_y, 7.0);
    }

    #[test]
    fn simplify_recomputes_bounds() {
        // Diagonal line — simplify drops intermediate points but endpoints remain.
        let pts: Box<[StrokePoint]> = (0..=10)
            .map(|i| StrokePoint::basic(i as f32, i as f32))
            .collect();
        let mut data = StrokeData::new(pts, ToolKind::Pen);
        let removed = data.simplify(0.1);
        assert!(removed > 0);
        // Endpoints kept → full extent preserved
        assert!((data.bounds.min_x - 0.0).abs() < 1e-5);
        assert!((data.bounds.max_x - 10.0).abs() < 1e-5);
    }
}
