use crate::rga::{ItemState, RgaArray, RgaItem, StrokeId};
use crate::types::*;

/// Maximum number of undo steps retained per session.
/// Prevents unbounded memory growth in long drawing sessions.
const MAX_UNDO_DEPTH: usize = 200;

// ─── Safety limits ─────────────────────────────────────────────────────────
// These are enforced on both local inserts and remote decoding to prevent
// resource exhaustion from malformed or malicious data.

/// Maximum points per stroke. At 240 Hz for 3 minutes = ~43k points.
/// 50k is generous enough for any realistic session segment.
pub const MAX_POINTS_PER_STROKE: usize = 50_000;

/// Maximum total strokes (active + tombstone) per document.
/// 100k strokes ≈ ~8 MB RGA memory — reasonable upper bound.
pub const MAX_STROKES: usize = 100_000;

/// Maximum distinct actors tracked in a VectorClock.
/// Prevents unbounded BTreeMap growth from spoofed actor IDs.
pub const MAX_ACTORS: usize = 10_000;
use crate::stroke::*;
use std::collections::HashMap;

// ─── LWW Map ──────────────────────────────────────────────────────────────────

/// Generic LWW-Map for metadata (viewport, grid config, etc.).
pub struct LwwMap<K: Eq + std::hash::Hash, V: Clone> {
    entries: HashMap<K, LwwRegister<Option<V>>>,
}

impl<K: Eq + std::hash::Hash, V: Clone> LwwMap<K, V> {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    pub fn set(&mut self, key: K, value: V, ts: OpId) {
        self.entries
            .entry(key)
            .and_modify(|reg| {
                reg.apply(Some(value.clone()), ts);
            })
            .or_insert_with(|| LwwRegister::new(Some(value), ts));
    }

    pub fn delete(&mut self, key: K, ts: OpId) {
        self.entries
            .entry(key)
            .and_modify(|reg| {
                reg.apply(None, ts);
            })
            .or_insert_with(|| LwwRegister::new(None, ts));
    }

    pub fn get(&self, key: &K) -> Option<&V> {
        self.entries.get(key)?.value.as_ref()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&K, &Option<V>)> {
        self.entries.iter().map(|(k, reg)| (k, &reg.value))
    }
}

impl<K: Eq + std::hash::Hash, V: Clone> Default for LwwMap<K, V> {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Metadata ─────────────────────────────────────────────────────────────────

/// Well-known keys for the metadata LWW-Map.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum MetadataKey {
    ViewportX,
    ViewportY,
    ViewportZoom,
    BackgroundColor,
    GridEnabled,
    GridSpacing,
    Custom(String),
}

/// Typed value for metadata.
#[derive(Debug, Clone)]
pub enum MetadataValue {
    F64(f64),
    Bool(bool),
    U32(u32),
    String(String),
}

// ─── Operations ───────────────────────────────────────────────────────────────

/// A serializable operation. This is what travels over the network
/// and is stored in the op-log.
#[derive(Debug, Clone)]
pub enum Operation {
    InsertStroke {
        id: OpId,
        origin_left: OpId,
        origin_right: OpId,
        data: StrokeData,
        properties: StrokeProperties,
    },
    DeleteStroke {
        /// OpId of this delete operation.
        id: OpId,
        /// The stroke to delete.
        target: StrokeId,
    },
    UpdateProperty {
        id: OpId,
        target: StrokeId,
        update: PropertyUpdate,
    },
    UpdateMetadata {
        id: OpId,
        key: MetadataKey,
        /// None = delete the key.
        value: Option<MetadataValue>,
    },
}

#[derive(Debug, Clone)]
pub enum PropertyUpdate {
    Color(u32),
    StrokeWidth(f32),
    Opacity(f32),
    Transform(Transform2D),
}

impl Operation {
    /// Extract the primary OpId of this operation.
    pub fn id(&self) -> OpId {
        match self {
            Operation::InsertStroke { id, .. } => *id,
            Operation::DeleteStroke { id, .. } => *id,
            Operation::UpdateProperty { id, .. } => *id,
            Operation::UpdateMetadata { id, .. } => *id,
        }
    }

    /// Extract the affected stroke ID (if any).
    pub fn target_stroke(&self) -> Option<StrokeId> {
        match self {
            Operation::InsertStroke { id, .. } => Some(*id),
            Operation::DeleteStroke { target, .. } => Some(*target),
            Operation::UpdateProperty { target, .. } => Some(*target),
            Operation::UpdateMetadata { .. } => None,
        }
    }
}

// ─── Document ─────────────────────────────────────────────────────────────────

/// The document root. Contains the full CRDT state.
pub struct Document {
    // — Identity —
    /// Actor ID of this client. Assigned by server on first connect.
    pub local_actor: ActorId,
    /// Lamport clock. `pub(crate)` — must only advance via `next_op_id`.
    pub(crate) clock: LamportTs,

    // — Causal Tracking —
    /// Vector clock: tracks what we've seen from each peer.
    /// `pub` — read by callers for delta-sync state vector encoding.
    pub version: VectorClock,

    // — CRDT Structures —
    /// Ordered list of strokes (z-ordering). RGA.
    /// `pub(crate)` — external mutation would corrupt the index.
    pub(crate) stroke_order: RgaArray,
    /// Stroke data + properties store.
    /// `pub(crate)` — external mutation bypasses CRDT invariants.
    pub(crate) stroke_store: StrokeStore,
    /// Canvas metadata (viewport, grid, etc.). LWW-Map.
    pub(crate) metadata: LwwMap<MetadataKey, MetadataValue>,

    // — Op Log —
    /// Pending operations to send to the server.
    /// `pub(crate)` — drain via `take_pending_ops()` from outside the crate.
    pub(crate) pending_ops: Vec<Operation>,

    // — GC —
    /// Minimum Version Vector: the version that ALL known peers have
    /// confirmed. Operations ≤ MVV are "causally stable" and GC-eligible.
    /// `pub(crate)` — must only be set via `update_min_version()` to
    /// prevent premature GC that would corrupt the document.
    pub(crate) min_version: VectorClock,
    /// GC run counter for statistics.
    pub(crate) gc_generation: u64,

    // — Local UX —
    /// Session-local undo stack. Not persisted; not serialized to wire.
    undo_stack: Vec<StrokeId>,

    /// RDP epsilon applied automatically to every inserted stroke.
    /// Set to `0.0` to disable auto-simplification.
    /// Default: `0.5` (sub-pixel accuracy on high-DPI displays).
    pub simplify_epsilon: f32,
}

impl Document {
    pub fn new(actor: ActorId) -> Self {
        Self {
            local_actor: actor,
            clock: LamportTs(0),
            version: VectorClock::default(),
            stroke_order: RgaArray::new(),
            stroke_store: StrokeStore::new(),
            metadata: LwwMap::new(),
            pending_ops: Vec::new(),
            min_version: VectorClock::default(),
            gc_generation: 0,
            undo_stack: Vec::new(),
            simplify_epsilon: 0.5,
        }
    }

    /// Drain and return all pending operations accumulated since the last flush.
    /// Call this to get the ops to encode and send to the server.
    pub fn take_pending_ops(&mut self) -> Vec<Operation> {
        std::mem::take(&mut self.pending_ops)
    }

    /// Generate the next OpId for a local operation.
    pub(crate) fn next_op_id(&mut self) -> OpId {
        let ts = self.clock.tick();
        let id = OpId {
            lamport: ts,
            actor: self.local_actor,
        };
        self.version.advance(self.local_actor, ts.0);
        id
    }

    // ─── High-Level Local API ─────────────────────────────────────────────────

    /// Insert a stroke at the top of the z-order (after all existing strokes).
    ///
    /// If `simplify_epsilon > 0`, automatically applies Ramer-Douglas-Peucker
    /// before storing and broadcasting — reducing point count and wire size.
    pub fn insert_stroke(
        &mut self,
        mut data: StrokeData,
        properties: StrokeProperties,
    ) -> StrokeId {
        // Auto-simplify before storing. Benefits: smaller RAM, smaller wire payload.
        // Threshold > 2 because simplify() already no-ops on ≤ 2 points.
        if self.simplify_epsilon > 0.0 && data.points.len() > 2 {
            data.simplify(self.simplify_epsilon);
        }

        let id = self.next_op_id();

        let origin_left = self
            .stroke_order
            .visible_items()
            .last()
            .map(|item| item.id)
            .unwrap_or(OpId::ZERO);

        let item = RgaItem {
            id,
            origin_left,
            origin_right: OpId::ZERO,
            content: id,
            state: ItemState::Active,
        };

        self.stroke_order.integrate(item);
        self.stroke_store
            .insert(id, data.clone(), properties.clone());

        self.pending_ops.push(Operation::InsertStroke {
            id,
            origin_left,
            origin_right: OpId::ZERO,
            data,
            properties,
        });

        // Track for undo. Drop the oldest entry if at capacity.
        if self.undo_stack.len() >= MAX_UNDO_DEPTH {
            self.undo_stack.remove(0);
        }
        self.undo_stack.push(id);

        id
    }

    /// Delete a stroke by ID.
    pub fn delete_stroke(&mut self, target: StrokeId) -> bool {
        if !self.stroke_store.contains(&target) {
            return false;
        }
        let id = self.next_op_id();
        let deleted = self.stroke_order.mark_deleted(target, id);
        if deleted {
            self.pending_ops
                .push(Operation::DeleteStroke { id, target });
        }
        deleted
    }

    /// Update stroke color.
    pub fn update_color(&mut self, target: StrokeId, color: u32) -> bool {
        let id = self.next_op_id();
        if let Some((_, props)) = self.stroke_store.get_mut(&target) {
            props.color.apply(color, id);
            self.pending_ops.push(Operation::UpdateProperty {
                id,
                target,
                update: PropertyUpdate::Color(color),
            });
            true
        } else {
            false
        }
    }

    /// Update stroke width.
    pub fn update_stroke_width(&mut self, target: StrokeId, width: f32) -> bool {
        let id = self.next_op_id();
        if let Some((_, props)) = self.stroke_store.get_mut(&target) {
            props.stroke_width.apply(width, id);
            self.pending_ops.push(Operation::UpdateProperty {
                id,
                target,
                update: PropertyUpdate::StrokeWidth(width),
            });
            true
        } else {
            false
        }
    }

    /// Update stroke opacity.
    pub fn update_opacity(&mut self, target: StrokeId, opacity: f32) -> bool {
        let id = self.next_op_id();
        if let Some((_, props)) = self.stroke_store.get_mut(&target) {
            props.opacity.apply(opacity, id);
            self.pending_ops.push(Operation::UpdateProperty {
                id,
                target,
                update: PropertyUpdate::Opacity(opacity),
            });
            true
        } else {
            false
        }
    }

    /// Update stroke transform.
    pub fn update_transform(&mut self, target: StrokeId, transform: Transform2D) -> bool {
        let id = self.next_op_id();
        if let Some((_, props)) = self.stroke_store.get_mut(&target) {
            props.transform.apply(transform, id);
            self.pending_ops.push(Operation::UpdateProperty {
                id,
                target,
                update: PropertyUpdate::Transform(transform),
            });
            true
        } else {
            false
        }
    }

    /// Set a metadata key.
    pub fn set_metadata(&mut self, key: MetadataKey, value: MetadataValue) {
        let id = self.next_op_id();
        self.metadata.set(key.clone(), value.clone(), id);
        self.pending_ops.push(Operation::UpdateMetadata {
            id,
            key,
            value: Some(value),
        });
    }

    // ─── Remote Apply ─────────────────────────────────────────────────────────

    /// Apply a remote operation. Core of the merge.
    /// Returns the affected StrokeId (if any) for incremental re-render.
    pub fn apply_remote(&mut self, op: Operation) -> Option<StrokeId> {
        // Advance Lamport clock with the remote timestamp.
        let op_id = op.id();
        self.clock.merge(op_id.lamport);
        self.version.advance(op_id.actor, op_id.lamport.0);

        let affected = op.target_stroke();

        match op {
            Operation::InsertStroke {
                id,
                origin_left,
                origin_right,
                data,
                properties,
            } => {
                // Skip if already applied (idempotent).
                if !self.stroke_store.contains(&id) {
                    // Enforce document size limit. Silently drop if exceeded —
                    // better than OOM. In practice the server should enforce
                    // this before broadcasting.
                    if self.stroke_order.total_count >= MAX_STROKES {
                        return affected;
                    }
                    let item = RgaItem {
                        id,
                        origin_left,
                        origin_right,
                        content: id,
                        state: ItemState::Active,
                    };
                    self.stroke_order.integrate(item);
                    self.stroke_store.insert(id, data, properties);
                }
            }

            Operation::DeleteStroke { id, target } => {
                self.stroke_order.mark_deleted(target, id);
                // Do NOT remove from stroke_store yet — wait for GC.
            }

            Operation::UpdateProperty { id, target, update } => {
                if let Some((_, props)) = self.stroke_store.get_mut(&target) {
                    match update {
                        PropertyUpdate::Color(v) => {
                            props.color.apply(v, id);
                        }
                        PropertyUpdate::StrokeWidth(v) => {
                            props.stroke_width.apply(v, id);
                        }
                        PropertyUpdate::Opacity(v) => {
                            props.opacity.apply(v, id);
                        }
                        PropertyUpdate::Transform(v) => {
                            props.transform.apply(v, id);
                        }
                    }
                }
            }

            Operation::UpdateMetadata { id, key, value } => match value {
                Some(v) => self.metadata.set(key, v, id),
                None => self.metadata.delete(key, id),
            },
        }

        affected
    }

    // ─── Simplification ──────────────────────────────────────────────────────

    /// Simplify an existing stroke's points using Ramer-Douglas-Peucker.
    /// Returns the number of points removed, or 0 if not found.
    ///
    /// Does NOT generate a pending op — simplification is a local optimization
    /// that reduces memory. The simplified version is only sent if the stroke
    /// is subsequently re-inserted. For existing strokes already synced to
    /// peers, use this only on your local replica.
    pub fn simplify_stroke(&mut self, target: StrokeId, epsilon: f32) -> usize {
        if let Some(data) = self.stroke_store.get_data_mut(&target) {
            data.simplify(epsilon)
        } else {
            0
        }
    }

    // ─── Undo ─────────────────────────────────────────────────────────────────

    /// Undo the last stroke drawn by the local actor.
    ///
    /// Pops the undo stack and deletes the stroke — generating a `DeleteStroke`
    /// operation that will be broadcast to peers. If the stroke was already
    /// deleted remotely, skips it and tries the next one.
    ///
    /// Returns the `StrokeId` that was deleted, or `None` if the stack is empty.
    pub fn undo_last_stroke(&mut self) -> Option<StrokeId> {
        while let Some(id) = self.undo_stack.pop() {
            // delete_stroke returns false if already tombstoned (remote delete).
            if self.delete_stroke(id) {
                return Some(id);
            }
            // Stroke was deleted remotely → skip and try previous.
        }
        None
    }

    /// Number of strokes currently available to undo.
    pub fn undo_depth(&self) -> usize {
        self.undo_stack.len()
    }

    // ─── Queries ──────────────────────────────────────────────────────────────

    /// Returns visible stroke IDs in z-order (bottom to top).
    pub fn visible_stroke_ids(&self) -> Vec<StrokeId> {
        self.stroke_order
            .visible_items()
            .map(|item| item.content)
            .collect()
    }

    /// Returns the stroke data + properties for a given ID.
    pub fn get_stroke(&self, id: &StrokeId) -> Option<(&StrokeData, &StrokeProperties)> {
        self.stroke_store.get(id)
    }

    // ─── Statistics ───────────────────────────────────────────────────────────

    pub fn stats(&self) -> DocumentStats {
        DocumentStats {
            total_items: self.stroke_order.total_count,
            visible_items: self.stroke_order.total_count - self.stroke_order.tombstone_count,
            tombstones: self.stroke_order.tombstone_count,
            tombstone_ratio: self.stroke_order.tombstone_ratio(),
            gc_generation: self.gc_generation,
            pending_ops: self.pending_ops.len(),
        }
    }
}

/// Snapshot of document statistics.
#[derive(Debug, Clone)]
pub struct DocumentStats {
    pub total_items: usize,
    pub visible_items: usize,
    pub tombstones: usize,
    pub tombstone_ratio: f64,
    pub gc_generation: u64,
    pub pending_ops: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_doc(actor: u64) -> Document {
        Document::new(ActorId(actor))
    }

    fn simple_stroke(points: &[(f32, f32)]) -> (StrokeData, StrokeProperties) {
        let pts: Box<[StrokePoint]> = points
            .iter()
            .map(|&(x, y)| StrokePoint::basic(x, y))
            .collect();
        let data = StrokeData::new(pts, ToolKind::Pen);
        let props = StrokeProperties::new(0xFF0000FF, 2.0, 1.0, OpId::ZERO);
        (data, props)
    }

    #[test]
    fn insert_and_visible() {
        let mut doc = make_doc(1);
        let (data, props) = simple_stroke(&[(0.0, 0.0), (10.0, 10.0)]);
        let id = doc.insert_stroke(data, props);
        let visible = doc.visible_stroke_ids();
        assert_eq!(visible, vec![id]);
    }

    #[test]
    fn delete_stroke() {
        let mut doc = make_doc(1);
        let (data, props) = simple_stroke(&[(0.0, 0.0)]);
        let id = doc.insert_stroke(data, props);
        assert!(doc.delete_stroke(id));
        assert!(doc.visible_stroke_ids().is_empty());
    }

    #[test]
    fn remote_insert_merge() {
        let mut doc_a = make_doc(1);
        let mut doc_b = make_doc(2);

        let (data, props) = simple_stroke(&[(0.0, 0.0)]);
        let id_a = doc_a.insert_stroke(data.clone(), props.clone());

        // Transfer pending ops from A to B.
        let ops = std::mem::take(&mut doc_a.pending_ops);
        for op in ops {
            doc_b.apply_remote(op);
        }

        let visible_b = doc_b.visible_stroke_ids();
        assert_eq!(visible_b, vec![id_a]);
    }

    #[test]
    fn concurrent_insert_convergence() {
        let mut doc_a = make_doc(1);
        let mut doc_b = make_doc(2);

        let (data, props) = simple_stroke(&[(1.0, 1.0)]);
        let _id_a = doc_a.insert_stroke(data.clone(), props.clone());
        let _id_b = doc_b.insert_stroke(data.clone(), props.clone());

        let ops_a = std::mem::take(&mut doc_a.pending_ops);
        let ops_b = std::mem::take(&mut doc_b.pending_ops);

        for op in ops_b.clone() {
            doc_a.apply_remote(op);
        }
        for op in ops_a.clone() {
            doc_b.apply_remote(op);
        }

        let visible_a = doc_a.visible_stroke_ids();
        let visible_b = doc_b.visible_stroke_ids();
        assert_eq!(visible_a, visible_b, "docs must converge");
        assert_eq!(visible_a.len(), 2);
    }

    #[test]
    fn undo_last_stroke_basic() {
        let mut doc = make_doc(1);
        // Disable auto-simplify so 1-point strokes aren't modified.
        doc.simplify_epsilon = 0.0;

        let (d1, p1) = simple_stroke(&[(0.0, 0.0)]);
        let (d2, p2) = simple_stroke(&[(1.0, 1.0)]);
        let id1 = doc.insert_stroke(d1, p1);
        let _id2 = doc.insert_stroke(d2, p2);
        assert_eq!(doc.visible_stroke_ids().len(), 2);
        assert_eq!(doc.undo_depth(), 2);

        // Undo last insert (id2)
        let undone = doc.undo_last_stroke();
        assert!(undone.is_some());
        assert_eq!(doc.visible_stroke_ids(), vec![id1]);
        assert_eq!(doc.undo_depth(), 1);

        // Undo first insert (id1)
        let undone2 = doc.undo_last_stroke();
        assert!(undone2.is_some());
        assert!(doc.visible_stroke_ids().is_empty());
        assert_eq!(doc.undo_depth(), 0);

        // Nothing left to undo
        assert!(doc.undo_last_stroke().is_none());
    }

    #[test]
    fn undo_skips_remotely_deleted_strokes() {
        let mut doc = make_doc(1);
        doc.simplify_epsilon = 0.0;

        let (d, p) = simple_stroke(&[(0.0, 0.0)]);
        let id = doc.insert_stroke(d, p);
        assert_eq!(doc.undo_depth(), 1);

        // Simulate remote delete arriving before undo
        let del_op = Operation::DeleteStroke {
            id: OpId {
                lamport: LamportTs(99),
                actor: ActorId(2),
            },
            target: id,
        };
        doc.apply_remote(del_op);
        assert!(doc.visible_stroke_ids().is_empty());

        // Undo should silently skip the already-deleted stroke
        let result = doc.undo_last_stroke();
        assert!(result.is_none(), "nothing left to undo");
    }

    #[test]
    fn undo_generates_pending_delete_op() {
        let mut doc = make_doc(1);
        doc.simplify_epsilon = 0.0;

        let (d, p) = simple_stroke(&[(0.0, 0.0)]);
        doc.insert_stroke(d, p);
        let _ = std::mem::take(&mut doc.pending_ops); // flush insert

        doc.undo_last_stroke();
        let ops = std::mem::take(&mut doc.pending_ops);
        assert_eq!(ops.len(), 1, "undo should produce a DeleteStroke op");
        assert!(matches!(ops[0], Operation::DeleteStroke { .. }));
    }

    #[test]
    fn auto_simplify_enabled_by_default() {
        let mut doc = make_doc(1);
        // Default epsilon = 0.5; insert a straight line with many points.
        let pts: Box<[StrokePoint]> = (0..100)
            .map(|i| StrokePoint::basic(i as f32, i as f32))
            .collect();
        let data = StrokeData::new(pts, ToolKind::Pen);
        let props = StrokeProperties::new(0xFF0000FF, 2.0, 1.0, OpId::ZERO);
        let id = doc.insert_stroke(data, props);

        // Stroke stored with simplified points
        let (stored, _) = doc.get_stroke(&id).unwrap();
        assert!(
            stored.points.len() < 100,
            "auto-simplify should reduce points"
        );
        assert_eq!(stored.points.len(), 2, "straight line → 2 points");
    }

    #[test]
    fn auto_simplify_disabled() {
        let mut doc = make_doc(1);
        doc.simplify_epsilon = 0.0; // disabled

        let pts: Box<[StrokePoint]> = (0..100)
            .map(|i| StrokePoint::basic(i as f32, i as f32))
            .collect();
        let n = pts.len();
        let data = StrokeData::new(pts, ToolKind::Pen);
        let props = StrokeProperties::new(0xFF0000FF, 2.0, 1.0, OpId::ZERO);
        let id = doc.insert_stroke(data, props);

        let (stored, _) = doc.get_stroke(&id).unwrap();
        assert_eq!(stored.points.len(), n, "no simplification when epsilon=0");
    }
}
