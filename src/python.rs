//! PyO3 bindings — Python API for vectis-crdt.
//!
//! Build with maturin:
//!   maturin develop --no-default-features --features python
//!   maturin build   --no-default-features --features python --release
//!   maturin publish --no-default-features --features python --release

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;

use crate::document::Document;
use crate::encoding::{
    decode_op_id, decode_snapshot, decode_update, encode_op_id, encode_snapshot,
    encode_state_vector, encode_update,
};
use crate::rga::StrokeId;
use crate::stroke::{StrokeData, StrokePoint, StrokeProperties, ToolKind};
use crate::types::{ActorId, OpId};

// ─── Helpers ──────────────────────────────────────────────────────────────────

fn id_to_bytes(id: StrokeId) -> Vec<u8> {
    encode_op_id(&id).into()
}

fn bytes_to_id(bytes: &[u8]) -> PyResult<StrokeId> {
    if bytes.len() != 16 {
        return Err(PyValueError::new_err(format!(
            "stroke_id must be exactly 16 bytes, got {}",
            bytes.len()
        )));
    }
    Ok(decode_op_id(bytes))
}

// ─── Document ─────────────────────────────────────────────────────────────────

/// CRDT document. One instance per client/peer.
///
/// All mutating methods accumulate pending operations internally.
/// Call `encode_pending_update()` to drain and encode them for sending.
#[pyclass(name = "Document")]
pub struct PyDocument {
    inner: Document,
}

#[pymethods]
impl PyDocument {
    /// Create a new empty document.
    ///
    /// `actor_id` must be a unique non-zero integer per client. Use a
    /// server-assigned u64 or a random 64-bit value.
    #[new]
    fn new(actor_id: u64) -> PyResult<Self> {
        if actor_id == 0 {
            return Err(PyValueError::new_err("actor_id 0 is reserved"));
        }
        Ok(Self { inner: Document::new(ActorId(actor_id)) })
    }

    /// Restore a document from a binary snapshot produced by `encode_snapshot()`.
    #[staticmethod]
    fn from_snapshot(actor_id: u64, snapshot: &[u8]) -> PyResult<Self> {
        let doc = decode_snapshot(snapshot, ActorId(actor_id))
            .map_err(|e| PyValueError::new_err(e.to_string()))?;
        Ok(Self { inner: doc })
    }

    // ─── Local operations ─────────────────────────────────────────────────────

    /// Insert a stroke.
    ///
    /// Parameters
    /// ----------
    /// points : list of (x, y, pressure) tuples — pressure in 0.0–1.0.
    /// tool   : int  — 0=Pen 1=Eraser 2=Marker 3=Laser 4=Shape 5=Arrow
    /// color  : int  — 0xRRGGBBAA
    /// stroke_width : float
    /// opacity      : float — 0.0–1.0
    ///
    /// Returns
    /// -------
    /// bytes : 16-byte stroke ID (lamport u64 LE + actor u64 LE).
    fn insert_stroke(
        &mut self,
        points: Vec<(f32, f32, f32)>,
        tool: u8,
        color: u32,
        stroke_width: f32,
        opacity: f32,
    ) -> PyResult<Vec<u8>> {
        if points.is_empty() {
            return Err(PyValueError::new_err("points must not be empty"));
        }
        let pts: Box<[StrokePoint]> = points
            .iter()
            .map(|&(x, y, p)| StrokePoint::new(x, y, p))
            .collect();
        let data = StrokeData::new(pts, ToolKind::from_u8(tool));
        let props = StrokeProperties::new(color, stroke_width, opacity, OpId::ZERO);
        let id = self.inner.insert_stroke(data, props);
        Ok(id_to_bytes(id))
    }

    /// Delete a stroke by its 16-byte ID.
    ///
    /// Returns True if the stroke was found and tombstoned.
    /// Generates a DeleteStroke op — flush via `encode_pending_update()`.
    fn delete_stroke(&mut self, stroke_id: &[u8]) -> PyResult<bool> {
        let id = bytes_to_id(stroke_id)?;
        Ok(self.inner.delete_stroke(id))
    }

    /// Update stroke color (0xRRGGBBAA).
    fn update_color(&mut self, stroke_id: &[u8], color: u32) -> PyResult<bool> {
        let id = bytes_to_id(stroke_id)?;
        Ok(self.inner.update_color(id, color))
    }

    /// Update stroke width.
    fn update_stroke_width(&mut self, stroke_id: &[u8], width: f32) -> PyResult<bool> {
        let id = bytes_to_id(stroke_id)?;
        Ok(self.inner.update_stroke_width(id, width))
    }

    /// Update stroke opacity (0.0–1.0).
    fn update_opacity(&mut self, stroke_id: &[u8], opacity: f32) -> PyResult<bool> {
        let id = bytes_to_id(stroke_id)?;
        Ok(self.inner.update_opacity(id, opacity))
    }

    // ─── Undo ─────────────────────────────────────────────────────────────────

    /// Undo the last stroke drawn by the local actor.
    ///
    /// Returns the 16-byte ID of the deleted stroke, or None if the undo
    /// stack is empty. Skips strokes already deleted by a remote peer.
    ///
    /// Generates a DeleteStroke op — flush via `encode_pending_update()`.
    fn undo(&mut self) -> Option<Vec<u8>> {
        self.inner.undo_last_stroke().map(id_to_bytes)
    }

    /// Number of strokes available to undo (local actor only).
    fn undo_depth(&self) -> usize {
        self.inner.undo_depth()
    }

    // ─── Sync ─────────────────────────────────────────────────────────────────

    /// Apply a binary update received from a peer.
    ///
    /// Operations are applied in causal order. Call this for every update
    /// received over the network.
    fn apply_update(&mut self, data: &[u8]) -> PyResult<()> {
        let ops = decode_update(data)
            .map_err(|e| PyValueError::new_err(e.to_string()))?;
        for op in ops {
            self.inner.apply_remote(op);
        }
        Ok(())
    }

    /// Encode and drain all pending operations into a binary blob.
    ///
    /// Send this blob to peers after any local mutation (insert, delete, undo).
    fn encode_pending_update(&mut self) -> Vec<u8> {
        let ops = self.inner.take_pending_ops();
        if ops.is_empty() {
            return Vec::new();
        }
        encode_update(&ops).into()
    }

    /// Encode the local vector clock as a state vector for delta sync.
    ///
    /// Send this to a peer/server; they reply with only the ops you are missing.
    fn encode_state_vector(&self) -> Vec<u8> {
        encode_state_vector(&self.inner.version).into()
    }

    /// Encode the full document as a binary snapshot.
    ///
    /// Use for initial load, reconnect, or persisting to disk.
    fn encode_snapshot(&self) -> Vec<u8> {
        encode_snapshot(&self.inner).into()
    }

    // ─── Queries ──────────────────────────────────────────────────────────────

    /// All visible stroke IDs in z-order (bottom to top).
    ///
    /// Returns a list of 16-byte `bytes` objects.
    fn visible_stroke_ids(&self) -> Vec<Vec<u8>> {
        self.inner.visible_stroke_ids().into_iter().map(id_to_bytes).collect()
    }

    // ─── Simplification ───────────────────────────────────────────────────────

    /// Configure the RDP epsilon for automatic point simplification on insert.
    ///
    /// Default: 0.5 (sub-pixel accuracy on high-DPI). Set to 0.0 to disable.
    fn set_simplify_epsilon(&mut self, epsilon: f32) {
        self.inner.simplify_epsilon = epsilon;
    }

    /// Manually simplify an existing stroke's points using Ramer-Douglas-Peucker.
    ///
    /// Returns the number of points removed (0 if not found or already minimal).
    fn simplify_stroke(&mut self, stroke_id: &[u8], epsilon: f32) -> PyResult<usize> {
        let id = bytes_to_id(stroke_id)?;
        Ok(self.inner.simplify_stroke(id, epsilon))
    }

    // ─── Meta ─────────────────────────────────────────────────────────────────

    /// Actor ID of this document instance.
    fn actor_id(&self) -> u64 {
        self.inner.local_actor.0
    }

    /// Document statistics as a JSON string.
    fn stats(&self) -> String {
        let s = self.inner.stats();
        format!(
            r#"{{"total":{},"visible":{},"tombstones":{},"tombstone_ratio":{:.4},"gc_generation":{},"pending_ops":{}}}"#,
            s.total_items,
            s.visible_items,
            s.tombstones,
            s.tombstone_ratio,
            s.gc_generation,
            s.pending_ops,
        )
    }
}

// ─── Module ───────────────────────────────────────────────────────────────────

#[pymodule]
pub fn vectis_crdt(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyDocument>()?;
    Ok(())
}
