# vectis-crdt

[![crates.io](https://img.shields.io/crates/v/vectis-crdt?label=crates.io&logo=rust)](https://crates.io/crates/vectis-crdt)
[![npm](https://img.shields.io/npm/v/vectis-crdt?label=npm&logo=npm)](https://www.npmjs.com/package/vectis-crdt)
[![docs.rs](https://img.shields.io/docsrs/vectis-crdt?logo=docs.rs)](https://docs.rs/vectis-crdt)
[![License: MIT](https://img.shields.io/badge/License-MIT-green)](LICENSE)

**vectis** (lat.) — arrow, vector.

A Rust CRDT library for ordered collections of mutable objects. Provides **Strong Eventual Consistency** for sequences of richly-attributed items — built for collaborative canvases (strokes, shapes, layers), but applicable to any domain requiring deterministic z-order with independently mutable properties.

Distributed as three packages from the same source:

| Package | Install |
|---------|---------|
| **Rust** (native or Wasm) | `cargo add vectis-crdt` |
| **TypeScript/JS** (pre-built Wasm + TS wrapper) | `npm install vectis-crdt` |
| **Python** (PyO3 bindings, build from source) | `maturin develop --features python` |

---

## How it works

Two complementary CRDTs handle the two independent concerns:

- **`RgaArray`** (YATA variant) — deterministic z-order for concurrent inserts from any number of peers, using Lamport timestamps + actor IDs as a tiebreak. No coordination needed.
- **`LwwRegister`** per property — Last-Write-Wins for mutable attributes (color, width, opacity, transform). Each property has its own timestamp so concurrent edits to *different* properties are always preserved.

All operations encode to a compact binary format (LEB128 varints + LE floats). A causal buffer holds out-of-order ops until their dependencies arrive. Tombstones are GC'd once causally stable, with origin re-parenting to preserve z-order in snapshots.

---

## Installation

### Rust

```toml
# Cargo.toml

# Pure Rust (no extra deps, for native servers/tests)
vectis-crdt = "0.1"

# With WebAssembly bindings
vectis-crdt = { version = "0.1", features = ["wasm"] }

# With LZ4 compression (payloads > 200 B)
vectis-crdt = { version = "0.1", features = ["compress"] }
```

### TypeScript / JavaScript

The npm package ships pre-built Wasm and a TypeScript wrapper — no Rust toolchain required.

```bash
npm install vectis-crdt
# or
pnpm add vectis-crdt
```

### Python

Not yet on PyPI. Build from source with [maturin](https://maturin.rs/):

```bash
git clone https://github.com/RafaCalRob/vectis-crdt
cd vectis-crdt
maturin develop --features python
```

---

## Quick start

### TypeScript (npm)

The high-level `VectisDocument` class handles WebSocket sync, serialization, and render data parsing:

```typescript
import { VectisDocument, ToolKind } from "vectis-crdt";

// Create a document and connect to a sync server
const doc = await VectisDocument.create(actorId, "wss://your-server/ws");

doc.onStrokesChanged = (strokes) => {
  // Re-render: strokes is an array of RenderStroke objects
  for (const s of strokes) {
    drawStroke(s.tool, s.color, s.strokeWidth, s.points); // s.points is a Float32Array view
  }
};

// Insert a stroke (points: [x, y, pressure, x, y, pressure, ...])
const strokeId = await doc.insertStroke(
  new Float32Array([0, 0, 1.0, 100, 100, 0.8, 200, 50, 0.9]),
  { tool: ToolKind.Pen, color: 0xFF0000FF, strokeWidth: 3.0, opacity: 1.0 }
);

// Update a property — propagates to all peers automatically
await doc.updateStrokeColor(strokeId, 0x0000FFFF);

// Undo the last local stroke
await doc.undo();

// Viewport-culled render data (zero-copy)
const visible = doc.getRenderDataViewport(camX, camY, camX + w, camY + h, 16);
```

### Rust (native)

```rust
use vectis_crdt::document::Document;
use vectis_crdt::stroke::{StrokeData, StrokePoint, StrokeProperties, ToolKind};
use vectis_crdt::types::{ActorId, OpId};

let mut doc_a = Document::new(ActorId(1));
let mut doc_b = Document::new(ActorId(2));

// Insert a stroke locally on peer A
let points: Box<[StrokePoint]> = vec![
    StrokePoint::new(0.0,  0.0,  1.0),
    StrokePoint::new(10.0, 10.0, 0.8),
].into();
let data  = StrokeData::new(points, ToolKind::Pen);
let props = StrokeProperties::new(0xFF0000FF, 2.0, 1.0, OpId::ZERO);
let _id   = doc_a.insert_stroke(data, props);

// Encode and send to peer B
let ops   = doc_a.take_pending_ops();
let bytes = vectis_crdt::encoding::encode_update(&ops);

// Apply on peer B — order doesn't matter, convergence is guaranteed
let remote_ops = vectis_crdt::encoding::decode_update(&bytes).unwrap();
for op in remote_ops {
    doc_b.apply_remote(op);
}

assert_eq!(doc_a.visible_stroke_ids(), doc_b.visible_stroke_ids());
```

### WebAssembly (low-level)

If you need direct Wasm control, build with `wasm-pack` and use the `WasmDocument` API:

```bash
cargo install wasm-pack
wasm-pack build --features wasm --target web --out-dir pkg
```

```javascript
import init, { WasmDocument } from "./pkg/vectis_crdt.js";

await init();
const doc = new WasmDocument(1n); // actorId as BigInt

// Insert a stroke
// tool: 0=Pen 1=Eraser 2=Marker 3=Laser 4=Shape 5=Arrow
// color: 0xRRGGBBAA as u32
const strokeId = doc.insert_stroke(
    new Float32Array([0, 0, 1.0, 10, 10, 0.8]),
    0,          // Pen
    0xFF0000FF,
    2.0,        // stroke_width
    1.0,        // opacity
);
// strokeId: Uint8Array(16) = lamport u64 LE + actor u64 LE

// Encode ops and send over WebSocket
const update = doc.encode_pending_update();
// ws.send(update)

// Apply update received from another peer
// doc.apply_update(receivedBytes)

// Zero-copy render data for viewport
const ptr = doc.build_render_data_viewport(
    camX, camY, camX + viewW, camY + viewH,
    16.0, // stroke_expand (half of max stroke_width)
);
const view = new DataView(wasmMemory.buffer, ptr, doc.get_render_data_len());
// Read strokes from view — see ARCHITECTURE.md §14 for buffer layout
```

---

## Features

- **Binary wire format** — LEB128 varints + LE floats; `DeleteStroke` ~6 bytes vs ~120 B JSON
- **Delta sync** — vector clock state vectors; only send what a peer is missing
- **Causal delivery buffer** — out-of-order ops held until causally deliverable
- **Incremental tombstone GC** — MVV-gated, with origin re-parenting to preserve z-order in snapshots
- **Viewport culling** — AABB per stroke, O(visible) render data instead of O(total)
- **RDP simplification** — configurable epsilon, iterative (no stack overflow at 240 Hz input rates)
- **Ephemeral cursor awareness** — 28-byte fixed format, TTL eviction, not persisted to CRDT state
- **Local undo** — depth 200, skips remotely-deleted strokes, generates real `DeleteStroke` ops
- **Optional LZ4 compression** — feature-gated, threshold 200 B

---

## Feature flags

| Feature | Default | Description |
|---------|---------|-------------|
| `wasm` | no | `wasm-bindgen` + `js-sys` — enables `WasmDocument` and the JS/TS API |
| `python` | no | `pyo3` — Python bindings via maturin |
| `compress` | no | `lz4_flex` — LZ4 compression for payloads over 200 B |

---

## Safety limits

Hard limits enforced on all external data paths — returns `VectisError::LimitExceeded`, never panics:

| Limit | Value |
|-------|-------|
| Points per stroke | 50 000 |
| Strokes per document | 100 000 |
| Actors tracked (vector clock) | 10 000 |
| Causal buffer capacity | 10 000 ops |
| Undo depth | 200 ops |

---

## Tests

```bash
cargo test               # 46 unit tests + 7 property tests (200 cases each)
cargo test --release     # faster for property tests
```

Property tests (proptest) verify:
- Two-actor and three-actor convergence
- Commutativity and idempotency
- Delete convergence
- Causal buffer convergence
- Snapshot round-trip integrity

---

## Architecture

See [ARCHITECTURE.md](ARCHITECTURE.md) for the full design rationale: why RGA/YATA over OT and Automerge, the GC re-parenting algorithm, the binary wire format, delta sync internals, and the zero-copy Wasm render path.

---

## License

MIT
