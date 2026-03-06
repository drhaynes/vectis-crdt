# vectis-crdt — Technical Architecture

> **vectis** (lat.) = arrow, vector. A CRDT engine for vector strokes on real-time collaborative whiteboards.

**Documented version:** v1.0
**Tests:** 53 passing (46 unit + 7 proptest)
**Build:** `cargo build --release` → ~85 KB Wasm gzipped

---

## Table of Contents

1. [Problem and context](#1-problem-and-context)
2. [Why RGA/YATA and not the alternatives](#2-why-rgayata-and-not-the-alternatives)
3. [Base types: ActorId, LamportTs, OpId, VectorClock](#3-base-types)
4. [RGA Array: insertion, integration and conflicts](#4-rga-array)
5. [YATA: the convergence algorithm step by step](#5-yata-convergence)
6. [Causal delivery: CausalBuffer](#6-causal-delivery-causalbuffer)
7. [Data model: Document, StrokeStore, RgaArray](#7-data-model)
8. [LWW-Register and LWW-Map for mutable properties](#8-lww-register-and-lww-map)
9. [Delta synchronization: VectorClock diff](#9-delta-synchronization)
10. [Incremental Garbage Collection with re-parenting](#10-garbage-collection)
11. [RDP: stroke simplification](#11-rdp-stroke-simplification)
12. [AABB and viewport culling](#12-aabb-and-viewport-culling)
13. [Binary wire format](#13-binary-wire-format)
14. [Wasm bridge: zero-copy render](#14-wasm-bridge-zero-copy-render)
15. [Awareness: ephemeral cursors](#15-awareness-ephemeral-cursors)
16. [Local undo](#16-local-undo)
17. [Testing strategy](#17-testing-strategy)
18. [Formal guarantees and wire format policy](#18-formal-guarantees-and-wire-format-policy)
19. [Defensive limits](#19-defensive-limits)
20. [Public API encapsulation](#20-public-api-encapsulation)
21. [Known limitations](#21-known-limitations)
22. [Roadmap](#22-roadmap)

---

## 1. Problem and context

A real-time collaborative whiteboard has three conflicting requirements:

1. **Local responsiveness**: every stylus touch must appear on screen immediately, without waiting for server confirmation. The UI cannot block on the network.
2. **Eventual convergence**: two clients that apply the same set of operations in any order must arrive at the same state. The z-order of strokes (which one is on top) must be deterministic across all nodes.
3. **Efficiency**: a stylus at 240 Hz generates up to 14,400 points per minute. The system must encode, transmit, and render this with minimal latency and memory.

CRDTs (Conflict-free Replicated Data Types) satisfy requirements (1) and (2) mathematically: local operations are applied instantly and convergence is guaranteed by construction, without coordination. The challenge is satisfying (3) at the same time.

**Central decision**: the whiteboard state is modeled as two independent CRDT structures:

- **RGA (Replicated Growable Array)** for stroke z-order: the relative ordering of strokes (which is in front) requires a sequence CRDT.
- **LWW-Register (Last-Write-Wins Register)** per mutable stroke property (color, width, opacity, transform): properties have no structural conflicts — last writer wins.

---

## 2. Why RGA/YATA and not the alternatives

### Operational Transformation (OT)

OT was the original approach in Google Docs. It requires every operation to be *transformed* against all concurrent operations before being applied. The problems:

- The correct transformation algorithm is notoriously hard to implement — there are dozens of papers with subtle bugs.
- Centralized OT requires a server to serialize operations and distribute transformations. Adding multiple servers requires even more complex transformation algorithms (multi-master OT).
- For fast-moving whiteboards (Apple Pencil at 120 Hz), the latency of centralized transformation is unacceptable.

### LOGOOT / LSEQ

P2P structures where each character receives a permanent fractional position in global space. Advantage: no tombstones — deletes are immediate. Disadvantage:

- Positions grow in size with document history → O(log n) or larger identifiers.
- For whiteboards with millions of points, identifier overhead eclipses the actual data.

### Automerge / Yjs

These are mature general-purpose CRDTs. Automerge is written in JS/Rust; Yjs in JS with a Rust binding. They are excellent but:

- Generality has a cost: for a whiteboard we need specific semantics (z-order per stroke, not per point) that are not directly representable in a generic character array.
- They bundle ~80 KB of library that includes functionality (collaborative text, nested maps) we don't need.
- We have no control over the wire format — critical for minimizing stroke point payloads.

### RGA + YATA (chosen)

RGA (Roh et al., 2011) models the document as a sequence where each item knows who its left neighbor was at the time of insertion (`origin_left`). The conflict is resolved locally, without coordination: if two items are inserted with the same `origin_left`, the tiebreak by OpId is deterministic.

**YATA** (Nicolaescu et al., 2016) refines RGA by adding `origin_right` and a skip rule for the "right subtree" that guarantees convergence even with complex interleaved operations.

Advantages for this use case:
- **Tight domain**: the item is a complete stroke (not a point), so n is small (hundreds of strokes, not millions).
- **Custom wire format**: full control over the encoding of the N points per stroke.
- **No external CRDT dependencies**: all logic is in ~800 auditable lines of Rust.

---

## 3. Base types

### `ActorId(u64)` — `src/types.rs`

Unique client identifier. u64 instead of UUID (128 bits) for two reasons:
1. **Wire size**: in LEB128, an ActorId < 2^56 fits in ≤ 8 bytes vs. 16 for a UUID.
2. **Ordering**: u64 has `Ord`, making it useful as a tiebreaker in OpId without special comparators.

The server assigns the ActorId on first handshake. The client persists it (e.g. in localStorage) for reconnections.

### `LamportTs(u64)` — `src/types.rs`

Lamport logical clock. Rules:
- `tick()`: the client increments the counter before each local operation.
- `merge(remote)`: on receiving a remote operation with timestamp `t`, update to `max(local, t)`.

Guarantee: if operation A causally precedes B, then `lamport(A) < lamport(B)`. The inverse is NOT necessarily true (equal timestamps are possible for concurrent operations).

```
LamportTs::tick()  → self.0 += 1; return self
LamportTs::merge() → if other.0 > self.0 { self.0 = other.0 }
```

### `OpId { lamport: LamportTs, actor: ActorId }` — `src/types.rs`

Globally unique operation identifier. Uniqueness comes from the combination (lamport, actor): two different actors that generate operations with the same Lamport have different ActorIds.

**Deterministic total order** (implemented as `Ord`):

```
OpId::cmp: lamport ASC → actor ASC (on tie)
```

This means: higher Lamport = greater OpId. On Lamport tie, higher ActorId = greater OpId. This **total and deterministic** order is what the YATA algorithm uses to resolve conflicts (without it, convergence would not be guaranteed).

`OpId::ZERO` = `{lamport: 0, actor: 0}` serves as a sentinel "no origin" (beginning of the sequence).

### `VectorClock` — `src/types.rs`

Map `ActorId → max_lamport_seen`. Uses:
- **Causal consistency**: before applying an operation, verify its `origin_left` has already been applied (`causal_buffer.rs`).
- **Delta sync**: `diff(other)` computes what operation ranges `self` has that `other` does not.
- **GC eligibility**: the document's `min_version` is the global minimum of all known vector clocks; an operation is "causally stable" when all peers have seen it.

```rust
// Complexity:
VectorClock::advance   → O(1) amortized (BTreeMap insert)
VectorClock::dominates → O(k) where k = number of actors
VectorClock::diff      → O(k)
VectorClock::merge     → O(k)
```

---

## 4. RGA Array

`RgaArray` in `src/rga.rs` is the sequence structure that maintains the z-order of strokes.

### Internal structure

```
items: Vec<RgaItem>           // actual document order
index: HashMap<OpId, usize>   // OpId → position in items
tombstone_count: usize
total_count: usize
```

Each `RgaItem` contains:
```
id:           OpId       // identity of this insertion
origin_left:  OpId       // left neighbor at the time of insertion
origin_right: OpId       // right neighbor at the time of insertion
content:      StrokeId   // reference to the stroke (same OpId as id)
state:        ItemState  // Active | Tombstone { deleted_at: OpId }
```

**RGA/StrokeStore separation**: the RGA contains only references (OpId, 16 bytes) to strokes, not the point data. This keeps the ordering structure compact and independent of stroke size.

### Complexity

| Operation | Average case | Worst case |
|-----------|-------------|------------|
| `integrate` | O(k) where k = concurrent ops | O(n) |
| `mark_deleted` | O(1) via index | O(1) |
| `visible_items` iterator | O(n) | O(n) |
| `rebuild_index_from(pos)` | O(n-pos) | O(n) |
| `rebuild_index` (full) | O(n) | O(n) |

### Why `Vec` instead of a tree

A balanced tree (`BTreeMap`, skip list) would give O(log n) for inserts. However:

1. **n is small**: a reasonably full whiteboard has ~500–5000 visible strokes, not millions.
2. **Cache locality**: `Vec<RgaItem>` is contiguous in memory. Iteration during render (traversing all items in order) is extremely cache-efficient. A tree introduces pointers and cache misses.
3. **GC simplification**: `retain()` over Vec is O(n) and very efficient with SIMD.

For n < 10,000, Vec outperforms trees in real benchmarks due to cache locality.

---

## 5. YATA convergence

### The conflict problem

Alice inserts stroke X after stroke A. Bob inserts stroke Y also after stroke A, concurrently. Both have `origin_left = A.id`.

Without a tiebreak, the result depends on arrival order: if Alice receives Y first, she sees [A, Y, X]; if Bob receives X first, he sees [A, X, Y]. **They don't converge.**

### The YATA rule

`RgaArray::integrate` in `src/rga.rs:86`:

```
1. Find the position of origin_left (scan_start)
2. Find the position of origin_right (scan_end)
3. Scan [scan_start, scan_end):
   - If existing.origin_left is STRICTLY LEFT of ours → break (we've passed our zone)
   - If existing.origin_left is to the RIGHT of ours ("right subtree") → skip (insert_pos = i+1)
   - Same origin zone → compare OpIds: if existing.id > item.id → insert_pos = i+1 (existing goes first)
4. Insert at insert_pos
```

**The critical fix** (lines 140-142 in `rga.rs`):

```rust
} else if existing_ol_pos > origin_left_pos {
    // existing.origin_left is to the RIGHT of ours → skip (do NOT break)
    insert_pos = i + 1;
```

In the original RGA without this fix, this case did `break`, causing the item to be inserted too far left depending on arrival order. The YATA correction says: if the existing item belongs to the "right subtree" (its origin is further right than ours), we must skip it — it has the right to be further right than us.

### Convergence example with YATA

```
Initial state: [A]

Alice: InsertStroke(X, origin_left=A, OpId={L=2, actor=1})
Bob:   InsertStroke(Y, origin_left=A, OpId={L=2, actor=2})
# Same L and same origin_left → conflict
# OpId(L=2, actor=2) > OpId(L=2, actor=1) → Y goes first

At Alice (receives Y after X):
  state = [A, X]
  integrate(Y): scan_start=1, scan_end=2
    i=1: existing=X, existing_ol_pos = pos(A), origin_left_pos = pos(A) → same zone
         X.id={2,1} vs Y.id={2,2}: X.id < Y.id → break (Y goes before X)
  insert_pos = 1 → [A, Y, X] ✓

At Bob (receives X after Y):
  state = [A, Y]
  integrate(X): scan_start=1, scan_end=2
    i=1: existing=Y, existing_ol_pos = pos(A), origin_left_pos = pos(A) → same zone
         Y.id={2,2} > X.id={2,1} → insert_pos = 2 (Y goes first)
  insert_pos = 2 → [A, Y, X] ✓
```

Both converge to [A, Y, X] regardless of arrival order.

### Idempotency

`integrate` checks `self.index.contains_key(&item.id)` at the start (line 88). If the item already exists, it does nothing. This makes `apply_remote` safe with retries and reconnections.

---

## 6. Causal delivery: CausalBuffer

`src/causal_buffer.rs`

### Why it's necessary

Imagine Bob inserted B after A: `B.origin_left = A.id`. If B's operation arrives at Alice before A's, the RGA cannot place B in its correct position (it doesn't know where A is). Without a causal buffer, B would be appended at the end or dropped.

### Design

The buffer stores operations that cannot yet be applied due to unresolved causal dependencies. On each call to `apply_remote_buffered(op)`:

1. Push `op` into the pending buffer.
2. Loop: drain all operations that are now causally ready (their dependencies exist in the document) and apply them.
3. Repeat until no more ops can be unblocked.

Readiness rules:

| Operation | Ready when |
|-----------|-----------|
| `InsertStroke` | `origin_left` is ZERO or present in the RGA index |
| `DeleteStroke` | target is present in the RGA index |
| `UpdateProperty` | target is present in the StrokeStore |
| `UpdateMetadata` | always ready |

**Termination guarantee**: since ops form a causal DAG (no cycles), propagation always terminates.

---

## 7. Data model

### `Document` — `src/document.rs`

```
Document {
    // Identity
    local_actor: ActorId
    clock:       LamportTs

    // Causal tracking
    version:     VectorClock    // what we've seen from each actor

    // CRDT structures
    stroke_order: RgaArray       // z-order (which stroke is in front)
    stroke_store: StrokeStore    // HashMap<StrokeId, (StrokeData, StrokeProperties)>
    metadata:     LwwMap         // viewport, grid config, etc.

    // Op log
    pending_ops:  Vec<Operation> // local ops pending send to server

    // GC
    min_version:  VectorClock    // MVV: global minimum of all peers
    gc_generation: u64

    // Local UX
    undo_stack:       Vec<StrokeId>  // capped at MAX_UNDO_DEPTH=200
    simplify_epsilon: f32            // default 0.5
}
```

### Local operation flow

```
insert_stroke(data, props)
  1. if simplify_epsilon > 0 and points.len() > 2 → data.simplify(epsilon)
  2. next_op_id() → tick Lamport, advance version
  3. origin_left = last visible item in RGA
  4. stroke_order.integrate(RgaItem)
  5. stroke_store.insert(id, data, props)
  6. pending_ops.push(InsertStroke{...})
  7. undo_stack.push(id)  // with cap
  → returns StrokeId
```

### Remote operation flow

```
apply_remote(op)
  1. Merge Lamport clock with op timestamp
  2. Advance version with op actor/lamport
  3. match op:
     InsertStroke → stroke_order.integrate + stroke_store.insert (if not exists)
     DeleteStroke → stroke_order.mark_deleted (tombstone, do NOT delete from store)
     UpdateProperty → stroke_store props LWW apply
     UpdateMetadata → metadata LWW apply
  4. return Option<StrokeId> affected (for incremental re-render)
```

### Why separate `stroke_order` from `stroke_store`

The RGA needs to iterate over N items in order to resolve conflicts. If items contained stroke points (potentially thousands of f32), the working set in cache during `integrate` would be enormous. Separating the RGA (16 bytes/item) from the store (full data) keeps the RGA in L1/L2 cache during integration.

---

## 8. LWW-Register and LWW-Map

### `LwwRegister<T>` — `src/stroke.rs`

```rust
pub struct LwwRegister<T: Clone> {
    pub value: T,
    pub timestamp: OpId,
}

fn apply(&mut self, value: T, timestamp: OpId) -> bool {
    if timestamp > self.timestamp {
        self.value = value;
        self.timestamp = timestamp;
        true
    } else { false }
}
```

Semantics: the write with the greatest OpId wins. Since OpId has a deterministic total order, convergence is guaranteed: all peers that apply the same writes in any order arrive at the same value.

**Limitation**: LWW does not preserve intermediate operations. If Alice writes color=red at t=5 and Bob writes color=blue at t=5 with a higher actorId, Alice's red is lost even if it was "the last" from Alice's perspective. This is standard LWW semantics, acceptable for aesthetic stroke properties.

### Stroke properties as independent LWW registers

`StrokeProperties` contains four independent LWW-Registers: `color`, `stroke_width`, `opacity`, `transform`. Each has its own timestamp.

**Rationale**: if they were a single atomic register, a concurrent color + width modification by two different users would discard one of the changes entirely. With independent registers, each property is resolved autonomously: both changes survive.

### `LwwMap<K, V>` — `src/document.rs`

A map where each key has its own LWW-Register. Used for canvas metadata (viewport, grid). Deleting a key is implemented as `set(key, None, ts)` — the register stores `Option<V>`.

---

## 9. Delta synchronization

### Initial state: full snapshot

On first connection (or after a long disconnection), the server sends a full snapshot:

```
encode_snapshot(doc) → bytes
  [version: u8 = 1]
  varint(actor_id)
  varint(lamport)
  length_prefixed(encode_state_vector(doc.version))
  encode_update(all_ops)  // InsertStroke + DeleteStroke for tombstones
```

The snapshot serializes state as a sequence of ops, not a binary struct dump. This is crucial: it allows a peer with a different code version to reconstruct state by replaying ops, rather than deserializing structs that might not match.

### Incremental synchronization

For incremental updates, the client sends its `VectorClock` to the server. The server computes `diff = server_version.diff(client_version)` and sends only the ops the client doesn't have.

```rust
VectorClock::diff(&self, other: &VectorClock) -> Vec<(ActorId, u64, u64)>
// Returns ranges (actor, from_lamport, to_lamport) that self has but other does not
```

Reconstruction is idempotent: if the client already has the op (due to reconnection), the RGA ignores it via the idempotency check.

---

## 10. Garbage Collection

The RGA needs to retain tombstones because a disconnected peer might send an operation that references a deleted item. If the item no longer exists in the RGA, the insert cannot be placed in the correct position.

### When is it safe to delete a tombstone?

A tombstone with `deleted_at = {lamport: T, actor: A}` is "causally stable" when all known peers have advanced their clock past T for actor A. At that point, no future peer can send an operation that causally depended on that tombstone.

The server maintains the **Minimum Version Vector (MVV)**: the pointwise minimum of all known vector clocks. An op is causally stable when `MVV[actor] >= lamport`.

```rust
// In gc.rs, Phase 1:
let is_stable = self.min_version.get(deleted_at.actor) >= deleted_at.lamport.0;
```

### The critical GC bug without re-parenting

**Scenario without fix**: there are three strokes A → B(active) → C(active), where `C.origin_left = B.id`. B is deleted (tombstone). GC removes B when stable. When serializing the snapshot, `C.origin_left` points to an ID that no longer exists in the RGA. When reconstructing the snapshot on another peer, `decode_snapshot` applies ops in order: when InsertStroke(C, origin_left=B) arrives, B doesn't exist yet (it's not in the snapshot because it was GC'd), so C is inserted at the end of the array. Z-order is corrupted.

**Fix: re-parenting before retain** (`gc.rs:145–187`):

```
Phase 2 (before retain):
  For each surviving item whose origin_left is in remove_set:
    new_ol = find_kept_ancestor(item.origin_left, remove_set, items, index)
    apply new_ol to item
```

`find_kept_ancestor` walks the `origin_left` chain until it finds an ID not in `remove_set`. If the entire chain is being deleted (e.g. A→B→C with A, B in remove_set and C surviving), C is re-parented to `OpId::ZERO` (document root).

**Determinism**: re-parenting is deterministic across peers because:
1. The MVV is broadcast by the server — all peers that receive it have the same MVV.
2. The same MVV produces the same `remove_set` (same tombstones are stable).
3. The same `remove_set` produces the same re-parenting.

Therefore, two peers that GC the same set of tombstones produce exactly the same re-parented state.

### GC configuration

```rust
GcConfig {
    tombstone_ratio_threshold: 0.30,   // triggers if >30% are tombstones
    tombstone_count_threshold: 10_000, // or if there are >10k absolute tombstones
    max_gc_per_cycle: 5_000,           // max tombstones removed per cycle
}
```

Incremental GC (`max_gc_per_cycle`) prevents long pauses in documents with a lot of history. If there are more eligible tombstones, they are processed in the next cycle.

### Offline peers

A peer disconnected longer than the `gc_grace_period` may have references to already GC'd items. On reconnection, it must receive a full snapshot instead of a delta, since the referenced items no longer exist.

---

## 11. RDP: stroke simplification

### Motivation

An Apple Pencil at 240 Hz drawing for 2 seconds generates ~480 points. A nearly straight line of those 480 points can be represented with just 2–5 points without perceptible visual loss at any typical zoom level. Transmitting 480 points vs. 5 points is a ~96% difference in wire payload.

### Ramer-Douglas-Peucker (RDP)

The classic polyline simplification algorithm (Ramer 1972, Douglas-Peucker 1973):

```
rdp(points[start..end], epsilon):
  1. Draw a straight segment between points[start] and points[end]
  2. Find the point with maximum perpendicular distance to the segment
  3. If max_dist > epsilon: mark that point as "keep"
     and apply recursively to [start..max] and [max..end]
  4. If max_dist <= epsilon: all intermediate points are dispensable
```

**Why iterative and not recursive** (`stroke.rs:144`): for very long strokes (10,000+ points captured at 240 Hz), deep recursion would cause a stack overflow. The implementation uses an explicit stack:

```rust
let mut stack: Vec<(usize, usize)> = Vec::with_capacity(64);
stack.push((0, n - 1));
while let Some((start, end)) = stack.pop() {
    // find max_dist in [start..end]
    if max_dist > epsilon {
        stack.push((start, max_idx));
        stack.push((max_idx, end));
    }
}
```

The stack grows to O(log n) in the typical case (smooth polyline), O(n) in the pathological case (extreme zigzag). No risk of system stack overflow.

### Perpendicular distance

```rust
fn perp_dist(p: &StrokePoint, a: &StrokePoint, b: &StrokePoint) -> f32 {
    // ||(b−a) × (a−p)|| / ||b−a||
    // For degenerate segment (a==b): point-to-point distance
}
```

Only uses x, y coordinates (not pressure). Pressure is not simplified — it is preserved only at the kept points.

### Default epsilon = 0.5

0.5 pixels of maximum deviation in canvas coordinates is sub-pixel: invisible even on high-density displays (Retina, Surface Pro). Typical reduction for a real stroke:

| Stroke type | Original pts | With epsilon=0.5 | Reduction |
|-------------|-------------|------------------|-----------|
| Straight line 500 pts | 500 | 2 | 99.6% |
| Smooth curve 500 pts | 500 | 25–60 | ~90% |
| Zigzag 500 pts | 500 | 100–200 | ~70% |
| Calligraphic signature 500 pts | 500 | 80–150 | ~75% |

### Auto-simplification on insert

`Document::insert_stroke` automatically calls `data.simplify(self.simplify_epsilon)` if the stroke has more than 2 points. Simplification happens before saving to the store and before emitting the network op — the remote peer receives the already-simplified stroke.

Disable: `doc.simplify_epsilon = 0.0`.

---

## 12. AABB and viewport culling

### Motivation

Rendering all document strokes on every frame is O(total). With a small viewport over a large canvas, most strokes are out of view. Culling reduces render work to O(visible).

### `Aabb` — `src/stroke.rs`

```rust
pub struct Aabb {
    pub min_x: f32, pub min_y: f32,
    pub max_x: f32, pub max_y: f32,
}
```

Computed once in `StrokeData::new()` and updated after `simplify()`. **Not serialized to wire**: it is fully derivable from points, so it is recomputed on decode (`decode_stroke_data_at` uses `StrokeData::new()` which calls `Aabb::from_points`).

### Viewport intersection

```rust
// O(1)
fn intersects(&self, other: &Aabb) -> bool {
    self.min_x <= other.max_x && self.max_x >= other.min_x &&
    self.min_y <= other.max_y && self.max_y >= other.min_y
}
```

### Stroke width padding

The AABB of a stroke is tight (no padding). To avoid culling strokes that are partially at the viewport edge, it is expanded by `stroke_width/2` before the intersection test:

```rust
data.bounds.expanded(stroke_expand)  // stroke_expand = stroke_width/2
```

### Strokes with transforms

If a stroke has a non-identity affine transform (rotation, scale, translation), the AABB in local space is not valid in canvas space. It is transformed before culling:

```rust
if props.transform.value.is_identity() {
    data.bounds.expanded(stroke_expand)
} else {
    data.bounds.transform(&props.transform.value).expanded(stroke_expand)
}
```

`Aabb::transform` computes the four corners of the transformed AABB and computes the new enclosing AABB. It is conservative (the rotated AABB is larger than tight), but correct.

### Viewport API

```javascript
// Wasm API
const ptr = doc.build_render_data_viewport(vx0, vy0, vx1, vy1, strokeExpand);
const len = doc.get_render_data_len();
const view = new DataView(wasmMemory.buffer, ptr, len);
// Only visible strokes in the viewport — read before any mutating call
```

---

## 13. Binary wire format

### General design

- **Integers**: unsigned LEB128 varint. For small numbers (< 128), 1 byte; for typical ActorId/LamportTs (< 2^14), 2 bytes. Compact for small IDs, scales well for large ones.
- **Floats**: IEEE 754 little-endian, 4 fixed bytes. No advantage in variable encoding for coordinate floats.
- **Strings**: varint(length) + UTF-8 bytes.

### Operation tags

| Tag | Operation |
|-----|-----------|
| `0x01` | `InsertStroke` |
| `0x02` | `DeleteStroke` |
| `0x03` | `UpdateProperty` |
| `0x04` | `UpdateMetadata` |

### Operation sizes

| Operation | Typical size | JSON equivalent |
|-----------|-------------|-----------------|
| `DeleteStroke` | ~6 bytes | ~120 bytes |
| `InsertStroke` (1 point) | ~40 bytes | ~200 bytes |
| `InsertStroke` (100 points) | ~1,200 bytes | ~3,200 bytes |
| `InsertStroke` (100 pts + LZ4) | ~900 bytes | — |

LZ4 compression is available with the `compress` feature (threshold 200 bytes). Stroke points are highly compressible: sequential coordinates with small deltas have high redundancy.

### `InsertStroke` detail

```
[0x01]                           // tag: 1 byte
[varint(lamport)][varint(actor)] // id: 2–16 bytes
[varint(lamport)][varint(actor)] // origin_left: 2–16 bytes
[varint(lamport)][varint(actor)] // origin_right: 2–16 bytes
[tool: u8]                       // 1 byte
[varint(N)]                      // point_count: 1–5 bytes
[N × (f32 f32 f32)]              // N × 12 bytes
[OpId color_ts][u32 color]       // 6–18 bytes
[OpId sw_ts][f32 sw]             // 6–18 bytes
[OpId op_ts][f32 op]             // 6–18 bytes
[OpId tr_ts][6 × f32 transform]  // 10–26 bytes
```

### Snapshot

```
[u8: version = 1]
[varint: actor_id]
[varint: lamport]
[varint: sv_length][sv_bytes...]     // state vector (VectorClock)
[varint: op_count][op_1][op_2]...    // all ops representing current state
```

The snapshot is a sequence of ops, not a struct dump. This has two advantages:
1. Format evolution: new versions can add fields without breaking decoding of old snapshots.
2. Code reuse: `decode_snapshot` simply calls `apply_remote(op)` for each op — the same code path as real-time network ops.

---

## 14. Wasm bridge: zero-copy render

### Compilation

```toml
[lib]
crate-type = ["cdylib", "rlib"]
# cdylib → wasm-bindgen generates .wasm + JS glue
# rlib → allows use as a Rust dependency
```

The `wasm` feature activates `wasm-bindgen` and `js-sys`. Without it, the crate compiles as a pure Rust library (useful for native tests, benchmarks, Rust backends).

### `WasmDocument` — `src/wasm_bridge.rs`

```rust
#[wasm_bindgen]
pub struct WasmDocument {
    inner: Document,
    buffer: CausalBuffer,
    awareness: AwarenessStore,
    render_buf: Vec<u8>,  // reusable render data buffer
}
```

`render_buf` is reused across frames: `clear()` instead of `alloc`. This avoids Wasm GC pressure and keeps the buffer warm in cache.

### Zero-copy render path

The standard Wasm pattern would copy data twice: Rust → JS. With zero-copy:

```rust
pub fn build_render_data(&mut self) -> *const u8 {
    // Write into render_buf
    // Return raw pointer into Wasm linear memory
    self.render_buf.as_ptr()
}
```

In JavaScript:
```javascript
const ptr = doc.build_render_data();
const len = doc.get_render_data_len();
const view = new DataView(wasmInstance.memory.buffer, ptr, len);
// Read directly from Wasm memory — no copy
```

**Caveat**: the pointer is valid only until the next operation that mutates `render_buf`. The JS client must read all data in the same rAF frame before any mutation.

### Render buffer layout

Per visible stroke:
```
[id: 16 bytes = 2×u64 LE]
[point_count: u32 LE]
[tool: u8]
[color: u32 LE]
[stroke_width: f32 LE]
[opacity: f32 LE]
[transform: 6×f32 LE = a,b,c,d,tx,ty]
[N × (x:f32, y:f32, pressure:f32)]  // N × 12 bytes
```

Pressure is included in each point for renderers that support it (variable line width, pencil-style rendering).

### Exposed Wasm API

```javascript
// Lifecycle
new WasmDocument(actorId: bigint): WasmDocument
doc.insert_stroke(points: Float32Array, tool: number, color: number,
                  width: number, opacity: number): Uint8Array  // → 16B StrokeId
doc.delete_stroke(strokeId: Uint8Array): boolean
doc.apply_update(updateBytes: Uint8Array): Uint8Array  // → affected StrokeIds

// Render
doc.build_render_data(): number                    // → pointer (use with DataView)
doc.get_render_data_len(): number
doc.build_render_data_viewport(vx0, vy0, vx1, vy1, strokeExpand): number

// Properties
doc.update_stroke_property(id, key, valueBytes): boolean
doc.set_simplify_epsilon(epsilon: number): void
doc.simplify_stroke(id, epsilon): number           // → points removed

// Undo
doc.undo(): Uint8Array                             // → 16B StrokeId or []
doc.undo_depth(): number

// Bounds
doc.get_stroke_bounds(id): Uint8Array              // → 16B [min_x,min_y,max_x,max_y] f32 LE

// Sync
doc.encode_pending_update(): Uint8Array            // local ops to send
doc.encode_state_vector(): Uint8Array
doc.encode_snapshot(): Uint8Array
WasmDocument.from_snapshot(actorId, bytes): WasmDocument

// Awareness
doc.apply_cursor_update(cursorBytes: Uint8Array): boolean
doc.encode_local_cursor(x, y, nowMs, color): Uint8Array  // → 28 bytes
doc.get_all_cursors(): Uint8Array                  // N×28 bytes
doc.evict_stale_cursors(nowMs: bigint): number
doc.remove_cursor(actorId: bigint): void
```

---

## 15. Awareness: ephemeral cursors

### Separation from CRDT

User cursors are **not** part of the CRDT. Rationale:

1. **Volatility**: cursor position changes 60 times per second on a touchscreen. Persisting it in the op-log would fill history with thousands of irrelevant ops.
2. **No convergence needed**: if two cursor states are lost, simply use the most recent one. There is no "both changes matter" semantics as there is for stroke inserts.
3. **No reconnect replay**: on reconnect, the cursor will be resent with the current position.

### Cursor wire format: 28 fixed bytes

```
[actor: u64 LE] [x: f32 LE] [y: f32 LE] [ts_ms: u64 LE] [color: u32 LE]
= 8 + 4 + 4 + 8 + 4 = 28 bytes
```

Unlike CRDT ops that use LEB128 (variable), cursors use fixed LE. Rationale: they are sent in bulk (N × 28 bytes) and decoded via `chunks_exact(28)` without parsing. Predictability compensates for the potential inefficiency for small actorIds.

### TTL-based eviction

`AwarenessStore` evicts actors whose last state has `updated_at_ms` older than `ttl_ms` (default: 30 seconds). Call `evict_stale(now_ms)` periodically (e.g. every second).

---

## 16. Local undo

### Why undo is local, not global

A global CRDT undo ("undo the last change regardless of who made it") requires complex op-inversion CRDTs (like those in Logoot-Undo). It also has confusing semantics in collaboration: if Alice undoes a Bob operation, is that correct?

The standard convention in collaborative tools (Figma, Google Docs) is: **undo only undoes your own changes, in order**.

### Implementation

```rust
// document.rs
const MAX_UNDO_DEPTH: usize = 200;
undo_stack: Vec<StrokeId>  // IDs of strokes inserted by this actor, in order
```

`undo_last_stroke()`:
1. Pop from stack.
2. Call `delete_stroke(id)`.
3. `delete_stroke` returns `false` if the stroke was already remotely deleted → skip to next.
4. If the stack empties without deleting anything → return `None`.

This generates a real `DeleteStroke` op that propagates to peers. Undo is collaboratively visible: if Alice undoes her stroke, Bob sees it disappear.

### Cap of 200 entries

Without a cap, a many-hour drawing session would accumulate thousands of entries. The cap removes the oldest (FIFO: `remove(0)`). 200 strokes are sufficient for any reasonable undo sequence.

**Performance note**: `Vec::remove(0)` is O(n) to shift elements. With cap=200, this is 200 memmoves — negligible. If the cap grew to thousands, a `VecDeque` would be justified.

### Session interaction

`undo_stack` is not serialized (not in the snapshot or ops). On reconnect or opening from a snapshot, the undo_stack is empty. This is intentional: undo is a session operation, not a document history operation.

---

## 17. Testing strategy

### Unit tests (46)

Cover each module in isolation:

| Module | Tests | What they verify |
|--------|-------|-----------------|
| `types.rs` | 3 | Lamport tick, OpId ordering, VectorClock dominates |
| `rga.rs` | 4 | Insert order, concurrent convergence, tombstone, idempotency |
| `stroke.rs` | 11 | LwwRegister, Transform2D, Aabb (from_points, intersects, expanded, transform), RDP |
| `encoding.rs` | 5 | Varint roundtrip, OpId roundtrip, update roundtrip, vector clock, snapshot roundtrip |
| `document.rs` | 8 | Insert/delete, remote merge, convergence, undo basic, undo skip remote, undo gen delete op, auto-simplify on/off |
| `gc.rs` | 4 | Basic GC, unstable GC, re-parenting 1-hop, re-parenting multi-hop |
| `awareness.rs` | 4 | Encode/decode cursor, LWW ordering, TTL eviction, bulk encode/decode |
| `causal_buffer.rs` | 3 | Basic buffering, cascading release, idempotency |
| `compression.rs` | 4 | LZ4 roundtrip, threshold passthrough, stats |

### Property tests (7) — `tests/convergence_props.rs`

Using `proptest` (Rust equivalent of QuickCheck), 200 random cases each:

```rust
proptest! {
    fn prop_two_actors_converge       // 2 peers, ops in different order → same state
    fn prop_idempotent                // applying same ops twice = applying once
    fn prop_commutative               // A then B = B then A
    fn prop_three_actors_converge     // 3 peers all converge
    fn prop_delete_converges          // concurrent delete+insert converges
    fn prop_causal_buffer_converges   // out-of-order delivery → same as in-order
    fn prop_snapshot_roundtrip        // encode + decode → identical state
}
```

Property tests are the most important defense against convergence bugs: edge cases (same Lamport, insert after tombstone, causal buffer cascades) are rarely covered by manual tests but proptest generates them systematically.

### Critical regression tests

- `gc_reparents_surviving_origin_references`: serializes snapshot after GC and reconstructs on a new peer. Verifies z-order is identical. This test would fail if re-parenting didn't exist.
- `gc_reparent_chain_multiple_hops`: chain A→B(deleted)→C(deleted)→D(alive). GC of B and C must re-parent D to A.
- `undo_skips_remotely_deleted_strokes`: undo on a stroke that a remote peer already deleted must skip it silently.
- `undo_generates_pending_delete_op`: verifies that undo generates exactly one `DeleteStroke` in `pending_ops`.

---

## 18. Formal guarantees and wire format policy

### Guarantees vectis-crdt provides

| Guarantee | Description |
|----------|-------------|
| **Strong Eventual Consistency** | Two replicas that apply the same set of operations in any order converge to the same visible state (same z-order, same properties). |
| **Deterministic total stroke order** | Z-order is a pure function of operations — it does not depend on arrival order, system clock time, or receiver ActorId. |
| **Idempotency** | Applying the same operation more than once is equivalent to applying it once. Safe with WebSocket redelivery. |
| **Snapshot-replay equivalence** | `decode_snapshot(encode_snapshot(doc))` produces the same visible state as `doc`. Z-order and properties are identical. |
| **GC does not change visible order** | After a GC cycle, `visible_stroke_ids()` returns exactly the same IDs in the same order as before. Re-parenting is deterministic across peers that run the same GC. |
| **Causal delivery** | `apply_remote_buffered` guarantees no operation is applied before its causal dependencies. |

### What vectis-crdt does NOT guarantee

| Limitation | Description |
|-----------|-------------|
| **Causal stability without MVV** | GC requires the server to compute and broadcast the Minimum Version Vector (MVV). Without a server, GC cannot run safely in a pure P2P scenario. |
| **LWW consistency across offline sessions** | If two peers modify the same property offline, the winner is the one with the higher OpId — not the "most recent by wall clock". |
| **Liveness under network partition** | If two peers are disconnected indefinitely, they don't converge until they reconnect. This is inherent to CRDTs without coordination. |

### Wire format and versioning policy

The wire format has an explicit version byte in snapshots (`SNAPSHOT_VERSION = 1`).

**Policy by data type:**

| Type | Guarantee |
|------|----------|
| **Snapshots** | Backward compatible within the same major version. v1 snapshots are always decodable by v1.x implementations. |
| **Incremental updates** | No explicit version — sender and receiver are assumed to have the same protocol version. Use snapshots for cross-version sync. |
| **Awareness (cursors)** | Fixed 28 bytes. No version. Not persisted — unknown fields are simply ignored if the format changes. |

**Evolution commitments:**

- Adding new fields to `InsertStroke` requires a `SNAPSHOT_VERSION` bump.
- The size of `StrokePoint` (12 bytes, 3×f32) is fixed by design — changing it would require snapshot migration.
- Operation tags (0x01–0x04) are stable. New tags (0x05+) can be added without breaking old decoders that return an error on unknown tags.

---

## 19. Defensive limits

To prevent resource exhaustion from malformed or malicious external data, `decode_*` and `apply_remote` enforce the following limits (defined in `document.rs` as `pub` constants):

| Constant | Value | What it protects |
|----------|-------|-----------------|
| `MAX_POINTS_PER_STROKE` | 50,000 | `decode_stroke_data_at`: rejects the op if the payload declares more points. Prevents giant allocations. At 240 Hz this is ~3.5 minutes of continuous stroke without simplification. |
| `MAX_STROKES` | 100,000 | `apply_remote(InsertStroke)`: silently drops if the document already has MAX_STROKES items. Also limits `ops_per_update` in `decode_update`. |
| `MAX_ACTORS` | 10,000 | `decode_vector_clock`: silently truncates. Prevents the "VectorClock explosion" attack where a spoofed peer sends 10^6 distinct entries causing unbounded BTreeMap growth. |

**Limits philosophy**: limits in external data parsing paths (`decode_*`) return `Err` or `None` — the error propagates to the caller. Limits in `apply_remote` (already-parsed data from remote peers) silently drop because the CRDT is designed to be tolerant; dropping a remote op is preferable to OOM.

**Local limits are more permissive**: `insert_stroke` (local, trusted operation) has no point count limit — auto-simplification (RDP epsilon=0.5) reduces 50k points to ~500 before storing.

---

## 20. Public API encapsulation

### Design principle

Internal fields of `Document`, `RgaArray`, `VectorClock`, and `StrokeStore` are `pub(crate)`. This means:

- Code within the crate (`gc.rs`, `encoding.rs`, `wasm_bridge.rs`) can access internals needed for its operation.
- Code external to the crate **can only use public methods**. It cannot:
  - Set `doc.min_version` directly (risk of premature GC → corruption)
  - Modify `rga_array.items` (risk of desyncing the index)
  - Access `stroke_store.strokes` directly (bypasses the abstraction)

### Fields that remain `pub`

| Field | Rationale |
|-------|----------|
| `Document.local_actor` | Read-only identity, legitimately needed externally |
| `Document.version` | External callers need to read it to build state vectors for delta sync |
| `Document.simplify_epsilon` | Legitimate user configuration |

### How to drain pending ops

```rust
// Correct (from outside the crate):
let ops = doc.take_pending_ops();
encode_update(&ops)

// Does NOT compile from outside the crate (pub(crate)):
let ops = std::mem::take(&mut doc.pending_ops);
```

---

## 21. Known limitations

### 1. No property history

`LwwRegister` only retains the winning value. There is no way to see "what colors did this stroke have" or to undo a specific property change.

### 2. Undo does not undo properties

`undo_last_stroke` only undoes inserts, not `UpdateProperty`. If Alice changes the color of a stroke and wants to undo it, she cannot with the current API.

### 3. Multi-select and groups

There are no CRDT primitives for "select a group of strokes and move them together". Each stroke has its own `transform` LWW. Moving a group requires applying `N` `UpdateProperty` operations, which are concurrently interleave-able by other peers.

### 4. Pressure not simplified in RDP

RDP simplifies based only on x,y coordinates. Pressure is preserved at kept points, but there is no interpolation for eliminated points. For strokes with very variable pressure, this can cause abrupt width changes at join points.

### 5. `VecDeque` for undo_stack

With cap=200, `Vec::remove(0)` is O(200). Fast enough, but `VecDeque::pop_front` would be O(1).

### 6. Offline peers > gc_grace_period

They require a full snapshot on reconnection. The server needs to detect "this peer has been disconnected for X time" and send a snapshot instead of a delta. This logic is documented but requires server-side implementation.

### 7. No snapshot compression

Snapshots are large (all points + properties for all strokes). Compressing the snapshot with LZ4 before sending would reduce initial load time. Currently LZ4 compression only exists for incremental updates.

---

## 22. Roadmap

### Integration

1. Build to Wasm: `wasm-pack build --target web --out-dir pkg`
2. Parse render buffer in JS using `DataView` with the layout from §14
3. Implement server-side MVV broadcast: compute `min_version = min(all_peers)` periodically and broadcast to all clients
4. Enable `compress` feature for updates > 200B

### Future improvements

5. **E2E tests with Playwright**: 2+ peer scenarios in headless Chromium
6. **`VecDeque` for undo_stack**: O(1) pop_front
7. **Property undo**: separate stack of `(StrokeId, PropertySnapshot)` for undoing style changes
8. **Groups/layers**: new layer CRDT primitive (RGA of layers, each layer has an RGA of strokes)
9. **Snapshot compression**: LZ4 of the full blob before sending
10. **Formal benchmarks**: `criterion` benchmarks for `integrate`, `encode_snapshot`, `build_render_data_viewport`
