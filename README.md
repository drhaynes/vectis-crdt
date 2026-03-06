# vectis-crdt

[![Rust](https://img.shields.io/badge/Rust-1.70+-orange?logo=rust)](https://www.rust-lang.org/)
[![WebAssembly](https://img.shields.io/badge/WebAssembly-ready-654ff0?logo=webassembly)](https://webassembly.org/)
[![CRDT](https://img.shields.io/badge/CRDT-RGA%2FYATA%20%2B%20LWW-blue)](https://crdt.tech/)
[![License: MIT](https://img.shields.io/badge/License-MIT-green)](LICENSE)

**vectis** (lat.) — arrow, vector.

A Rust CRDT library for ordered collections of mutable objects, compiled to WebAssembly. It provides **Strong Eventual Consistency** for any domain where items have a defined z-order and independently mutable properties — built primarily for vector strokes on collaborative canvases, but applicable to any sequence of richly-attributed objects.

The library combines two complementary conflict-free data structures:

- **[`RgaArray`]** — a YATA-style Replicated Growable Array that maintains a deterministic total order over items. Concurrent inserts from any number of peers converge to the same sequence without coordination, using Lamport timestamps and actor IDs as a tiebreak. Tombstones are retained for causal consistency and reclaimed via incremental GC once causally stable (MVV-gated).
- **[`LwwRegister`]** — a Last-Write-Wins register per mutable property (color, width, opacity, affine transform). Each property is an independent register with its own OpId timestamp, so concurrent edits to different properties of the same item are always preserved — only true conflicts (same property, same instant) are resolved deterministically by OpId order.

Operations are encoded in a compact binary format (LEB128 varints + LE floats), synchronized via vector clock state vectors for delta delivery, and can be applied in any order — the causal buffer holds out-of-order operations until their dependencies arrive. The Wasm API exposes a zero-copy render path: visible item data is written into a reusable buffer in Wasm linear memory and read directly from JS via `DataView`, with optional viewport culling via AABB intersection.

---

## Features

- Binary wire format (LEB128 varints + LE floats) — compact stroke payloads
- Delta sync via vector clock state vectors — only send what the peer is missing
- Incremental tombstone GC with origin re-parenting — bounded memory growth
- Viewport culling with AABB bounds — O(visible) render data, not O(total)
- RDP stroke simplification — configurable epsilon, iterative (no stack overflow)
- Causal delivery buffer — out-of-order ops buffered until causally deliverable
- Ephemeral cursor awareness — TTL-based, not persisted to CRDT state
- Local undo — stack of local op IDs, depth 200, skips remotely deleted strokes
- Optional LZ4 compression — feature-gated, threshold 200 B
- Wasm-bindgen JS API — zero-copy render data via raw Wasm memory pointer

## Usage

### Rust

```rust
use vectis_crdt::document::Document;
use vectis_crdt::stroke::{StrokeData, StrokePoint, StrokeProperties, ToolKind};
use vectis_crdt::types::{ActorId, OpId};

let mut doc = Document::new(ActorId(1));

// Build stroke data
let points: Box<[StrokePoint]> = vec![
    StrokePoint::new(0.0, 0.0, 1.0),
    StrokePoint::new(10.0, 10.0, 0.8),
].into();
let data = StrokeData::new(points, ToolKind::Pen);
let props = StrokeProperties::new(0xFF0000FF, 2.0, 1.0, OpId::ZERO);

// Insert locally — generates a pending op
let stroke_id = doc.insert_stroke(data, props);

// Drain pending ops to encode and send over the wire
let ops = doc.take_pending_ops();
let wire_bytes = vectis_crdt::encoding::encode_update(&ops);

// Apply on a remote peer
let mut peer = Document::new(ActorId(2));
let remote_ops = vectis_crdt::encoding::decode_update(&wire_bytes).unwrap();
for op in remote_ops {
    peer.apply_remote(op);
}

assert_eq!(doc.visible_stroke_ids(), peer.visible_stroke_ids());
```

### WebAssembly

```bash
cargo install wasm-pack
wasm-pack build --target web --out-dir pkg
```

```javascript
import init, { WasmDocument } from "./pkg/vectis_crdt.js";

await init();

// actor_id: u64 passed as BigInt from JS
const doc = new WasmDocument(1n);

// Insert stroke: flat Float32Array [x, y, pressure, x, y, pressure, ...]
// tool: 0=Pen, 1=Eraser, 2=Marker, 3=Laser, 4=Shape, 5=Arrow
// color: 0xRRGGBBAA, stroke_width: f32, opacity: 0.0–1.0
const strokeId = doc.insert_stroke(
    new Float32Array([0, 0, 1.0, 10, 10, 0.8]),
    0,          // tool: Pen
    0xFF0000FF, // color: red
    2.0,        // stroke_width
    1.0,        // opacity
);
// strokeId: Uint8Array of 16 bytes (lamport u64 LE + actor u64 LE)

// Encode pending ops and send over WebSocket
const update = doc.encode_pending_update();
// ws.send(update)

// Apply a binary update received from another peer
// doc.apply_update(receivedBytes)

// Get render data for the current viewport (zero-copy)
const ptr = doc.build_render_data_viewport(
    camX, camY,               // top-left in canvas coords
    camX + viewW, camY + viewH, // bottom-right
    16.0,                      // stroke_expand: half of max stroke_width
);
const len = doc.get_render_data_len();
const view = new DataView(wasmMemory.buffer, ptr, len);
// Parse strokes from view — see ARCHITECTURE.md §14 for layout
```

## Feature flags

| Feature | Default | Description |
|---------|---------|-------------|
| `wasm` | yes | wasm-bindgen + JS API |
| `compress` | no | LZ4 compression for payloads > 200 B |

To use without Wasm (pure Rust library):

```toml
vectis-crdt = { version = "0.1", default-features = false }
```

## Architecture

See [ARCHITECTURE.md](ARCHITECTURE.md) for a detailed description of every design decision: why RGA/YATA over OT or Automerge, the GC re-parenting algorithm, the binary wire format, delta sync, and defensive limits.

## Safety limits

The library enforces hard limits to prevent resource exhaustion from malformed or malicious peers:

| Limit | Value |
|-------|-------|
| Points per stroke | 50 000 |
| Strokes per document | 100 000 |
| Actors tracked (vector clock) | 10 000 |
| Causal buffer capacity | 10 000 ops |
| Undo depth | 200 ops |

Exceeding these returns `VectisError::LimitExceeded` — no panics.

## Tests

```bash
cargo test               # 46 unit tests + 7 property tests (200 cases each)
cargo test --release     # faster property test runs
```

Property tests (proptest) cover:

- Two-actor convergence
- Three-actor convergence
- Commutativity
- Idempotency
- Delete convergence
- Causal buffer convergence
- Snapshot round-trip

## License

MIT
