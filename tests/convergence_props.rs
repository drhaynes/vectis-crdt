//! Property-based convergence tests using proptest.
//!
//! These tests encode the fundamental CRDT guarantees:
//!
//! 1. **Convergence**: Any two documents that have applied the same set of
//!    operations (in any order) must have identical visible state.
//!
//! 2. **Idempotency**: Applying an operation more than once is equivalent
//!    to applying it exactly once.
//!
//! 3. **Commutativity**: The order in which operations from different actors
//!    are applied does not affect the final state.
//!
//! 4. **Three-actor convergence**: Convergence holds for any number of actors
//!    (verified for 3 here).
//!
//! 5. **Delete commutativity**: A delete concurrent with an insert converges
//!    regardless of which arrives first.
//!
//! 6. **Causal buffer**: Out-of-order delivery still converges via the buffer.

use proptest::prelude::*;
use vectis_crdt::{
    causal_buffer::CausalBuffer,
    document::{Document, Operation},
    stroke::{StrokeData, StrokePoint, StrokeProperties, ToolKind},
    types::{ActorId, OpId},
};

// ─── Test helpers ─────────────────────────────────────────────────────────────

fn make_doc(actor: u64) -> Document {
    Document::new(ActorId(actor))
}

fn make_stroke(seed: u64) -> (StrokeData, StrokeProperties) {
    let pts: Box<[StrokePoint]> = vec![
        StrokePoint::new(seed as f32, 0.0, 1.0),
        StrokePoint::new(seed as f32 + 1.0, seed as f32 + 1.0, 0.8),
    ]
    .into();
    let data = StrokeData::new(pts, ToolKind::Pen);
    let props = StrokeProperties::new(0xFF0000FF, 2.0, 1.0, OpId::ZERO);
    (data, props)
}

/// Drain pending ops from `doc` into a Vec and return them.
fn drain_ops(doc: &mut Document) -> Vec<Operation> {
    doc.take_pending_ops()
}

/// Insert `count` strokes into `doc`, return all generated ops.
fn insert_n(doc: &mut Document, count: usize, seed_offset: u64) -> Vec<Operation> {
    let mut ops = Vec::new();
    for i in 0..count {
        let (data, props) = make_stroke(seed_offset + i as u64);
        doc.insert_stroke(data, props);
        ops.extend(drain_ops(doc));
    }
    ops
}

// ─── Property tests ───────────────────────────────────────────────────────────

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    /// P1 — Two-actor convergence.
    /// A and B apply their own ops locally, then exchange. Final state must match.
    #[test]
    fn prop_two_actors_converge(
        n_a in 1usize..10,
        n_b in 1usize..10,
    ) {
        let mut doc_a = make_doc(1);
        let mut doc_b = make_doc(2);

        let ops_a = insert_n(&mut doc_a, n_a, 0);
        let ops_b = insert_n(&mut doc_b, n_b, 1000);

        // Exchange in opposite orders
        for op in ops_b.clone() { doc_a.apply_remote(op); }
        for op in ops_a.clone() { doc_b.apply_remote(op); }

        let ids_a = doc_a.visible_stroke_ids();
        let ids_b = doc_b.visible_stroke_ids();
        prop_assert_eq!(ids_a, ids_b);
    }

    /// P2 — Idempotency.
    /// Applying the same ops twice yields the same visible state as applying once.
    #[test]
    fn prop_idempotent(n in 1usize..8) {
        let mut src = make_doc(1);
        let ops = insert_n(&mut src, n, 0);

        let mut doc = make_doc(99);
        for op in ops.clone() { doc.apply_remote(op.clone()); }
        let state_once = doc.visible_stroke_ids();

        for op in ops { doc.apply_remote(op); } // duplicate apply
        let state_twice = doc.visible_stroke_ids();

        prop_assert_eq!(state_once, state_twice);
    }

    /// P3 — Commutativity.
    /// Two orderings of the same ops produce identical state.
    #[test]
    fn prop_commutative(n_a in 1usize..8, n_b in 1usize..8) {
        let mut src_a = make_doc(1);
        let mut src_b = make_doc(2);

        let ops_a = insert_n(&mut src_a, n_a, 0);
        let ops_b = insert_n(&mut src_b, n_b, 1000);

        let order1: Vec<_> = ops_a.iter().chain(ops_b.iter()).cloned().collect();
        let order2: Vec<_> = ops_b.iter().chain(ops_a.iter()).cloned().collect();

        let mut doc1 = make_doc(99);
        let mut doc2 = make_doc(99);
        for op in order1 { doc1.apply_remote(op); }
        for op in order2 { doc2.apply_remote(op); }

        prop_assert_eq!(doc1.visible_stroke_ids(), doc2.visible_stroke_ids());
    }

    /// P4 — Three-actor convergence.
    #[test]
    fn prop_three_actors_converge(
        n_a in 1usize..6,
        n_b in 1usize..6,
        n_c in 1usize..6,
    ) {
        let mut src_a = make_doc(1);
        let mut src_b = make_doc(2);
        let mut src_c = make_doc(3);

        let ops_a = insert_n(&mut src_a, n_a, 0);
        let ops_b = insert_n(&mut src_b, n_b, 1000);
        let ops_c = insert_n(&mut src_c, n_c, 2000);

        let all: Vec<_> = ops_a.iter()
            .chain(ops_b.iter())
            .chain(ops_c.iter())
            .cloned()
            .collect();

        let mut docs: Vec<Document> = vec![make_doc(10), make_doc(11), make_doc(12)];
        // Each doc gets all ops in a different order
        let orders: [Vec<_>; 3] = [
            all.clone(),
            ops_b.iter().chain(ops_a.iter()).chain(ops_c.iter()).cloned().collect(),
            ops_c.iter().chain(ops_b.iter()).chain(ops_a.iter()).cloned().collect(),
        ];

        for (doc, order) in docs.iter_mut().zip(orders.iter()) {
            for op in order.clone() { doc.apply_remote(op); }
        }

        let ref_state = docs[0].visible_stroke_ids();
        prop_assert_eq!(&ref_state, &docs[1].visible_stroke_ids());
        prop_assert_eq!(&ref_state, &docs[2].visible_stroke_ids());
    }

    /// P5 — Delete commutativity.
    /// insert(A) then delete(A) converges whether delete or insert arrives first.
    #[test]
    fn prop_delete_converges(n_before in 0usize..4, n_after in 0usize..4) {
        let mut src = make_doc(1);

        // Insert some strokes before the target
        let ops_before = insert_n(&mut src, n_before, 0);

        // Insert target stroke
        let (data, props) = make_stroke(999);
        let target_id = src.insert_stroke(data, props);
        let insert_op = drain_ops(&mut src).remove(0);

        // Insert some strokes after
        let ops_after = insert_n(&mut src, n_after, 2000);

        // Delete the target
        src.delete_stroke(target_id);
        let delete_op = drain_ops(&mut src).remove(0);

        // Doc A: insert arrives before delete (normal order)
        let mut doc_a = make_doc(10);
        for op in ops_before.clone() { doc_a.apply_remote(op.clone()); }
        doc_a.apply_remote(insert_op.clone());
        for op in ops_after.clone() { doc_a.apply_remote(op.clone()); }
        doc_a.apply_remote(delete_op.clone());

        // Doc B: delete arrives before insert (reversed).
        // Use CausalBuffer so the delete waits for the insert to arrive.
        let mut doc_b = make_doc(11);
        let mut buf_b = CausalBuffer::new();
        doc_b.apply_remote_buffered(delete_op.clone(), &mut buf_b).unwrap();
        for op in ops_before.clone() { doc_b.apply_remote_buffered(op.clone(), &mut buf_b).unwrap(); }
        doc_b.apply_remote_buffered(insert_op.clone(), &mut buf_b).unwrap();
        for op in ops_after.clone() { doc_b.apply_remote_buffered(op.clone(), &mut buf_b).unwrap(); }

        prop_assert_eq!(doc_a.visible_stroke_ids(), doc_b.visible_stroke_ids());
        prop_assert!(!doc_a.visible_stroke_ids().contains(&target_id));
    }

    /// P6 — Causal buffer convergence.
    /// Out-of-order delivery via CausalBuffer yields same result as in-order.
    #[test]
    fn prop_causal_buffer_converges(n_a in 2usize..6, n_b in 1usize..6) {
        let mut src_a = make_doc(1);
        let mut src_b = make_doc(2);

        let ops_a = insert_n(&mut src_a, n_a, 0);
        let ops_b = insert_n(&mut src_b, n_b, 1000);

        // Reference: in-order apply
        let mut doc_ref = make_doc(99);
        for op in ops_a.iter().chain(ops_b.iter()).cloned() {
            doc_ref.apply_remote(op);
        }

        // Test: reversed ops go through causal buffer
        let mut doc_buf = make_doc(99);
        let mut buf = CausalBuffer::new();

        // Apply A's ops in REVERSE order (out-of-order)
        for op in ops_a.iter().rev().cloned() {
            doc_buf.apply_remote_buffered(op, &mut buf).unwrap();
        }
        // Apply B's ops normally
        for op in ops_b.iter().cloned() {
            doc_buf.apply_remote_buffered(op, &mut buf).unwrap();
        }

        prop_assert_eq!(buf.len(), 0);
        prop_assert_eq!(doc_ref.visible_stroke_ids(), doc_buf.visible_stroke_ids());
    }

    /// P7 — Snapshot roundtrip preserves visible state.
    #[test]
    fn prop_snapshot_roundtrip(n in 1usize..8) {
        let mut doc = make_doc(1);
        insert_n(&mut doc, n, 0);
        // Also delete one stroke
        if n > 1 {
            let ids = doc.visible_stroke_ids();
            doc.delete_stroke(ids[0]);
        }

        let snapshot = vectis_crdt::encoding::encode_snapshot(&doc);
        let restored = vectis_crdt::encoding::decode_snapshot(&snapshot, ActorId(99)).unwrap();

        prop_assert_eq!(
            doc.visible_stroke_ids(),
            restored.visible_stroke_ids()
        );
    }
}
