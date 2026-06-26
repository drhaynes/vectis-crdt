# vectis-crdt

[![crates.io](https://img.shields.io/crates/v/vectis-crdt?label=crates.io&logo=rust)](https://crates.io/crates/vectis-crdt)
[![docs.rs](https://img.shields.io/docsrs/vectis-crdt?logo=docs.rs)](https://docs.rs/vectis-crdt)
[![License: MIT](https://img.shields.io/badge/License-MIT-green)](LICENSE)

**vectis** (lat.) — arrow, vector.

A Rust CRDT library for ordered collections of mutable objects. It provides **Strong Eventual Consistency** for sequences of richly-attributed items, built for collaborative canvases where deterministic z-order and independently mutable properties matter.

The repository now contains:

| Component | Purpose |
|---------|---------|
| `crates/vectis-crdt` | Core Rust CRDT library |
| `crates/vectis-protocol` | Shared binary client/server WebSocket envelope |
| `crates/app-core` | Platform-independent demo application logic |
| `crates/wasm_demo` | Browser host adapter using `web-sys` and Canvas2D |
| `crates/vectis-server` | In-memory WebSocket room server for the demo |

---

## Installation

```toml
# Cargo.toml
vectis-crdt = "0.1"

# Optional LZ4 compression for larger payloads
vectis-crdt = { version = "0.1", features = ["compress"] }
```

---

## Quick Start

```rust
use vectis_crdt::document::Document;
use vectis_crdt::stroke::{StrokeData, StrokePoint, StrokeProperties, ToolKind};
use vectis_crdt::types::{ActorId, OpId};

let mut doc_a = Document::new(ActorId(1));
let mut doc_b = Document::new(ActorId(2));

let points: Box<[StrokePoint]> = vec![
    StrokePoint::new(0.0, 0.0, 1.0),
    StrokePoint::new(10.0, 10.0, 0.8),
].into();

let data = StrokeData::new(points, ToolKind::Pen);
let props = StrokeProperties::new(0xFF0000FF, 2.0, 1.0, OpId::ZERO);
doc_a.insert_stroke(data, props);

let ops = doc_a.take_pending_ops();
let bytes = vectis_crdt::encoding::encode_update(&ops);

let remote_ops = vectis_crdt::encoding::decode_update(&bytes).unwrap();
for op in remote_ops {
    doc_b.apply_remote(op);
}

assert_eq!(doc_a.visible_stroke_ids(), doc_b.visible_stroke_ids());
```

---

## Client/Server Browser Demo

The browser demo now uses a real client/server model. `crates/app-core` owns one local client document, CRDT orchestration, and render view models. `crates/wasm_demo` translates browser input/events into `app-core` calls, sends binary WebSocket frames, and renders with Canvas2D. `crates/vectis-server` assigns actor IDs, sends a full snapshot on join, applies accepted updates to room state, and broadcasts them to other clients.

Run the WebSocket server:

```bash
./build.sh server
```

Build the Wasm client:

```bash
./build.sh demo
```

For an optimized build:

```bash
./build.sh demo:release
```

Serve the repository root and open the demo:

```bash
python -m http.server 8080
```

```text
http://localhost:8080/crates/wasm_demo/#demo
```

Open the same URL in multiple tabs to collaborate in the same room. The room id is the URL hash.

---

## Features

- **Binary wire format** — LEB128 varints + little-endian floats.
- **Delta sync primitives** — vector-clock state vectors and compact operation updates.
- **Causal delivery buffer** — out-of-order operations can be held until dependencies arrive.
- **Incremental tombstone GC** — MVV-gated, with origin re-parenting to preserve z-order in snapshots.
- **Viewport-oriented data model** — AABB per stroke for efficient culling by renderers.
- **RDP simplification** — configurable, iterative path simplification for high-frequency stylus input.
- **Ephemeral cursor awareness** — TTL-based cursor state, separate from CRDT history.
- **Local undo** — session-local undo stack that emits real delete operations.
- **Optional LZ4 compression** — feature-gated with `compress`.

---

## Feature Flags

| Feature | Default | Description |
|---------|---------|-------------|
| `compress` | no | Enables `lz4_flex` compression helpers |

---

## Tests

```bash
cargo test
cargo test --release
```

Property tests verify convergence, commutativity, idempotency, delete convergence, causal-buffer convergence, and snapshot round-trip integrity. `app-core` also has behavior tests for demo networking, sync, undo, and reset flows.

---

## Architecture

See [ARCHITECTURE.md](ARCHITECTURE.md) for the full design rationale. Historical packaging notes from an earlier npm/TypeScript/Python direction are preserved under [`docs_historical/`](docs_historical/).

---

## License

MIT
