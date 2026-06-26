//! Causal ordering buffer.
//!
//! In a distributed system, operations can arrive out of causal order.
//! Example: peer B inserts stroke X with `origin_left = Y.id`, but the
//! InsertStroke(Y) hasn't arrived yet. Applying X immediately would place
//! it at the wrong position (appended at the end instead of after Y).
//!
//! The CausalBuffer holds such "not yet ready" operations and re-tries them
//! each time a new operation is successfully integrated.
//!
//! ## Causal readiness rules
//!
//! | Operation        | Ready when                                            |
//! |------------------|-------------------------------------------------------|
//! | InsertStroke     | origin_left is ZERO or in RGA index                   |
//! | DeleteStroke     | target is in RGA index (insert was applied)           |
//! | UpdateProperty   | target is in StrokeStore (insert was applied)         |
//! | UpdateMetadata   | always ready                                          |

use crate::document::{Document, Operation};
use crate::error::{VectisError, VectisResult};
use crate::rga::StrokeId;
use crate::types::OpId;

/// Maximum number of buffered operations before we declare a broken peer.
const DEFAULT_MAX_CAPACITY: usize = 10_000;

/// Checks whether `op` can be applied to `doc` right now.
fn is_causally_ready(op: &Operation, doc: &Document) -> bool {
    match op {
        Operation::InsertStroke { origin_left, .. } => {
            origin_left.is_zero() || doc.stroke_order.index.contains_key(origin_left)
        }
        Operation::DeleteStroke { target, .. } => {
            // Apply even if it's already a tombstone (idempotent).
            doc.stroke_order.index.contains_key(target)
        }
        Operation::UpdateProperty { target, .. } => doc.stroke_store.contains(target),
        Operation::UpdateMetadata { .. } => true,
    }
}

/// Holds operations that cannot be applied yet due to unresolved
/// causal dependencies.
pub struct CausalBuffer {
    pending: Vec<Operation>,
    max_capacity: usize,
}

impl CausalBuffer {
    pub fn new() -> Self {
        Self {
            pending: Vec::new(),
            max_capacity: DEFAULT_MAX_CAPACITY,
        }
    }

    pub fn with_capacity(max_capacity: usize) -> Self {
        Self {
            pending: Vec::new(),
            max_capacity,
        }
    }

    /// Returns the number of buffered (pending) operations.
    #[inline]
    pub fn len(&self) -> usize {
        self.pending.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// Enqueues an operation. Returns `Err` if the buffer is full.
    pub fn push(&mut self, op: Operation) -> VectisResult<()> {
        if self.pending.len() >= self.max_capacity {
            return Err(VectisError::CausalBufferOverflow {
                capacity: self.max_capacity,
            });
        }
        self.pending.push(op);
        Ok(())
    }

    /// Drains all operations that are causally ready given the current
    /// document state. Leaves unready operations in the buffer.
    ///
    /// This must be called in a loop until it returns an empty Vec,
    /// because applying one ready op may unblock others.
    pub fn drain_ready(&mut self, doc: &Document) -> Vec<Operation> {
        let mut ready = Vec::new();
        let mut still_pending = Vec::new();

        for op in self.pending.drain(..) {
            if is_causally_ready(&op, doc) {
                ready.push(op);
            } else {
                still_pending.push(op);
            }
        }

        self.pending = still_pending;
        ready
    }

    /// Returns the OpIds of all operations still waiting in the buffer.
    /// Useful for diagnostics / debugging stuck ops.
    pub fn pending_ids(&self) -> Vec<OpId> {
        self.pending.iter().map(|op| op.id()).collect()
    }
}

impl Default for CausalBuffer {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Document integration ────────────────────────────────────────────────────

impl Document {
    /// Apply a remote operation through the causal buffer.
    ///
    /// Pushes `op` into the buffer, then repeatedly drains + applies
    /// ready operations until no more can be unblocked.
    ///
    /// Returns the StrokeIds that were changed (for incremental re-render).
    pub fn apply_remote_buffered(
        &mut self,
        op: Operation,
        buffer: &mut CausalBuffer,
    ) -> VectisResult<Vec<StrokeId>> {
        buffer.push(op)?;
        let mut changed: Vec<StrokeId> = Vec::new();

        loop {
            let ready = buffer.drain_ready(self);
            if ready.is_empty() {
                break;
            }
            for op in ready {
                if let Some(id) = self.apply_remote(op) {
                    if !changed.contains(&id) {
                        changed.push(id);
                    }
                }
            }
        }

        Ok(changed)
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{Document, Operation};
    use crate::stroke::{StrokeData, StrokePoint, StrokeProperties, ToolKind};
    use crate::types::{ActorId, OpId};

    fn make_doc(actor: u64) -> Document {
        Document::new(ActorId(actor))
    }

    fn simple_stroke() -> (StrokeData, StrokeProperties) {
        let pts: Box<[StrokePoint]> = vec![StrokePoint::basic(1.0, 1.0)].into();
        let data = StrokeData::new(pts, ToolKind::Pen);
        let props = StrokeProperties::new(0xFF000000, 2.0, 1.0, OpId::ZERO);
        (data, props)
    }

    #[test]
    fn buffered_delete_waits_for_insert() {
        let mut doc_src = make_doc(1);
        let (data, props) = simple_stroke();
        let stroke_id = doc_src.insert_stroke(data, props);
        let mut ops: Vec<Operation> = std::mem::take(&mut doc_src.pending_ops);
        // ops[0] = InsertStroke

        // Also produce a delete op
        doc_src.delete_stroke(stroke_id);
        let del_op = doc_src.pending_ops.remove(0); // DeleteStroke

        // Apply DELETE before INSERT to a fresh doc
        let mut doc = make_doc(99);
        let mut buf = CausalBuffer::new();

        // Delete arrives first — must be buffered
        let changed = doc.apply_remote_buffered(del_op, &mut buf).unwrap();
        assert!(changed.is_empty(), "delete can't apply without its insert");
        assert_eq!(buf.len(), 1, "delete must be buffered");

        // Now insert arrives — both should apply
        let insert_op = ops.remove(0);
        let _changed = doc.apply_remote_buffered(insert_op, &mut buf).unwrap();
        assert_eq!(buf.len(), 0, "buffer must be empty after insert");
        assert!(doc.visible_stroke_ids().is_empty(), "stroke deleted");
    }

    #[test]
    fn buffered_insert_waits_for_origin() {
        // B.origin_left = A.id, but A arrives after B
        let mut src = make_doc(1);
        let (data_a, props_a) = simple_stroke();
        let id_a = src.insert_stroke(data_a, props_a);
        let op_a = src.pending_ops.remove(0);

        let (data_b, props_b) = simple_stroke();
        src.insert_stroke(data_b, props_b);
        let op_b = src.pending_ops.remove(0); // origin_left = id_a

        let mut doc = make_doc(99);
        let mut buf = CausalBuffer::new();

        // B arrives first
        let changed = doc.apply_remote_buffered(op_b, &mut buf).unwrap();
        assert!(changed.is_empty());
        assert_eq!(buf.len(), 1);

        // A arrives — both should now apply
        let _changed = doc.apply_remote_buffered(op_a, &mut buf).unwrap();
        assert_eq!(buf.len(), 0);
        assert_eq!(doc.visible_stroke_ids().len(), 2);
        // A must come before B in z-order
        let ids = doc.visible_stroke_ids();
        assert_eq!(ids[0], id_a);
    }

    #[test]
    fn buffer_overflow_returns_error() {
        let mut buf = CausalBuffer::with_capacity(2);
        let mut src = make_doc(1);

        for _ in 0..3 {
            let (data, props) = simple_stroke();
            src.insert_stroke(data, props);
            let op = src.pending_ops.remove(0);
            let _ = buf.push(op); // we don't check each result here
        }

        // The 3rd push should fail
        let mut src2 = make_doc(2);
        let (data, props) = simple_stroke();
        src2.insert_stroke(data, props);
        let op = src2.pending_ops.remove(0);
        let result = buf.push(op);
        assert!(result.is_err());
    }
}
