//! # vectis-crdt
//!
//! CRDT engine for collaborative whiteboards.
//!
//! **vectis** (lat.) = arrow, vector.
//!
//! ## Architecture
//!
//! See [`ARCHITECTURE.md`] at the crate root for a comprehensive technical
//! description of every design decision, algorithm, and trade-off.
//!
//! ## Quick overview
//!
//! The whiteboard state is two independent CRDTs:
//!
//! - **[`rga::RgaArray`]** — YATA-style Replicated Growable Array for z-order.
//!   Each item is a stroke (not a point), so `n` is in the hundreds, not millions.
//! - **[`stroke::LwwRegister`]** — Last-Write-Wins register per stroke property
//!   (color, width, opacity, transform). Independent registers enable granular
//!   concurrent merges without conflict.
//!
//! The root entry point is [`document::Document`]. The Wasm-facing API lives in
//! [`wasm_bridge::WasmDocument`] (feature `wasm`).
//!
//! ## Feature flags
//!
//! | Feature | Default | Description |
//! |---------|---------|-------------|
//! | `wasm` | yes | Enables wasm-bindgen and the JS API |
//! | `compress` | no | Enables LZ4 compression for updates > 200 B |
//!
//! ## Module map
//!
//! | Module | Role |
//! |--------|------|
//! | [`types`] | `ActorId`, `LamportTs`, `OpId`, `VectorClock` |
//! | [`rga`] | YATA-style RGA, `RgaArray`, `RgaItem`, `ItemState` |
//! | [`stroke`] | `StrokePoint`, `StrokeData`, `Aabb`, `LwwRegister`, `StrokeProperties` |
//! | [`document`] | `Document` root, all CRDT operations, `LwwMap` |
//! | [`gc`] | Incremental tombstone GC with MVV gating and origin re-parenting |
//! | [`encoding`] | Binary wire format: LEB128 varints + LE floats |
//! | [`causal_buffer`] | Buffers out-of-order remote ops until causally deliverable |
//! | [`awareness`] | Ephemeral cursor positions (TTL-based, not CRDT) |
//! | [`compression`] | LZ4 feature-gated threshold compression |
//! | [`error`] | `VectisError`, `VectisResult` |
//! | [`wasm_bridge`] | `WasmDocument`: zero-copy JS API (feature `wasm`) |

pub mod awareness;
pub mod causal_buffer;
pub mod compression;
pub mod document;
pub mod encoding;
pub mod error;
pub mod gc;
pub mod rga;
pub mod stroke;
pub mod types;

#[cfg(feature = "wasm")]
pub mod wasm_bridge;

#[cfg(feature = "python")]
pub mod python;
