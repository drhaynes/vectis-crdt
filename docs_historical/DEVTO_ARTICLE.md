> Historical note:
>
> This article describes an earlier direction for `vectis-crdt` where the repository shipped Rust,
> npm/TypeScript, WebAssembly JavaScript bindings, and Python/PyO3 bindings from the same source tree.
>
> The current project direction has changed:
> - npm/TypeScript package support has been removed.
> - Python/PyO3 support has been removed.
> - The old `WasmDocument` / `wasm_bridge.rs` JavaScript-facing API has been removed.
> - Browser demo rendering now lives primarily in Rust/Wasm in the `wasm_demo` crate using `web-sys` and `CanvasRenderingContext2d`, with only a tiny JS loader.
>
> The CRDT design discussion remains useful background, but packaging, binding, and integration details in this article are no longer authoritative.

# How I Built a CRDT Engine for My Collaborative Whiteboard in Rust

> **vectis** (lat.) — arrow, vector.
> A CRDT engine for vector strokes on real-time collaborative whiteboards.
> GitHub: [vectis-crdt](https://github.com/pencilsync/vectis-crdt) | Crates.io | PyPI

---

I'm building **PencilSync**, a real-time collaborative whiteboard. Think of it like Figma's infinite canvas, but focused on stylus input and handwriting. Multiple users draw simultaneously, offline sessions sync when reconnected, and every stroke appears on everyone's screen without conflicts.

Sounds simple. It isn't.

After evaluating existing CRDT libraries, none of them modeled the domain correctly — they were built for text editors, not vector graphics. So I built `vectis-crdt`: a Rust library that compiles to both native and WebAssembly, with Python bindings for the server side.

This is the story of why I made every design decision I made.

---

## The Problem: Three Requirements That Pull in Opposite Directions

A real-time collaborative whiteboard has three fundamental constraints:

1. **Local responsiveness**: Every stylus touch must appear on screen *immediately*, before any server round-trip. You can't afford 80ms of latency between pen-down and pixel-rendered.
2. **Eventual convergence**: Two clients that have applied the same set of operations — in any order — must end up with identical visible state.
3. **No merge conflicts**: A whiteboard has no "conflicts" to resolve. Two users drawing simultaneously both draw. You never show a conflict dialog to someone holding a stylus.

The classic solution is **CRDTs** (Conflict-free Replicated Data Types). But which flavor?

---

## Why RGA + YATA, Not OT or Simple Counters

I evaluated three approaches:

**Operational Transformation (OT)** — Used by Google Docs. Requires a central server to sequence all operations before broadcasting. Kill condition for offline support; adds latency for every stroke.

**State-based CRDTs (grow-only sets, counters)** — Simple but wrong for this domain. A whiteboard needs *ordered* strokes (z-order). "Stroke B is drawn on top of stroke A" is fundamental. A set has no order; a counter has no identity.

**RGA (Replicated Growable Array) + YATA** — This is what Yjs uses for text. It maintains a sequence with a total deterministic order for concurrent insertions. I adapted it: instead of characters, each slot holds a stroke reference.

The key insight: **the array is small**. A whiteboard has hundreds of strokes, not millions of characters. This unlocks simpler data structures — a `Vec` with a `HashMap` index rather than a tree — without performance concerns.

---

## The Base Layer: OpId and Vector Clocks

Every operation in the system gets a globally unique identifier:

```rust
pub struct OpId {
    pub lamport: LamportTs,  // monotonically increasing logical clock
    pub actor: ActorId,      // u64 — compact wire representation of a peer
}
```

`ActorId` is a `u64` assigned by the server on first connection, not a UUID. This saves 8 bytes per reference on the wire — significant when every stroke carries three of them (`id`, `origin_left`, `origin_right`).

The ordering on `OpId` is total and deterministic:

```rust
impl Ord for OpId {
    fn cmp(&self, other: &Self) -> Ordering {
        self.lamport.0.cmp(&other.lamport.0)
            .then_with(|| self.actor.0.cmp(&other.actor.0))
    }
}
```

Higher Lamport wins; on tie (concurrent), higher ActorId wins. This is the tie-breaker that makes the CRDT converge when two users draw at the exact same logical moment.

For causality tracking, each peer maintains a **Vector Clock**:

```rust
pub struct VectorClock {
    clocks: BTreeMap<ActorId, u64>,  // actor → max lamport seen from that actor
}
```

The vector clock powers three features: causal delivery, delta synchronization, and garbage collection. It's the single most important data structure in the system.

---

## The Core CRDT: YATA Integration Algorithm

The RGA array holds the z-ordering of strokes. Each item stores its insertion context:

```rust
pub struct RgaItem {
    pub id: OpId,
    pub origin_left: OpId,   // item to the left at insert time
    pub origin_right: OpId,  // item to the right at insert time
    pub content: StrokeId,   // reference to the actual stroke data
    pub state: ItemState,    // Active or Tombstone { deleted_at }
}
```

The genius of YATA is how it resolves concurrent insertions. Imagine Alice and Bob both insert a stroke at the same position simultaneously:

- Alice inserts `A` with `origin_left = X`
- Bob inserts `B` with `origin_left = X`

Without a rule, the order would depend on which operation arrives first — breaking convergence. YATA's rule:

> Within items sharing the same `origin_left`, items whose `origin_left` position is to the *right* of ours are "right subtree" items and are skipped. Among remaining items in the same zone, **higher OpId goes further left**.

The implementation:

```rust
pub fn integrate(&mut self, item: RgaItem) {
    // Idempotent: skip if already seen.
    if self.index.contains_key(&item.id) { return; }

    let scan_start = /* index after origin_left */;
    let scan_end   = /* index of origin_right */;
    let origin_left_pos = /* position of our origin_left */;

    let mut insert_pos = scan_start;
    for i in scan_start..scan_end {
        let existing = &self.items[i];
        let existing_ol_pos = /* position of existing.origin_left */;

        if existing_ol_pos < origin_left_pos {
            break;  // passed our zone
        } else if existing_ol_pos > origin_left_pos {
            insert_pos = i + 1;  // skip right-subtree item
        } else {
            // Same zone: higher OpId → further left
            if existing.id > item.id {
                insert_pos = i + 1;
            } else {
                break;
            }
        }
    }
    self.items.insert(insert_pos, item);
    self.rebuild_index_from(insert_pos);
}
```

This is `O(k)` where `k` is the number of concurrent conflicting operations at that position — typically 1 or 2 in practice, `O(n)` worst case.

---

## Deletions: Tombstones and Why You Can't Just Remove Items

In a distributed system, you can't immediately remove a deleted item from the array. Consider this scenario:

1. Alice has `[A, B, C]`. She deletes B.
2. Bob, offline, inserts D *after B*. So Bob has `[A, B, D, C]`.
3. Bob reconnects and his insert arrives.
4. If we'd already erased B from Alice's array, D's `origin_left = B.id` would be unresolvable.

The solution: **tombstones**. Deleted items stay in the array with state `Tombstone { deleted_at: OpId }`, invisible to the application but still present for conflict resolution.

```rust
pub enum ItemState {
    Active,
    Tombstone { deleted_at: OpId },
}
```

This means the array can grow unbounded over time. Which brings us to garbage collection.

---

## Incremental Garbage Collection with Re-Parenting

Tombstones are safe to remove only when **all** known peers have seen the deletion — a condition called "causal stability". The system tracks this via the **Minimum Version Vector (MVV)**: the server broadcasts the oldest vector clock component among all connected peers.

A tombstone `T` with `deleted_at = op` is GC-eligible when:
```
mvv.get(op.actor) >= op.lamport
```

The incremental GC runs in bounded cycles (`max_gc_per_cycle = 5,000` items) to avoid long pauses.

But removing a tombstone creates a problem: surviving items may have `origin_left = tombstone.id`, which would become a dangling reference in snapshots. The GC performs **re-parenting**: before erasing tombstones, it walks each surviving item's `origin_left` chain to find the nearest kept ancestor:

```rust
fn find_kept_ancestor(mut origin: OpId, remove_set: &HashSet<OpId>, ...) -> OpId {
    for _ in 0..MAX_DEPTH {
        if !remove_set.contains(&origin) { return origin; }
        origin = items[index[&origin]].origin_left;
    }
    OpId::ZERO  // attach to root if chain is exhausted
}
```

This re-parenting is **deterministic**: any two peers with the same MVV produce identical re-parented states. Convergence is preserved.

---

## Mutable Properties: LWW-Registers per Field

Strokes have mutable properties: color, stroke width, opacity, transform. If Alice changes the color while Bob changes the opacity, both changes must survive — not conflict.

The solution: each property is an independent **Last-Write-Wins Register**:

```rust
pub struct StrokeProperties {
    pub color:        LwwRegister<u32>,
    pub stroke_width: LwwRegister<f32>,
    pub opacity:      LwwRegister<f32>,
    pub transform:    LwwRegister<Transform2D>,
}

pub struct LwwRegister<T: Clone> {
    pub value: T,
    pub timestamp: OpId,  // determines the winner
}

impl<T: Clone> LwwRegister<T> {
    pub fn apply(&mut self, value: T, timestamp: OpId) -> bool {
        if timestamp > self.timestamp {
            self.value = value;
            self.timestamp = timestamp;
            true
        } else { false }
    }
}
```

Color change and opacity change use different registers → both are preserved. Color vs. color concurrent change → higher timestamp wins.

---

## Causal Delivery: The CausalBuffer

Operations can arrive out of order over WebSocket. If an `InsertStroke(B, origin_left=A.id)` arrives before `InsertStroke(A)`, applying B immediately would place it at the wrong z-order position.

The `CausalBuffer` holds not-yet-ready operations and retries them each time a new operation is successfully applied:

```rust
fn is_causally_ready(op: &Operation, doc: &Document) -> bool {
    match op {
        Operation::InsertStroke { origin_left, .. } =>
            origin_left.is_zero() || doc.stroke_order.index.contains_key(origin_left),
        Operation::DeleteStroke { target, .. } =>
            doc.stroke_order.index.contains_key(target),
        Operation::UpdateProperty { target, .. } =>
            doc.stroke_store.contains(target),
        Operation::UpdateMetadata { .. } => true,
    }
}
```

The buffer has a hard limit (`10,000` operations). If exceeded, the client requests a full snapshot from the server rather than trying to recover — a safer failure mode than OOM.

---

## Stroke Simplification: Ramer-Douglas-Peucker at Insert Time

A stylus at 240Hz produces one point every ~4ms. A 3-second stroke = ~720 raw points. Storing and transmitting all of them is wasteful — the human eye can't perceive the difference at normal zoom levels.

I implemented the **Ramer-Douglas-Peucker** algorithm, applied automatically at insert time:

```rust
pub fn insert_stroke(&mut self, mut data: StrokeData, properties: StrokeProperties) -> StrokeId {
    if self.simplify_epsilon > 0.0 && data.points.len() > 2 {
        data.simplify(self.simplify_epsilon);  // in-place, before storing
    }
    // ...
}
```

The RDP implementation uses an **iterative stack** rather than recursion — no stack overflow risk for 50k-point strokes:

```rust
fn rdp_indices(points: &[StrokePoint], epsilon: f32) -> Vec<usize> {
    let mut stack: Vec<(usize, usize)> = Vec::with_capacity(64);
    stack.push((0, n - 1));
    while let Some((start, end)) = stack.pop() {
        // find max perpendicular distance in [start, end]
        // if > epsilon: keep the point, push both halves
    }
}
```

Typical reduction: a 500-point freehand stroke simplifies to 30–80 points (~88%) at `epsilon = 0.5`, with no perceptible visual difference.

---

## Delta Synchronization

When a client reconnects after being offline, you don't want to send the entire document history — just the operations the client hasn't seen yet.

The Vector Clock `diff` method computes exactly this:

```rust
pub fn diff(&self, other: &VectorClock) -> Vec<(ActorId, u64, u64)> {
    // Returns (actor, from_ts, to_ts) ranges for each actor
    // where `self` has seen more than `other`
    self.clocks.iter()
        .filter(|(&actor, &my_ts)| my_ts > other.get(actor))
        .map(|(&actor, &my_ts)| (actor, other.get(actor) + 1, my_ts))
        .collect()
}
```

Client sends its vector clock → server computes diff → server sends only the missing operations. O(actors) to compute, not O(operations).

---

## The Wasm Bridge: Zero-Copy Rendering

The hot path for rendering must be fast. Every animation frame, the canvas engine needs all visible stroke data. Crossing the JS↔Wasm boundary with individual function calls is expensive (~100–200ns each).

The solution: pack all visible strokes into a single contiguous buffer in Wasm linear memory, then hand JS a raw pointer:

```rust
#[wasm_bindgen]
pub fn build_render_data_viewport(
    &mut self, vx0: f32, vy0: f32, vx1: f32, vy1: f32, stroke_expand: f32
) -> *const u8 {
    let viewport = Aabb { min_x: vx0, min_y: vy0, max_x: vx1, max_y: vy1 };
    self.render_buf.clear();
    for id in self.inner.visible_stroke_ids() {
        if let Some((data, props)) = self.inner.get_stroke(&id) {
            let effective_bounds = if props.transform.value.is_identity() {
                data.bounds.expanded(stroke_expand)
            } else {
                data.bounds.transform(&props.transform.value).expanded(stroke_expand)
            };
            if !effective_bounds.intersects(&viewport) { continue; }  // viewport culling
            write_stroke_to_buf(&mut self.render_buf, &id, data, props);
        }
    }
    self.render_buf.as_ptr()
}
```

The buffer format is a flat binary layout per stroke (16B ID + header + N × 12B points). JS reads it with a `Float32Array` view over `WebAssembly.Memory` — **zero copies, zero allocations on the JS side**.

AABB viewport culling skips strokes outside the camera view entirely, without iterating their points. For a whiteboard with 5,000 strokes but only 200 visible in the current viewport, this is a significant win.

---

## Wire Format: LEB128 Varints

The binary protocol uses unsigned LEB128 for all integers and little-endian IEEE 754 for floats. A typical `InsertStroke` for a simplified 40-point stroke takes ~560 bytes on the wire. Uncompressed. With the optional LZ4 feature, repeated coordinates compress well.

The format is versioned (`SNAPSHOT_VERSION = 1`) and each decode function enforces size limits before allocating:

```rust
if count as usize > MAX_POINTS_PER_STROKE { return None; }  // reject before Vec::with_capacity
```

This prevents resource exhaustion from malformed or hostile payloads — important when the Wasm module runs in a browser with untrusted server data.

---

## Local Undo

Undo on a collaborative whiteboard is subtle. The naive approach — "revert to previous state" — is wrong because you can't undo what other users did. The correct semantic: **undo generates a delete operation** that is broadcast to all peers.

```rust
pub fn undo_last_stroke(&mut self) -> Option<StrokeId> {
    while let Some(id) = self.undo_stack.pop() {
        if self.delete_stroke(id) {  // generates a DeleteStroke pending op
            return Some(id);
        }
        // Stroke was already deleted remotely → skip, try previous.
    }
    None
}
```

The undo stack only tracks the *local* actor's strokes. The stack is session-only (not persisted). Depth is capped at 200 entries.

---

## Formal Guarantees

| Property | Guarantee |
|----------|-----------|
| **Strong Eventual Consistency** | Any two replicas with the same operation set have identical `visible_stroke_ids()`. |
| **Idempotency** | `apply_remote(op)` called twice is equivalent to once. |
| **Commutativity** | Application order of concurrent ops doesn't change final state. |
| **GC Safety** | Only causally stable tombstones are removed. No operation that any known peer might still need is GC'd. |
| **Wire compatibility** | Snapshot version mismatch returns `SnapshotVersionMismatch` error; format changes bump `SNAPSHOT_VERSION`. |

---

## Defensive Limits

The library enforces hard limits to prevent resource exhaustion:

```rust
pub const MAX_POINTS_PER_STROKE: usize = 50_000;  // 240Hz × 3min × safety margin
pub const MAX_STROKES: usize = 100_000;            // ~8 MB RGA memory
pub const MAX_ACTORS: usize = 10_000;              // bounds VectorClock BTreeMap
const DEFAULT_MAX_CAPACITY: usize = 10_000;        // CausalBuffer
```

These are checked at every public decode boundary — before any allocation.

---

## Build Output

```
cargo build --release --target wasm32-unknown-unknown
wasm-opt -O3 -o vectis_crdt_bg.wasm vectis_crdt_bg.wasm
gzip -9 vectis_crdt_bg.wasm
```

Result: **~85 KB gzipped**. Fits comfortably in a single HTTP/2 push alongside the app bundle.

---

## What I Would Do Differently

1. **`VecDeque` for the undo stack** — `undo_stack.remove(0)` is O(n). With 200 entries it's imperceptible, but `VecDeque::pop_front` is cleaner.

2. **Spatial index for culling** — The current AABB culling iterates all visible strokes linearly. For documents with 10,000+ visible strokes, an R-tree or grid partition would be faster.

3. **Op-log persistence** — The library currently operates in-memory. Persisting the op-log to IndexedDB on the client and to a WAL on the server would enable true offline-first with conflict-free reconnect without full snapshots.

4. **Op compression by run** — Adjacent InsertStroke ops from the same actor often have sequential Lamport timestamps and similar coordinates. Delta-encoding them would reduce wire size further.

---

## Conclusion

Building `vectis-crdt` taught me that domain-specific CRDTs are worth the investment. A general-purpose CRDT library would have forced the whiteboard domain to adapt to the library's model. Instead, the model adapts to the domain:

- Strokes, not characters, are the unit of the array.
- Z-ordering is a first-class concept, not an afterthought.
- Simplification, viewport culling, and awareness are built-in, not bolted on.

The YATA algorithm gives convergence. Vector clocks give causal consistency. The MVV gives safe GC. The Wasm bridge gives zero-copy rendering. Each piece is independently verifiable.

If you're building a collaborative canvas application, I hope this walkthrough saves you the six weeks it took me to get all these pieces to fit together.

---

**Links:**
- [vectis-crdt on GitHub](https://github.com/pencilsync/vectis-crdt)
- [ARCHITECTURE.md — full technical reference](https://github.com/pencilsync/vectis-crdt/blob/main/ARCHITECTURE.md)
- [crates.io/crates/vectis-crdt](https://crates.io/crates/vectis-crdt)

*Tags: `rust`, `crdt`, `distributed-systems`, `webassembly`, `collaborative`*
