use crate::document::Document;
use crate::rga::ItemState;
use crate::stroke::StrokePoint;
use crate::types::OpId;
use std::collections::{HashMap, HashSet};

/// GC configuration. Tunable by the operator.
pub struct GcConfig {
    /// Tombstone ratio that triggers automatic GC.
    pub tombstone_ratio_threshold: f64,
    /// Absolute tombstone count that triggers GC.
    pub tombstone_count_threshold: usize,
    /// Max tombstones to process per GC cycle (prevents long pauses).
    pub max_gc_per_cycle: usize,
}

impl Default for GcConfig {
    fn default() -> Self {
        Self {
            tombstone_ratio_threshold: 0.30,
            tombstone_count_threshold: 10_000,
            max_gc_per_cycle: 5_000,
        }
    }
}

/// Result of a GC cycle.
#[derive(Debug, Clone)]
pub struct GcResult {
    pub tombstones_removed: usize,
    pub bytes_freed_estimate: usize,
    pub generation: u64,
    /// True if GC was cut short (more tombstones remain eligible).
    pub partial: bool,
}

/// Walk the `origin_left` chain from `origin` until reaching an ID NOT in
/// `remove_set`. Uses the index for O(1) per hop; bounded by `max_depth`
/// to guard against pathological inputs.
///
/// This is called during GC to re-parent surviving items whose `origin_left`
/// references a tombstone being erased. Without re-parenting, encoded
/// snapshots would contain dangling `origin_left` references, causing
/// z-order corruption on any peer that reconstructs from that snapshot.
fn find_kept_ancestor(
    mut origin: OpId,
    remove_set: &HashSet<OpId>,
    items: &[crate::rga::RgaItem],
    index: &HashMap<OpId, usize>,
) -> OpId {
    const MAX_DEPTH: usize = 10_000; // guard against cycles (shouldn't exist in valid RGA)
    for _ in 0..MAX_DEPTH {
        if !remove_set.contains(&origin) {
            return origin; // Found a kept ancestor.
        }
        match index.get(&origin) {
            Some(&pos) => origin = items[pos].origin_left,
            None => return OpId::ZERO, // Chain broken — attach to root.
        }
    }
    OpId::ZERO // Pathological depth: attach to root.
}

impl Document {
    /// Returns true if GC should run based on current thresholds.
    pub fn should_gc(&self, config: &GcConfig) -> bool {
        self.stroke_order.tombstone_ratio() > config.tombstone_ratio_threshold
            || self.stroke_order.tombstone_count >= config.tombstone_count_threshold
    }

    /// Runs an incremental GC cycle.
    ///
    /// SAFETY: Only removes tombstones that are causally stable —
    /// i.e., ALL known peers have seen the delete operation.
    /// This guarantees no future peer will need the tombstone to
    /// resolve conflicts.
    ///
    /// ## Re-parenting of origin references
    ///
    /// Before erasing GC'd tombstones, this method re-parents any surviving
    /// item whose `origin_left` (or `origin_right`) pointed to a GC'd ID.
    /// Re-parenting walks the chain to the nearest kept ancestor, so that
    /// encoded snapshots remain fully self-contained — no dangling references.
    ///
    /// Because re-parenting is deterministic (same MVV → same GC set across
    /// all peers), convergence is preserved: two peers that GC the same set
    /// produce identical re-parented states.
    ///
    /// ## Offline peers
    ///
    /// Peers offline longer than `gc_grace_period` must perform a full state
    /// sync on reconnect; their deltas may reference items already GC'd.
    pub fn run_gc(&mut self, config: &GcConfig) -> GcResult {
        let mut removed = 0;
        let mut bytes_freed: usize = 0;
        let mut ids_to_remove: Vec<OpId> = Vec::new();
        let mut partial = false;

        // ── Phase 1: Collect causally-stable tombstones ──────────────────────
        for item in self.stroke_order.items.iter() {
            if removed >= config.max_gc_per_cycle {
                partial = true;
                break;
            }

            if let ItemState::Tombstone { deleted_at } = &item.state {
                let is_stable = self.min_version.get(deleted_at.actor)
                    >= deleted_at.lamport.0;

                if is_stable {
                    ids_to_remove.push(item.id);
                    removed += 1;

                    // Estimate bytes freed.
                    bytes_freed += std::mem::size_of::<crate::rga::RgaItem>();
                    bytes_freed += 80; // HashMap entry overhead estimate
                    if let Some((data, _)) = self.stroke_store.strokes.get(&item.content) {
                        bytes_freed += data.points.len() * std::mem::size_of::<StrokePoint>();
                        bytes_freed += 128; // properties overhead estimate
                    }
                }
            }
        }

        if ids_to_remove.is_empty() {
            self.gc_generation += 1;
            return GcResult {
                tombstones_removed: 0,
                bytes_freed_estimate: 0,
                generation: self.gc_generation,
                partial,
            };
        }

        let remove_set: HashSet<OpId> = ids_to_remove.iter().copied().collect();

        // ── Phase 2: Re-parent surviving items ───────────────────────────────
        //
        // Compute new origin_left/right for any surviving item that references
        // a GC'd tombstone. We snapshot the current index before mutation so
        // that `find_kept_ancestor` can resolve chains without borrow issues.
        //
        // Doing this BEFORE the `retain` ensures the index is complete for
        // ancestor resolution.
        let reparents: Vec<(OpId, OpId, OpId)> = self
            .stroke_order
            .items
            .iter()
            .filter(|item| !remove_set.contains(&item.id)) // only surviving items
            .filter(|item| {
                remove_set.contains(&item.origin_left)
                    || (!item.origin_right.is_zero() && remove_set.contains(&item.origin_right))
            })
            .map(|item| {
                let new_ol = if remove_set.contains(&item.origin_left) {
                    find_kept_ancestor(
                        item.origin_left,
                        &remove_set,
                        &self.stroke_order.items,
                        &self.stroke_order.index,
                    )
                } else {
                    item.origin_left
                };
                let new_or = if !item.origin_right.is_zero()
                    && remove_set.contains(&item.origin_right)
                {
                    find_kept_ancestor(
                        item.origin_right,
                        &remove_set,
                        &self.stroke_order.items,
                        &self.stroke_order.index,
                    )
                } else {
                    item.origin_right
                };
                (item.id, new_ol, new_or)
            })
            .collect();

        // Apply re-parentings. The index is still intact here.
        for (id, new_ol, new_or) in reparents {
            if let Some(&pos) = self.stroke_order.index.get(&id) {
                self.stroke_order.items[pos].origin_left = new_ol;
                self.stroke_order.items[pos].origin_right = new_or;
            }
        }

        // ── Phase 3: Remove from StrokeStore and RGA ─────────────────────────
        for id in &ids_to_remove {
            self.stroke_store.remove(id);
        }

        self.stroke_order.items.retain(|item| !remove_set.contains(&item.id));

        // Full index rebuild required after retain (positions shifted).
        self.stroke_order.rebuild_index();

        self.stroke_order.tombstone_count =
            self.stroke_order.tombstone_count.saturating_sub(removed);
        self.stroke_order.total_count =
            self.stroke_order.total_count.saturating_sub(removed);

        self.gc_generation += 1;

        GcResult {
            tombstones_removed: removed,
            bytes_freed_estimate: bytes_freed,
            generation: self.gc_generation,
            partial,
        }
    }

    /// Update the Minimum Version Vector (MVV) and run GC if needed.
    pub fn update_min_version(
        &mut self,
        mvv: crate::types::VectorClock,
        config: &GcConfig,
    ) -> Option<GcResult> {
        self.min_version = mvv;
        if self.should_gc(config) {
            Some(self.run_gc(config))
        } else {
            None
        }
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;
    use crate::stroke::{StrokeData, StrokePoint, StrokeProperties, ToolKind};
    use crate::types::{ActorId, OpId, VectorClock};

    fn simple_doc() -> Document {
        let mut doc = Document::new(ActorId(1));
        doc.simplify_epsilon = 0.0; // disable simplification in GC tests
        doc
    }

    fn simple_stroke() -> (StrokeData, StrokeProperties) {
        let pts: Box<[StrokePoint]> = vec![StrokePoint::basic(0.0, 0.0)].into();
        let data = StrokeData::new(pts, ToolKind::Pen);
        let props = StrokeProperties::new(0xFF0000FF, 2.0, 1.0, OpId::ZERO);
        (data, props)
    }

    fn all_stable_mvv() -> VectorClock {
        let mut mvv = VectorClock::new();
        mvv.advance(ActorId(1), 1_000_000); // ahead of everything
        mvv
    }

    fn aggressive_gc() -> GcConfig {
        GcConfig {
            tombstone_ratio_threshold: 0.0,
            tombstone_count_threshold: 0,
            max_gc_per_cycle: 100_000,
        }
    }

    #[test]
    fn gc_removes_stable_tombstones() {
        let mut doc = simple_doc();
        let (data, props) = simple_stroke();
        let id = doc.insert_stroke(data, props);

        doc.delete_stroke(id);
        assert_eq!(doc.stroke_order.tombstone_count, 1);

        doc.min_version = all_stable_mvv();
        let result = doc.run_gc(&aggressive_gc());

        assert_eq!(result.tombstones_removed, 1);
        assert_eq!(doc.stroke_order.tombstone_count, 0);
        assert_eq!(doc.stroke_order.total_count, 0);
    }

    #[test]
    fn gc_does_not_remove_unstable_tombstones() {
        let mut doc = simple_doc();
        let (data, props) = simple_stroke();
        let id = doc.insert_stroke(data, props);
        doc.delete_stroke(id);

        doc.min_version = VectorClock::new(); // all zeros — nothing stable

        let result = doc.run_gc(&aggressive_gc());

        assert_eq!(result.tombstones_removed, 0);
        assert_eq!(doc.stroke_order.tombstone_count, 1);
    }

    /// Critical regression test: after GC removes a tombstone that is
    /// referenced as `origin_left` by a surviving stroke, encoding a snapshot
    /// and reconstructing from it must preserve the correct z-order.
    ///
    /// Without the re-parenting fix, the reconstructed stroke would be
    /// appended at the END of the z-order instead of after its true ancestor.
    #[test]
    fn gc_reparents_surviving_origin_references() {
        use crate::encoding::{decode_snapshot, encode_snapshot};

        let mut doc = simple_doc();
        doc.simplify_epsilon = 0.0;

        // Insert A, then B (B.origin_left = A), then C (C.origin_left = B).
        let (da, pa) = simple_stroke();
        let id_a = doc.insert_stroke(da, pa);

        let (db, pb) = simple_stroke();
        let id_b = doc.insert_stroke(db, pb);

        let (dc, pc) = simple_stroke();
        let id_c = doc.insert_stroke(dc, pc);

        // Delete B — it becomes a tombstone.
        doc.delete_stroke(id_b);

        // At this point: order is [A(active), B(tombstone), C(active)]
        // C.origin_left = B.id
        assert_eq!(doc.visible_stroke_ids(), vec![id_a, id_c]);

        // GC removes B (the tombstone). Without re-parenting, C's origin_left
        // becomes a dangling reference.
        doc.min_version = all_stable_mvv();
        let result = doc.run_gc(&aggressive_gc());
        assert_eq!(result.tombstones_removed, 1);

        // After GC: [A(active), C(active)], C re-parented to A.
        assert_eq!(doc.visible_stroke_ids(), vec![id_a, id_c]);

        // Encode a snapshot and reconstruct on a fresh doc.
        let snapshot = encode_snapshot(&doc);
        let doc2 = decode_snapshot(&snapshot, ActorId(99)).expect("snapshot decode failed");

        // Z-order must be preserved: A before C.
        let visible2 = doc2.visible_stroke_ids();
        assert_eq!(visible2.len(), 2);
        assert_eq!(visible2[0], id_a, "A must remain before C");
        assert_eq!(visible2[1], id_c, "C must follow A after reconstruction");
    }

    #[test]
    fn gc_reparent_chain_multiple_hops() {
        // A → B(deleted) → C(deleted) → D(active)
        // After GC of B and C, D should be re-parented to A.
        use crate::encoding::{decode_snapshot, encode_snapshot};

        let mut doc = simple_doc();
        doc.simplify_epsilon = 0.0;

        let (da, pa) = simple_stroke();
        let id_a = doc.insert_stroke(da, pa);
        let (db, pb) = simple_stroke();
        let id_b = doc.insert_stroke(db, pb);
        let (dc, pc) = simple_stroke();
        let id_c = doc.insert_stroke(dc, pc);
        let (dd, pd) = simple_stroke();
        let id_d = doc.insert_stroke(dd, pd);

        doc.delete_stroke(id_b);
        doc.delete_stroke(id_c);

        assert_eq!(doc.visible_stroke_ids(), vec![id_a, id_d]);

        doc.min_version = all_stable_mvv();
        let result = doc.run_gc(&aggressive_gc());
        assert_eq!(result.tombstones_removed, 2);
        assert_eq!(doc.visible_stroke_ids(), vec![id_a, id_d]);

        let snapshot = encode_snapshot(&doc);
        let doc2 = decode_snapshot(&snapshot, ActorId(99)).unwrap();
        let visible2 = doc2.visible_stroke_ids();
        assert_eq!(visible2, vec![id_a, id_d]);
    }
}
