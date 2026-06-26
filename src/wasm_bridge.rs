//! Wasm-bindgen exports. JS calls these from the browser.
//!
//! Design goals:
//!   1. Minimize JS↔Wasm boundary crossings (each costs ~50-200ns).
//!   2. Zero-copy for hot-path data (points buffer, render data).
//!   3. Batch local operations; flush to WebSocket on rAF.
//!   4. All remote ops go through the CausalBuffer for correct ordering.

use wasm_bindgen::prelude::*;

use crate::awareness::{decode_cursor, encode_cursor, AwarenessStore};
use crate::causal_buffer::CausalBuffer;
use crate::document::Document;
use crate::encoding::{
    decode_op_id, decode_snapshot, decode_update, decode_vector_clock, encode_op_id,
    encode_snapshot, encode_state_vector, encode_stroke_ids, encode_update,
};
use crate::gc::GcConfig;
use crate::rga::StrokeId;
use crate::stroke::{Aabb, StrokeData, StrokePoint, StrokeProperties, ToolKind, Transform2D};
use crate::types::ActorId;

// ─── WasmDocument ────────────────────────────────────────────────────────────

/// Opaque handle to a document. JS only ever sees this type.
#[wasm_bindgen]
pub struct WasmDocument {
    inner: Document,
    /// Causal buffer for out-of-order remote operations.
    buffer: CausalBuffer,
    /// Ephemeral awareness state (peer cursors).
    awareness: AwarenessStore,
    /// Render data buffer — reused across frames to avoid allocation.
    render_buf: Vec<u8>,
}

#[wasm_bindgen]
impl WasmDocument {
    // ─── Lifecycle ───────────────────────────────────────────────────────────

    /// Create a new empty document for the given actor.
    #[wasm_bindgen(constructor)]
    pub fn new(actor_id: u64) -> Self {
        Self {
            inner: Document::new(ActorId(actor_id)),
            buffer: CausalBuffer::new(),
            awareness: AwarenessStore::new(),
            render_buf: Vec::new(),
        }
    }

    /// Load a document from a binary snapshot (full state transfer).
    /// Used on reconnect or initial load.
    #[wasm_bindgen]
    pub fn from_snapshot(actor_id: u64, snapshot: &[u8]) -> Result<WasmDocument, JsValue> {
        let doc = decode_snapshot(snapshot, ActorId(actor_id))
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        Ok(Self {
            inner: doc,
            buffer: CausalBuffer::new(),
            awareness: AwarenessStore::new(),
            render_buf: Vec::new(),
        })
    }

    // ─── Local Operations ────────────────────────────────────────────────────

    /// Insert a stroke. Receives points as a raw Float32Array for zero-copy.
    ///
    /// Buffer layout: `[x0, y0, pressure0, x1, y1, pressure1, ...]`
    /// Each point = 3 × f32 = 12 bytes.
    ///
    /// Returns the StrokeId as 16 bytes (lamport u64 LE + actor u64 LE).
    ///
    /// Auto-simplification (RDP) is applied if `document.simplify_epsilon > 0`.
    #[wasm_bindgen]
    pub fn insert_stroke(
        &mut self,
        points_raw: &[f32],
        tool: u8,
        color: u32,
        stroke_width: f32,
        opacity: f32,
    ) -> Box<[u8]> {
        let points = parse_points(points_raw);
        let data = StrokeData::new(points, ToolKind::from_u8(tool));
        let props = StrokeProperties::new(color, stroke_width, opacity, crate::types::OpId::ZERO);
        let stroke_id = self.inner.insert_stroke(data, props);
        encode_op_id(&stroke_id)
    }

    /// Delete a stroke by its 16-byte ID.
    #[wasm_bindgen]
    pub fn delete_stroke(&mut self, stroke_id: &[u8]) -> bool {
        let target = decode_op_id(stroke_id);
        self.inner.delete_stroke(target)
    }

    /// Update a stroke property.
    /// `property_key`: 0=color(u32 LE), 1=stroke_width(f32 LE),
    ///                 2=opacity(f32 LE), 3=transform(6×f32 LE = 24B)
    #[wasm_bindgen]
    pub fn update_stroke_property(
        &mut self,
        stroke_id: &[u8],
        property_key: u8,
        value_raw: &[u8],
    ) -> bool {
        let target = decode_op_id(stroke_id);
        match property_key {
            0 => {
                if value_raw.len() < 4 {
                    return false;
                }
                let v = u32::from_le_bytes(value_raw[0..4].try_into().unwrap());
                self.inner.update_color(target, v)
            }
            1 => {
                if value_raw.len() < 4 {
                    return false;
                }
                let v = f32::from_le_bytes(value_raw[0..4].try_into().unwrap());
                self.inner.update_stroke_width(target, v)
            }
            2 => {
                if value_raw.len() < 4 {
                    return false;
                }
                let v = f32::from_le_bytes(value_raw[0..4].try_into().unwrap());
                self.inner.update_opacity(target, v)
            }
            3 => {
                if value_raw.len() < 24 {
                    return false;
                }
                let t = decode_transform_from_bytes(value_raw);
                self.inner.update_transform(target, t)
            }
            _ => false,
        }
    }

    // ─── Sync / Merge ────────────────────────────────────────────────────────

    /// Apply a binary update received from the WebSocket.
    /// Operations are passed through the causal buffer to guarantee
    /// correct ordering even when packets arrive out of order.
    ///
    /// Returns a flat array of affected StrokeIds (16 bytes each)
    /// so JS knows which strokes to re-render.
    #[wasm_bindgen]
    pub fn apply_update(&mut self, update: &[u8]) -> Box<[u8]> {
        let ops = match decode_update(update) {
            Ok(ops) => ops,
            Err(_) => return Box::new([]), // malformed payload — skip silently
        };
        let mut changed: Vec<StrokeId> = Vec::with_capacity(ops.len());

        for op in ops {
            match self.inner.apply_remote_buffered(op, &mut self.buffer) {
                Ok(ids) => {
                    for id in ids {
                        if !changed.contains(&id) {
                            changed.push(id);
                        }
                    }
                }
                Err(_) => {
                    // Buffer overflow — op is dropped. In production this
                    // triggers a full state sync request to the server.
                }
            }
        }

        encode_stroke_ids(&changed).into()
    }

    /// Number of operations currently waiting in the causal buffer.
    /// Non-zero means we received out-of-order ops. Expected to drain to 0
    /// once the missing dependency arrives.
    #[wasm_bindgen]
    pub fn causal_buffer_len(&self) -> usize {
        self.buffer.len()
    }

    /// Encode and drain pending operations into a binary update blob.
    #[wasm_bindgen]
    pub fn encode_pending_update(&mut self) -> Box<[u8]> {
        let ops = self.inner.take_pending_ops();
        if ops.is_empty() {
            return Box::new([]);
        }
        encode_update(&ops).into()
    }

    /// Encode the local VectorClock as a state vector for delta sync.
    #[wasm_bindgen]
    pub fn encode_state_vector(&self) -> Box<[u8]> {
        encode_state_vector(&self.inner.version).into()
    }

    /// Encode a full snapshot of this document.
    #[wasm_bindgen]
    pub fn encode_snapshot(&self) -> Box<[u8]> {
        encode_snapshot(&self.inner).into()
    }

    // ─── Awareness (Ephemeral Cursor State) ──────────────────────────────────

    /// Apply a cursor update received from a peer.
    /// `cursor_bytes` must be exactly 28 bytes.
    /// Returns true if the state was accepted (newer timestamp).
    #[wasm_bindgen]
    pub fn apply_cursor_update(&mut self, cursor_bytes: &[u8]) -> bool {
        if let Some(state) = decode_cursor(cursor_bytes) {
            self.awareness.update(state)
        } else {
            false
        }
    }

    /// Encode all known cursor states as a bulk buffer (N × 28 bytes).
    #[wasm_bindgen]
    pub fn get_all_cursors(&self) -> Box<[u8]> {
        self.awareness.encode_all().into()
    }

    /// Remove a peer's cursor (on disconnect).
    #[wasm_bindgen]
    pub fn remove_cursor(&mut self, actor_id: u64) {
        self.awareness.remove(ActorId(actor_id));
    }

    /// Evict cursor states older than the configured TTL.
    /// `now_ms`: current `Date.now()` value from JS.
    /// Returns the number of evicted actors.
    #[wasm_bindgen]
    pub fn evict_stale_cursors(&mut self, now_ms: u64) -> usize {
        self.awareness.evict_stale(now_ms)
    }

    /// Set the cursor TTL in milliseconds (default: 30_000).
    #[wasm_bindgen]
    pub fn set_cursor_ttl(&mut self, ttl_ms: u64) {
        self.awareness.ttl_ms = ttl_ms;
    }

    /// Build a 28-byte cursor update for the local actor to broadcast.
    /// `now_ms` = Date.now(), `color` = 0xRRGGBBAA.
    #[wasm_bindgen]
    pub fn encode_local_cursor(&self, x: f32, y: f32, now_ms: u64, color: u32) -> Box<[u8]> {
        let state = crate::awareness::CursorState::new(self.inner.local_actor, x, y, now_ms, color);
        Box::new(encode_cursor(&state))
    }

    // ─── Rendering (Zero-Copy Hot Path) ──────────────────────────────────────

    /// Write all visible stroke data into an internal buffer.
    /// Returns a raw pointer into Wasm linear memory.
    /// Pair with `get_render_data_len()`.
    ///
    /// Buffer layout per stroke:
    ///   [stroke_id: 16B][point_count: u32 LE][tool: u8]
    ///   [color: u32 LE][stroke_width: f32 LE][opacity: f32 LE]
    ///   [transform: 6×f32 LE = 24B]
    ///   [points: point_count × (x f32, y f32, pressure f32)]
    ///
    /// CAUTION: pointer invalidated by any subsequent Wasm allocation.
    /// Read within the same rAF callback before calling any mutating method.
    #[wasm_bindgen]
    pub fn build_render_data(&mut self) -> *const u8 {
        self.render_buf.clear();
        for id in self.inner.visible_stroke_ids() {
            if let Some((data, props)) = self.inner.get_stroke(&id) {
                write_stroke_to_buf(&mut self.render_buf, &id, data, props);
            }
        }
        self.render_buf.as_ptr()
    }

    /// Write only the visible strokes whose bounds intersect the given viewport
    /// rectangle into the internal buffer. Returns a raw pointer.
    ///
    /// This is the **primary rendering API for large documents** — strokes
    /// entirely outside the viewport are skipped without any point iteration.
    ///
    /// # Parameters
    /// - `vx0`, `vy0`, `vx1`, `vy1`: viewport rectangle in canvas coordinates.
    /// - `stroke_expand`: padding added to each stroke's bounds before the
    ///   intersection test. Pass `max_stroke_width / 2` to avoid clipping thick
    ///   strokes at the viewport edge. Pass `0` for a tight test.
    ///
    /// # Example
    /// ```js
    /// // Canvas coords visible in current view:
    /// const ptr = doc.build_render_data_viewport(
    ///   camX, camY, camX + viewW, camY + viewH, maxStrokeWidth / 2
    /// );
    /// ```
    #[wasm_bindgen]
    pub fn build_render_data_viewport(
        &mut self,
        vx0: f32,
        vy0: f32,
        vx1: f32,
        vy1: f32,
        stroke_expand: f32,
    ) -> *const u8 {
        let viewport = Aabb {
            min_x: vx0,
            min_y: vy0,
            max_x: vx1,
            max_y: vy1,
        };
        self.render_buf.clear();

        for id in self.inner.visible_stroke_ids() {
            if let Some((data, props)) = self.inner.get_stroke(&id) {
                // Compute effective bounds (handle transforms + padding).
                let effective_bounds = if props.transform.value.is_identity() {
                    data.bounds.expanded(stroke_expand)
                } else {
                    data.bounds
                        .transform(&props.transform.value)
                        .expanded(stroke_expand)
                };

                if !effective_bounds.intersects(&viewport) {
                    continue; // Culled — outside viewport.
                }

                write_stroke_to_buf(&mut self.render_buf, &id, data, props);
            }
        }

        self.render_buf.as_ptr()
    }

    /// Byte length of the last render data buffer (from either `build_render_data`
    /// or `build_render_data_viewport`).
    #[wasm_bindgen]
    pub fn get_render_data_len(&self) -> usize {
        self.render_buf.len()
    }

    /// Get a stroke's full data as an owned buffer (same layout as build_render_data).
    /// Returns an empty slice if the stroke is not found.
    /// Safe to keep across Wasm calls (owned allocation).
    #[wasm_bindgen]
    pub fn get_stroke_data_owned(&self, stroke_id: &[u8]) -> Box<[u8]> {
        let id = decode_op_id(stroke_id);
        if let Some((data, props)) = self.inner.get_stroke(&id) {
            let mut buf = Vec::new();
            write_stroke_to_buf(&mut buf, &id, data, props);
            buf.into_boxed_slice()
        } else {
            Box::new([])
        }
    }

    /// Zero-copy pointer to a stroke's points. Returns null if not found.
    #[wasm_bindgen]
    pub fn get_stroke_points_ptr(&self, stroke_id: &[u8]) -> *const f32 {
        let id = decode_op_id(stroke_id);
        self.inner
            .get_stroke(&id)
            .map(|(d, _)| d.points.as_ptr() as *const f32)
            .unwrap_or(std::ptr::null())
    }

    /// Number of points in a stroke (0 if not found).
    #[wasm_bindgen]
    pub fn get_stroke_point_count(&self, stroke_id: &[u8]) -> usize {
        let id = decode_op_id(stroke_id);
        self.inner
            .get_stroke(&id)
            .map(|(d, _)| d.points.len())
            .unwrap_or(0)
    }

    /// All visible stroke IDs in z-order as a flat byte array (16B each).
    #[wasm_bindgen]
    pub fn get_visible_stroke_ids(&self) -> Box<[u8]> {
        encode_stroke_ids(&self.inner.visible_stroke_ids()).into()
    }

    /// Get the bounding box of a stroke as `[min_x, min_y, max_x, max_y]`
    /// encoded as 4 × f32 LE (16 bytes total).
    /// Returns an empty slice if the stroke is not found.
    ///
    /// The returned bounds are tight (no padding). Add `stroke_width / 2`
    /// in JS if needed for accurate culling.
    #[wasm_bindgen]
    pub fn get_stroke_bounds(&self, stroke_id: &[u8]) -> Box<[u8]> {
        let id = decode_op_id(stroke_id);
        if let Some((data, _)) = self.inner.get_stroke(&id) {
            let b = &data.bounds;
            let mut out = [0u8; 16];
            out[0..4].copy_from_slice(&b.min_x.to_le_bytes());
            out[4..8].copy_from_slice(&b.min_y.to_le_bytes());
            out[8..12].copy_from_slice(&b.max_x.to_le_bytes());
            out[12..16].copy_from_slice(&b.max_y.to_le_bytes());
            Box::new(out)
        } else {
            Box::new([])
        }
    }

    // ─── Simplification Control ───────────────────────────────────────────────

    /// Configure the RDP epsilon for automatic point simplification on insert.
    /// `0.0` disables auto-simplification.
    /// Default: `0.5` (sub-pixel accuracy on high-DPI displays).
    #[wasm_bindgen]
    pub fn set_simplify_epsilon(&mut self, epsilon: f32) {
        self.inner.simplify_epsilon = epsilon;
    }

    /// Manually simplify an existing stroke's points using RDP.
    /// Useful to retroactively reduce old strokes loaded from a snapshot.
    ///
    /// Returns the number of points removed (0 if stroke not found or already minimal).
    #[wasm_bindgen]
    pub fn simplify_stroke(&mut self, stroke_id: &[u8], epsilon: f32) -> usize {
        let id = decode_op_id(stroke_id);
        self.inner.simplify_stroke(id, epsilon)
    }

    // ─── Undo ─────────────────────────────────────────────────────────────────

    /// Undo the last stroke drawn by the local actor.
    ///
    /// Generates a `DeleteStroke` operation visible to all peers (collaborative
    /// undo: other users see the stroke disappear). If the stroke was already
    /// deleted remotely, the next most-recent stroke is tried instead.
    ///
    /// Returns the deleted `StrokeId` as 16 bytes, or an empty slice if the
    /// undo stack is empty.
    ///
    /// The resulting delete op is queued in `pending_ops` — flush it via
    /// `encode_pending_update()` as usual.
    #[wasm_bindgen]
    pub fn undo(&mut self) -> Box<[u8]> {
        match self.inner.undo_last_stroke() {
            Some(id) => encode_op_id(&id),
            None => Box::new([]),
        }
    }

    /// Number of strokes currently available to undo (local actor only).
    #[wasm_bindgen]
    pub fn undo_depth(&self) -> usize {
        self.inner.undo_depth()
    }

    // ─── GC Control ──────────────────────────────────────────────────────────

    /// Update the Minimum Version Vector (broadcast by the server) and
    /// optionally run incremental GC.
    /// Returns a JSON string with GC stats, or null if GC didn't run.
    #[wasm_bindgen]
    pub fn update_min_version_and_gc(&mut self, mvv_bytes: &[u8]) -> JsValue {
        let mvv = decode_vector_clock(mvv_bytes);
        match self.inner.update_min_version(mvv, &GcConfig::default()) {
            Some(r) => JsValue::from_str(&format!(
                "{{\"removed\":{},\"freed\":{},\"gen\":{},\"partial\":{}}}",
                r.tombstones_removed, r.bytes_freed_estimate, r.generation, r.partial,
            )),
            None => JsValue::NULL,
        }
    }

    // ─── Stats / Debug ────────────────────────────────────────────────────────

    /// JSON string with document statistics.
    #[wasm_bindgen]
    pub fn stats(&self) -> String {
        let s = self.inner.stats();
        format!(
            "{{\"total_items\":{},\"visible\":{},\"tombstones\":{},\
             \"tombstone_ratio\":{:.3},\"gc_gen\":{},\"pending_ops\":{},\
             \"causal_buffer\":{},\"awareness_peers\":{}}}",
            s.total_items,
            s.visible_items,
            s.tombstones,
            s.tombstone_ratio,
            s.gc_generation,
            s.pending_ops,
            self.buffer.len(),
            self.awareness.actor_count(),
        )
    }

    #[wasm_bindgen]
    pub fn actor_id(&self) -> u64 {
        self.inner.local_actor.0
    }

    #[wasm_bindgen]
    pub fn lamport(&self) -> u64 {
        self.inner.clock.0
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn parse_points(raw: &[f32]) -> Box<[StrokePoint]> {
    raw.chunks_exact(3)
        .map(|c| StrokePoint::new(c[0], c[1], c[2]))
        .collect::<Vec<_>>()
        .into()
}

/// Write one stroke's header + points into `buf`.
/// Shared by `build_render_data` and `build_render_data_viewport`.
///
/// Layout:
///   stroke_id:    16 B
///   point_count:   4 B (u32 LE)
///   tool:          1 B
///   color:         4 B (u32 LE)
///   stroke_width:  4 B (f32 LE)
///   opacity:       4 B (f32 LE)
///   transform:    24 B (6 × f32 LE)
///   points:       N × 12 B (x, y, pressure — each f32 LE)
fn write_stroke_to_buf(
    buf: &mut Vec<u8>,
    id: &crate::rga::StrokeId,
    data: &crate::stroke::StrokeData,
    props: &crate::stroke::StrokeProperties,
) {
    buf.extend_from_slice(&encode_op_id(id));
    buf.extend_from_slice(&(data.points.len() as u32).to_le_bytes());
    buf.push(data.tool as u8);
    buf.extend_from_slice(&props.color.value.to_le_bytes());
    buf.extend_from_slice(&props.stroke_width.value.to_le_bytes());
    buf.extend_from_slice(&props.opacity.value.to_le_bytes());
    let t = &props.transform.value;
    for &v in &[t.a, t.b, t.c, t.d, t.tx, t.ty] {
        buf.extend_from_slice(&v.to_le_bytes());
    }
    for pt in data.points.iter() {
        buf.extend_from_slice(&pt.x.to_le_bytes());
        buf.extend_from_slice(&pt.y.to_le_bytes());
        buf.extend_from_slice(&pt.pressure.to_le_bytes());
    }
}

fn decode_transform_from_bytes(bytes: &[u8]) -> Transform2D {
    let r = |off: usize| -> f32 {
        bytes
            .get(off..off + 4)
            .and_then(|s| s.try_into().ok())
            .map(f32::from_le_bytes)
            .unwrap_or(0.0)
    };
    Transform2D {
        a: r(0),
        b: r(4),
        c: r(8),
        d: r(12),
        tx: r(16),
        ty: r(20),
    }
}

// ─── Free functions ───────────────────────────────────────────────────────────

/// Returns the number of operations in a binary update blob.
#[wasm_bindgen]
pub fn vectis_op_count(update: &[u8]) -> usize {
    decode_update(update).map(|ops| ops.len()).unwrap_or(0)
}

/// Encode an OpId to 16 bytes.
#[wasm_bindgen]
pub fn vectis_encode_op_id(lamport: u64, actor: u64) -> Box<[u8]> {
    let id = crate::types::OpId {
        lamport: crate::types::LamportTs(lamport),
        actor: crate::types::ActorId(actor),
    };
    encode_op_id(&id)
}
