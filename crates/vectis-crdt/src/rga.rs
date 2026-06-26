use crate::types::*;
use std::collections::HashMap;

/// The ID of a stroke — just the OpId of its insertion operation.
pub type StrokeId = OpId;

/// State of an RGA item. Critical for GC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemState {
    /// Visible and active.
    Active,
    /// Deleted but retained for convergence.
    /// `deleted_at` allows GC when causally stable.
    Tombstone { deleted_at: OpId },
}

/// One slot in the RGA. Represents a stroke's position in z-order.
///
/// Memory layout: ~80 bytes per item.
/// The `content` field is an OpId referencing the Stroke in StrokeStore,
/// NOT the stroke inline — keeps RGA compact regardless of stroke size.
#[derive(Debug, Clone)]
pub struct RgaItem {
    /// Unique identifier of this insertion operation.
    pub id: OpId,
    /// ID of the item to the LEFT at insert time (origin).
    /// OpId::ZERO if inserted at the beginning.
    pub origin_left: OpId,
    /// ID of the item to the RIGHT at insert time.
    /// OpId::ZERO if no right boundary.
    pub origin_right: OpId,
    /// Reference to content (StrokeId = OpId of the stroke insertion).
    pub content: StrokeId,
    /// Current state.
    pub state: ItemState,
}

impl RgaItem {
    #[inline]
    pub fn is_visible(&self) -> bool {
        matches!(self.state, ItemState::Active)
    }

    #[inline]
    pub fn is_tombstone(&self) -> bool {
        matches!(self.state, ItemState::Tombstone { .. })
    }
}

/// RGA Array: the ordered list of items.
/// Internally a `Vec<RgaItem>` maintained in document order.
///
/// Complexity:
///   - integrate: O(n) worst case, O(k) where k = concurrent conflicting ops
///   - mark_deleted: O(1) via index
///   - iterate visible: O(n) skipping tombstones
pub struct RgaArray {
    /// Items in document order (z-order of the whiteboard).
    /// `pub(crate)` — external mutation would desync `index`.
    pub(crate) items: Vec<RgaItem>,
    /// Index: OpId -> position in `items`. O(1) lookup by ID.
    /// `pub(crate)` — must stay consistent with `items`.
    pub(crate) index: HashMap<OpId, usize>,
    /// Count of tombstones (for GC trigger).
    pub(crate) tombstone_count: usize,
    /// Total items including tombstones.
    pub(crate) total_count: usize,
}

impl RgaArray {
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            index: HashMap::new(),
            tombstone_count: 0,
            total_count: 0,
        }
    }

    /// Integrates a new (possibly remote) item at the correct position.
    /// This is the CORE conflict-resolution algorithm.
    ///
    /// Rules:
    ///   1. Find the region bounded by [origin_left, origin_right].
    ///   2. Within that region, among items sharing the same origin_left,
    ///      insert BEFORE items with a higher OpId (higher OpId = further left).
    pub fn integrate(&mut self, item: RgaItem) {
        // Idempotent: skip if we've already seen this operation.
        if self.index.contains_key(&item.id) {
            return;
        }

        // Step 1: Find left boundary index.
        let scan_start = if item.origin_left.is_zero() {
            0
        } else {
            match self.index.get(&item.origin_left) {
                Some(&left_idx) => left_idx + 1,
                // origin_left not yet applied — append at end for now;
                // caller must ensure causal ordering before calling integrate.
                None => self.items.len(),
            }
        };

        // Step 2: Find right boundary index.
        let scan_end = if item.origin_right.is_zero() {
            self.items.len()
        } else {
            match self.index.get(&item.origin_right) {
                Some(&right_idx) => right_idx,
                None => self.items.len(),
            }
        };

        // Step 3: Scan between left and right to find exact insert position.
        // YATA-style: skip over items whose origin_left is to the RIGHT of ours
        // ("right subtree" items). Only stop scanning when we hit an item whose
        // origin_left is strictly to the LEFT of ours.
        //
        // Within the same origin zone, higher OpId goes FIRST (left).
        let origin_left_pos: isize = if item.origin_left.is_zero() {
            -1
        } else {
            self.index
                .get(&item.origin_left)
                .map(|&i| i as isize)
                .unwrap_or(-1)
        };

        let mut insert_pos = scan_start;

        for i in scan_start..scan_end {
            let existing = &self.items[i];

            let existing_ol_pos: isize = if existing.origin_left.is_zero() {
                -1
            } else {
                self.index
                    .get(&existing.origin_left)
                    .map(|&p| p as isize)
                    .unwrap_or(-1)
            };

            if existing_ol_pos < origin_left_pos {
                // existing.origin_left is strictly LEFT of ours → we've passed our zone.
                break;
            } else if existing_ol_pos > origin_left_pos {
                // existing.origin_left is to the RIGHT of ours (right subtree) → skip it.
                insert_pos = i + 1;
            } else {
                // Same origin zone: higher OpId goes further left.
                if existing.id > item.id {
                    insert_pos = i + 1;
                } else {
                    break;
                }
            }
        }

        // Step 4: Insert and update index.
        let is_tomb = item.is_tombstone();
        self.items.insert(insert_pos, item);

        // Shift all index entries at insert_pos and beyond.
        self.rebuild_index_from(insert_pos);

        self.total_count += 1;
        if is_tomb {
            self.tombstone_count += 1;
        }
    }

    /// Marks an item as a tombstone. O(1) via index.
    /// Returns true if the item was found and was previously active.
    pub fn mark_deleted(&mut self, target: OpId, deleted_at: OpId) -> bool {
        if let Some(&idx) = self.index.get(&target)
            && self.items[idx].is_visible()
        {
            self.items[idx].state = ItemState::Tombstone { deleted_at };
            self.tombstone_count += 1;
            return true;
        }
        false
    }

    /// Iterator over visible (non-tombstone) items.
    pub fn visible_items(&self) -> impl Iterator<Item = &RgaItem> {
        self.items.iter().filter(|item| item.is_visible())
    }

    /// Tombstone ratio (0.0 – 1.0). Used to trigger GC.
    pub fn tombstone_ratio(&self) -> f64 {
        if self.total_count == 0 {
            return 0.0;
        }
        self.tombstone_count as f64 / self.total_count as f64
    }

    /// Rebuild the index for all positions from `from` onwards.
    /// Called after every insert.
    fn rebuild_index_from(&mut self, from: usize) {
        for i in from..self.items.len() {
            self.index.insert(self.items[i].id, i);
        }
    }

    /// Full index rebuild. Called after GC batch removes.
    pub(crate) fn rebuild_index(&mut self) {
        self.index.clear();
        for (i, item) in self.items.iter().enumerate() {
            self.index.insert(item.id, i);
        }
    }
}

impl Default for RgaArray {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_item(lamport: u64, actor: u64, ol: OpId, or_: OpId) -> RgaItem {
        let id = OpId {
            lamport: LamportTs(lamport),
            actor: ActorId(actor),
        };
        RgaItem {
            id,
            origin_left: ol,
            origin_right: or_,
            content: id,
            state: ItemState::Active,
        }
    }

    #[test]
    fn basic_insert_order() {
        let mut arr = RgaArray::new();
        let a = make_item(1, 1, OpId::ZERO, OpId::ZERO);
        let b = make_item(2, 1, a.id, OpId::ZERO);
        arr.integrate(a.clone());
        arr.integrate(b.clone());
        let visible: Vec<_> = arr.visible_items().map(|i| i.id).collect();
        assert_eq!(visible, vec![a.id, b.id]);
    }

    #[test]
    fn concurrent_insert_convergence() {
        // α and β both insert after A, concurrently.
        let mut arr_alpha = RgaArray::new();
        let mut arr_beta = RgaArray::new();

        let a = make_item(1, 1, OpId::ZERO, OpId::ZERO);
        arr_alpha.integrate(a.clone());
        arr_beta.integrate(a.clone());

        // α inserts at (L=2, actor=1), β inserts at (L=2, actor=2) — same Lamport.
        let stroke_alpha = make_item(2, 1, a.id, OpId::ZERO);
        let stroke_beta = make_item(2, 2, a.id, OpId::ZERO);

        // Apply in different orders.
        arr_alpha.integrate(stroke_alpha.clone());
        arr_alpha.integrate(stroke_beta.clone());

        arr_beta.integrate(stroke_beta.clone());
        arr_beta.integrate(stroke_alpha.clone());

        // Both should converge to [a, beta, alpha] — higher OpId (actor=2) goes left.
        let ids_alpha: Vec<_> = arr_alpha.visible_items().map(|i| i.id).collect();
        let ids_beta: Vec<_> = arr_beta.visible_items().map(|i| i.id).collect();
        assert_eq!(ids_alpha, ids_beta);
        assert_eq!(ids_alpha[0], a.id);
        assert_eq!(ids_alpha[1], stroke_beta.id); // actor=2 > actor=1 → left
        assert_eq!(ids_alpha[2], stroke_alpha.id);
    }

    #[test]
    fn tombstone_mark() {
        let mut arr = RgaArray::new();
        let a = make_item(1, 1, OpId::ZERO, OpId::ZERO);
        arr.integrate(a.clone());
        assert_eq!(arr.tombstone_count, 0);
        let del_id = OpId {
            lamport: LamportTs(2),
            actor: ActorId(1),
        };
        assert!(arr.mark_deleted(a.id, del_id));
        assert_eq!(arr.tombstone_count, 1);
        assert_eq!(arr.visible_items().count(), 0);
    }

    #[test]
    fn idempotent_integrate() {
        let mut arr = RgaArray::new();
        let a = make_item(1, 1, OpId::ZERO, OpId::ZERO);
        arr.integrate(a.clone());
        arr.integrate(a.clone()); // duplicate — should be ignored
        assert_eq!(arr.total_count, 1);
    }
}
