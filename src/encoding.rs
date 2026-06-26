//! Binary encoding/decoding for operations and state.
//!
//! Wire format uses unsigned LEB128 varints for all integer fields
//! and little-endian IEEE 754 for floats.
//!
//! Operation type tags:
//!   0x01 = InsertStroke
//!   0x02 = DeleteStroke
//!   0x03 = UpdateProperty
//!   0x04 = UpdateMetadata
//!
//! PropertyUpdate sub-tags:
//!   0x00 = Color (u32)
//!   0x01 = StrokeWidth (f32)
//!   0x02 = Opacity (f32)
//!   0x03 = Transform (6 × f32)
//!
//! MetadataValue type tags:
//!   0x00 = F64
//!   0x01 = Bool
//!   0x02 = U32
//!   0x03 = String

use crate::document::{
    MetadataKey, MetadataValue, Operation, PropertyUpdate, MAX_ACTORS, MAX_POINTS_PER_STROKE,
    MAX_STROKES,
};
use crate::error::VectisError;
use crate::error::VectisResult;
use crate::rga::StrokeId;
use crate::stroke::{StrokeData, StrokePoint, StrokeProperties, ToolKind, Transform2D};
use crate::types::{ActorId, LamportTs, OpId, VectorClock};

/// Current snapshot binary format version.
pub const SNAPSHOT_VERSION: u8 = 1;

// ─── Varint helpers ───────────────────────────────────────────────────────────

/// Encode a u64 as unsigned LEB128. Returns number of bytes written.
fn encode_varint(mut value: u64, out: &mut Vec<u8>) {
    loop {
        let mut byte = (value & 0x7F) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if value == 0 {
            break;
        }
    }
}

/// Decode a u64 from unsigned LEB128. Returns (value, bytes_consumed).
fn decode_varint(bytes: &[u8]) -> Option<(u64, usize)> {
    let mut result: u64 = 0;
    let mut shift = 0u32;
    for (i, &byte) in bytes.iter().enumerate() {
        let low7 = (byte & 0x7F) as u64;
        result |= low7 << shift;
        shift += 7;
        if byte & 0x80 == 0 {
            return Some((result, i + 1));
        }
        if shift >= 64 {
            return None; // overflow
        }
    }
    None // incomplete
}

// ─── Primitive encode/decode helpers ─────────────────────────────────────────

fn encode_f32(v: f32, out: &mut Vec<u8>) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn encode_u32(v: u32, out: &mut Vec<u8>) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn encode_op_id_into(id: &OpId, out: &mut Vec<u8>) {
    encode_varint(id.lamport.0, out);
    encode_varint(id.actor.0, out);
}

/// Decode an OpId and advance cursor. Returns (OpId, new_cursor).
fn decode_op_id_at(bytes: &[u8], cursor: usize) -> Option<(OpId, usize)> {
    let (lamport, n1) = decode_varint(&bytes[cursor..])?;
    let (actor, n2) = decode_varint(&bytes[cursor + n1..])?;
    Some((
        OpId {
            lamport: LamportTs(lamport),
            actor: ActorId(actor),
        },
        cursor + n1 + n2,
    ))
}

fn decode_f32_at(bytes: &[u8], cursor: usize) -> Option<(f32, usize)> {
    if cursor + 4 > bytes.len() {
        return None;
    }
    let arr: [u8; 4] = bytes[cursor..cursor + 4].try_into().ok()?;
    Some((f32::from_le_bytes(arr), cursor + 4))
}

fn decode_u32_at(bytes: &[u8], cursor: usize) -> Option<(u32, usize)> {
    if cursor + 4 > bytes.len() {
        return None;
    }
    let arr: [u8; 4] = bytes[cursor..cursor + 4].try_into().ok()?;
    Some((u32::from_le_bytes(arr), cursor + 4))
}

fn encode_string(s: &str, out: &mut Vec<u8>) {
    encode_varint(s.len() as u64, out);
    out.extend_from_slice(s.as_bytes());
}

fn decode_string_at(bytes: &[u8], cursor: usize) -> Option<(String, usize)> {
    let (len, n) = decode_varint(&bytes[cursor..])?;
    let end = cursor + n + len as usize;
    if end > bytes.len() {
        return None;
    }
    let s = std::str::from_utf8(&bytes[cursor + n..end])
        .ok()?
        .to_string();
    Some((s, end))
}

// ─── Transform2D ─────────────────────────────────────────────────────────────

fn encode_transform(t: &Transform2D, out: &mut Vec<u8>) {
    encode_f32(t.a, out);
    encode_f32(t.b, out);
    encode_f32(t.c, out);
    encode_f32(t.d, out);
    encode_f32(t.tx, out);
    encode_f32(t.ty, out);
}

fn decode_transform_at(bytes: &[u8], mut cursor: usize) -> Option<(Transform2D, usize)> {
    let (a, c) = decode_f32_at(bytes, cursor)?;
    cursor = c;
    let (b, c) = decode_f32_at(bytes, cursor)?;
    cursor = c;
    let (cc, c) = decode_f32_at(bytes, cursor)?;
    cursor = c;
    let (d, c) = decode_f32_at(bytes, cursor)?;
    cursor = c;
    let (tx, c) = decode_f32_at(bytes, cursor)?;
    cursor = c;
    let (ty, c) = decode_f32_at(bytes, cursor)?;
    cursor = c;
    Some((
        Transform2D {
            a,
            b,
            c: cc,
            d,
            tx,
            ty,
        },
        cursor,
    ))
}

// ─── StrokeData / StrokeProperties ───────────────────────────────────────────

fn encode_stroke_data(data: &StrokeData, out: &mut Vec<u8>) {
    out.push(data.tool as u8);
    encode_varint(data.points.len() as u64, out);
    for pt in data.points.iter() {
        encode_f32(pt.x, out);
        encode_f32(pt.y, out);
        encode_f32(pt.pressure, out);
    }
}

fn decode_stroke_data_at(bytes: &[u8], mut cursor: usize) -> Option<(StrokeData, usize)> {
    if cursor >= bytes.len() {
        return None;
    }
    let tool = ToolKind::from_u8(bytes[cursor]);
    cursor += 1;
    let (count, n) = decode_varint(&bytes[cursor..])?;
    cursor += n;
    // Enforce point limit: reject malformed payloads before allocating.
    if count as usize > MAX_POINTS_PER_STROKE {
        return None;
    }
    let mut points = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let (x, c) = decode_f32_at(bytes, cursor)?;
        cursor = c;
        let (y, c) = decode_f32_at(bytes, cursor)?;
        cursor = c;
        let (p, c) = decode_f32_at(bytes, cursor)?;
        cursor = c;
        points.push(StrokePoint::new(x, y, p));
    }
    Some((StrokeData::new(points.into(), tool), cursor))
}

fn encode_stroke_properties(props: &StrokeProperties, out: &mut Vec<u8>) {
    encode_op_id_into(&props.color.timestamp, out);
    encode_u32(props.color.value, out);
    encode_op_id_into(&props.stroke_width.timestamp, out);
    encode_f32(props.stroke_width.value, out);
    encode_op_id_into(&props.opacity.timestamp, out);
    encode_f32(props.opacity.value, out);
    encode_op_id_into(&props.transform.timestamp, out);
    encode_transform(&props.transform.value, out);
}

fn decode_stroke_properties_at(
    bytes: &[u8],
    mut cursor: usize,
) -> Option<(StrokeProperties, usize)> {
    use crate::stroke::LwwRegister;

    let (color_ts, c) = decode_op_id_at(bytes, cursor)?;
    cursor = c;
    let (color_val, c) = decode_u32_at(bytes, cursor)?;
    cursor = c;

    let (sw_ts, c) = decode_op_id_at(bytes, cursor)?;
    cursor = c;
    let (sw_val, c) = decode_f32_at(bytes, cursor)?;
    cursor = c;

    let (op_ts, c) = decode_op_id_at(bytes, cursor)?;
    cursor = c;
    let (op_val, c) = decode_f32_at(bytes, cursor)?;
    cursor = c;

    let (tr_ts, c) = decode_op_id_at(bytes, cursor)?;
    cursor = c;
    let (tr_val, c) = decode_transform_at(bytes, cursor)?;
    cursor = c;

    Some((
        StrokeProperties {
            color: LwwRegister::new(color_val, color_ts),
            stroke_width: LwwRegister::new(sw_val, sw_ts),
            opacity: LwwRegister::new(op_val, op_ts),
            transform: LwwRegister::new(tr_val, tr_ts),
        },
        cursor,
    ))
}

// ─── MetadataKey / MetadataValue ─────────────────────────────────────────────

fn encode_metadata_key(key: &MetadataKey, out: &mut Vec<u8>) {
    let tag: u8 = match key {
        MetadataKey::ViewportX => 0,
        MetadataKey::ViewportY => 1,
        MetadataKey::ViewportZoom => 2,
        MetadataKey::BackgroundColor => 3,
        MetadataKey::GridEnabled => 4,
        MetadataKey::GridSpacing => 5,
        MetadataKey::Custom(_) => 255,
    };
    out.push(tag);
    if let MetadataKey::Custom(s) = key {
        encode_string(s, out);
    }
}

fn decode_metadata_key_at(bytes: &[u8], cursor: usize) -> Option<(MetadataKey, usize)> {
    if cursor >= bytes.len() {
        return None;
    }
    let tag = bytes[cursor];
    let next = cursor + 1;
    let key = match tag {
        0 => (MetadataKey::ViewportX, next),
        1 => (MetadataKey::ViewportY, next),
        2 => (MetadataKey::ViewportZoom, next),
        3 => (MetadataKey::BackgroundColor, next),
        4 => (MetadataKey::GridEnabled, next),
        5 => (MetadataKey::GridSpacing, next),
        255 => {
            let (s, c) = decode_string_at(bytes, next)?;
            (MetadataKey::Custom(s), c)
        }
        _ => return None,
    };
    Some(key)
}

fn encode_metadata_value(value: &MetadataValue, out: &mut Vec<u8>) {
    match value {
        MetadataValue::F64(v) => {
            out.push(0);
            out.extend_from_slice(&v.to_le_bytes());
        }
        MetadataValue::Bool(v) => {
            out.push(1);
            out.push(*v as u8);
        }
        MetadataValue::U32(v) => {
            out.push(2);
            encode_u32(*v, out);
        }
        MetadataValue::String(s) => {
            out.push(3);
            encode_string(s, out);
        }
    }
}

fn decode_metadata_value_at(bytes: &[u8], cursor: usize) -> Option<(MetadataValue, usize)> {
    if cursor >= bytes.len() {
        return None;
    }
    let tag = bytes[cursor];
    let next = cursor + 1;
    match tag {
        0 => {
            if next + 8 > bytes.len() {
                return None;
            }
            let arr: [u8; 8] = bytes[next..next + 8].try_into().ok()?;
            Some((MetadataValue::F64(f64::from_le_bytes(arr)), next + 8))
        }
        1 => {
            if next >= bytes.len() {
                return None;
            }
            Some((MetadataValue::Bool(bytes[next] != 0), next + 1))
        }
        2 => {
            let (v, c) = decode_u32_at(bytes, next)?;
            Some((MetadataValue::U32(v), c))
        }
        3 => {
            let (s, c) = decode_string_at(bytes, next)?;
            Some((MetadataValue::String(s), c))
        }
        _ => None,
    }
}

// ─── PropertyUpdate ──────────────────────────────────────────────────────────

fn encode_property_update(update: &PropertyUpdate, out: &mut Vec<u8>) {
    match update {
        PropertyUpdate::Color(v) => {
            out.push(0);
            encode_u32(*v, out);
        }
        PropertyUpdate::StrokeWidth(v) => {
            out.push(1);
            encode_f32(*v, out);
        }
        PropertyUpdate::Opacity(v) => {
            out.push(2);
            encode_f32(*v, out);
        }
        PropertyUpdate::Transform(v) => {
            out.push(3);
            encode_transform(v, out);
        }
    }
}

fn decode_property_update_at(bytes: &[u8], cursor: usize) -> Option<(PropertyUpdate, usize)> {
    if cursor >= bytes.len() {
        return None;
    }
    let tag = bytes[cursor];
    let next = cursor + 1;
    match tag {
        0 => {
            let (v, c) = decode_u32_at(bytes, next)?;
            Some((PropertyUpdate::Color(v), c))
        }
        1 => {
            let (v, c) = decode_f32_at(bytes, next)?;
            Some((PropertyUpdate::StrokeWidth(v), c))
        }
        2 => {
            let (v, c) = decode_f32_at(bytes, next)?;
            Some((PropertyUpdate::Opacity(v), c))
        }
        3 => {
            let (v, c) = decode_transform_at(bytes, next)?;
            Some((PropertyUpdate::Transform(v), c))
        }
        _ => None,
    }
}

// ─── Operation encode/decode ──────────────────────────────────────────────────

fn encode_operation(op: &Operation, out: &mut Vec<u8>) {
    match op {
        Operation::InsertStroke {
            id,
            origin_left,
            origin_right,
            data,
            properties,
        } => {
            out.push(0x01);
            encode_op_id_into(id, out);
            encode_op_id_into(origin_left, out);
            encode_op_id_into(origin_right, out);
            encode_stroke_data(data, out);
            encode_stroke_properties(properties, out);
        }
        Operation::DeleteStroke { id, target } => {
            out.push(0x02);
            encode_op_id_into(id, out);
            encode_op_id_into(target, out);
        }
        Operation::UpdateProperty { id, target, update } => {
            out.push(0x03);
            encode_op_id_into(id, out);
            encode_op_id_into(target, out);
            encode_property_update(update, out);
        }
        Operation::UpdateMetadata { id, key, value } => {
            out.push(0x04);
            encode_op_id_into(id, out);
            encode_metadata_key(key, out);
            match value {
                Some(v) => {
                    out.push(1);
                    encode_metadata_value(v, out);
                }
                None => {
                    out.push(0);
                }
            }
        }
    }
}

fn decode_operation_at(bytes: &[u8], cursor: usize) -> Option<(Operation, usize)> {
    if cursor >= bytes.len() {
        return None;
    }
    let tag = bytes[cursor];
    let mut c = cursor + 1;

    match tag {
        0x01 => {
            let (id, nc) = decode_op_id_at(bytes, c)?;
            c = nc;
            let (ol, nc) = decode_op_id_at(bytes, c)?;
            c = nc;
            let (or_, nc) = decode_op_id_at(bytes, c)?;
            c = nc;
            let (data, nc) = decode_stroke_data_at(bytes, c)?;
            c = nc;
            let (properties, nc) = decode_stroke_properties_at(bytes, c)?;
            c = nc;
            Some((
                Operation::InsertStroke {
                    id,
                    origin_left: ol,
                    origin_right: or_,
                    data,
                    properties,
                },
                c,
            ))
        }
        0x02 => {
            let (id, nc) = decode_op_id_at(bytes, c)?;
            c = nc;
            let (target, nc) = decode_op_id_at(bytes, c)?;
            c = nc;
            Some((Operation::DeleteStroke { id, target }, c))
        }
        0x03 => {
            let (id, nc) = decode_op_id_at(bytes, c)?;
            c = nc;
            let (target, nc) = decode_op_id_at(bytes, c)?;
            c = nc;
            let (update, nc) = decode_property_update_at(bytes, c)?;
            c = nc;
            Some((Operation::UpdateProperty { id, target, update }, c))
        }
        0x04 => {
            let (id, nc) = decode_op_id_at(bytes, c)?;
            c = nc;
            let (key, nc) = decode_metadata_key_at(bytes, c)?;
            c = nc;
            if c >= bytes.len() {
                return None;
            }
            let has_value = bytes[c];
            c += 1;
            let value = if has_value != 0 {
                let (v, nc) = decode_metadata_value_at(bytes, c)?;
                c = nc;
                Some(v)
            } else {
                None
            };
            Some((Operation::UpdateMetadata { id, key, value }, c))
        }
        _ => None,
    }
}

// ─── Public API ───────────────────────────────────────────────────────────────

/// Encode a batch of operations into a binary update blob.
pub fn encode_update(ops: &[Operation]) -> Vec<u8> {
    let mut out = Vec::new();
    encode_varint(ops.len() as u64, &mut out);
    for op in ops {
        encode_operation(op, &mut out);
    }
    out
}

/// Decode a binary update blob into a list of operations.
///
/// Returns `Err` if the bytes are truncated, malformed, or if any embedded
/// stroke exceeds [`MAX_POINTS_PER_STROKE`], or if the op count exceeds
/// [`MAX_STROKES`]. This prevents resource exhaustion from hostile payloads.
pub fn decode_update(bytes: &[u8]) -> VectisResult<Vec<Operation>> {
    let (count, mut cursor) = decode_varint(bytes)
        .ok_or_else(|| VectisError::DecodingError("empty or truncated update".into()))?;
    if count as usize > MAX_STROKES {
        return Err(VectisError::LimitExceeded {
            what: "ops_per_update",
            limit: MAX_STROKES,
            actual: count as usize,
        });
    }
    let mut ops = Vec::with_capacity(count as usize);
    for i in 0..count {
        match decode_operation_at(bytes, cursor) {
            Some((op, next)) => {
                cursor = next;
                ops.push(op);
            }
            None => {
                return Err(VectisError::DecodingError(format!(
                    "malformed operation {} of {} at byte {}",
                    i + 1,
                    count,
                    cursor
                )))
            }
        }
    }
    Ok(ops)
}

/// Encode a VectorClock as a state vector for delta sync requests.
pub fn encode_state_vector(vc: &VectorClock) -> Vec<u8> {
    let mut out = Vec::new();
    encode_varint(vc.clocks.len() as u64, &mut out);
    for (&actor, &ts) in &vc.clocks {
        encode_varint(actor.0, &mut out);
        encode_varint(ts, &mut out);
    }
    out
}

/// Decode a state vector (VectorClock) from bytes.
///
/// Silently truncates if the actor count exceeds [`MAX_ACTORS`] to prevent
/// unbounded BTreeMap growth from spoofed actor IDs.
pub fn decode_vector_clock(bytes: &[u8]) -> VectorClock {
    let mut vc = VectorClock::new();
    let Some((count, mut cursor)) = decode_varint(bytes) else {
        return vc;
    };
    let safe_count = (count as usize).min(MAX_ACTORS);
    for _ in 0..safe_count {
        let Some((actor, n1)) = decode_varint(&bytes[cursor..]) else {
            break;
        };
        cursor += n1;
        let Some((ts, n2)) = decode_varint(&bytes[cursor..]) else {
            break;
        };
        cursor += n2;
        vc.advance(ActorId(actor), ts);
    }
    vc
}

/// Encode an OpId as a fixed 16-byte little-endian representation.
pub fn encode_op_id(id: &OpId) -> Box<[u8]> {
    let mut out = [0u8; 16];
    out[0..8].copy_from_slice(&id.lamport.0.to_le_bytes());
    out[8..16].copy_from_slice(&id.actor.0.to_le_bytes());
    Box::new(out)
}

/// Decode an OpId from 16 bytes.
pub fn decode_op_id(bytes: &[u8]) -> OpId {
    if bytes.len() < 16 {
        return OpId::ZERO;
    }
    let lamport = u64::from_le_bytes(bytes[0..8].try_into().unwrap_or([0u8; 8]));
    let actor = u64::from_le_bytes(bytes[8..16].try_into().unwrap_or([0u8; 8]));
    OpId {
        lamport: LamportTs(lamport),
        actor: ActorId(actor),
    }
}

/// Encode a list of StrokeIds as a flat byte array (16 bytes each).
pub fn encode_stroke_ids(ids: &[StrokeId]) -> Vec<u8> {
    let mut out = Vec::with_capacity(ids.len() * 16);
    for id in ids {
        out.extend_from_slice(&encode_op_id(id));
    }
    out
}

/// Encode a full document snapshot.
/// Format:
///   [version: u8 = SNAPSHOT_VERSION]
///   varint(actor_id)
///   varint(lamport)
///   state_vector
///   varint(op_count)
///   [operations...]
///
/// The snapshot encodes all visible strokes as InsertStroke operations
/// plus tombstone metadata for GC state reconstruction.
pub fn encode_snapshot(doc: &crate::document::Document) -> Vec<u8> {
    let mut out = Vec::new();

    // Version byte — allows detecting format changes on decode
    out.push(SNAPSHOT_VERSION);

    // Actor + clock
    encode_varint(doc.local_actor.0, &mut out);
    encode_varint(doc.clock.0, &mut out);

    // State vector
    let sv = encode_state_vector(&doc.version);
    encode_varint(sv.len() as u64, &mut out);
    out.extend_from_slice(&sv);

    // Collect operations representing the current state:
    // 1. InsertStroke for each item (visible or tombstone — needed for index reconstruction)
    // 2. DeleteStroke for tombstones
    let mut ops: Vec<Operation> = Vec::new();

    for item in doc.stroke_order.items.iter() {
        if let Some((data, props)) = doc.stroke_store.strokes.get(&item.content) {
            ops.push(Operation::InsertStroke {
                id: item.id,
                origin_left: item.origin_left,
                origin_right: item.origin_right,
                data: data.clone(),
                properties: props.clone(),
            });
        }
        if let crate::rga::ItemState::Tombstone { deleted_at } = item.state {
            ops.push(Operation::DeleteStroke {
                id: deleted_at,
                target: item.id,
            });
        }
    }

    // Metadata operations
    for (key, value_opt) in doc.metadata.iter() {
        if let Some(value) = value_opt {
            ops.push(Operation::UpdateMetadata {
                id: OpId::ZERO,
                key: key.clone(),
                value: Some(value.clone()),
            });
        }
    }

    let encoded_ops = encode_update(&ops);
    out.extend_from_slice(&encoded_ops);

    out
}

/// Decode a snapshot into a new Document.
pub fn decode_snapshot(bytes: &[u8], actor: ActorId) -> VectisResult<crate::document::Document> {
    if bytes.is_empty() {
        return Err(VectisError::DecodingError("empty snapshot".into()));
    }

    let mut cursor = 0;

    // Version byte check
    let got_version = bytes[cursor];
    cursor += 1;
    if got_version != SNAPSHOT_VERSION {
        return Err(VectisError::SnapshotVersionMismatch {
            expected: SNAPSHOT_VERSION,
            got: got_version,
        });
    }

    // Stored actor + clock (we use the provided actor ID, not the stored one)
    let (_, n) = decode_varint(&bytes[cursor..])
        .ok_or_else(|| VectisError::DecodingError("failed to read actor".into()))?;
    cursor += n;

    let (lamport, n) = decode_varint(&bytes[cursor..])
        .ok_or_else(|| VectisError::DecodingError("failed to read lamport".into()))?;
    cursor += n;

    // State vector (length-prefixed)
    let (sv_len, n) = decode_varint(&bytes[cursor..])
        .ok_or_else(|| VectisError::DecodingError("failed to read sv_len".into()))?;
    cursor += n;
    let sv_end = cursor + sv_len as usize;
    if sv_end > bytes.len() {
        return Err(VectisError::DecodingError("state vector truncated".into()));
    }
    let version = decode_vector_clock(&bytes[cursor..sv_end]);
    cursor = sv_end;

    let mut doc = crate::document::Document::new(actor);
    doc.clock = LamportTs(lamport);
    doc.version = version;

    // Operations
    let ops =
        decode_update(&bytes[cursor..]).map_err(|e| VectisError::DecodingError(e.to_string()))?;
    for op in ops {
        doc.apply_remote(op);
    }

    Ok(doc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;
    use crate::stroke::{StrokeData, StrokePoint, StrokeProperties, ToolKind};
    use crate::types::ActorId;

    #[test]
    fn varint_roundtrip() {
        for &v in &[0u64, 1, 127, 128, 300, 16383, 16384, u64::MAX / 2] {
            let mut buf = Vec::new();
            encode_varint(v, &mut buf);
            let (decoded, _) = decode_varint(&buf).unwrap();
            assert_eq!(decoded, v, "varint roundtrip failed for {}", v);
        }
    }

    #[test]
    fn op_id_roundtrip() {
        let id = OpId {
            lamport: LamportTs(12345),
            actor: ActorId(67890),
        };
        let encoded = encode_op_id(&id);
        let decoded = decode_op_id(&encoded);
        assert_eq!(id, decoded);
    }

    #[test]
    fn encode_decode_update_roundtrip() {
        let mut doc = Document::new(ActorId(1));
        let pts: Box<[StrokePoint]> = vec![
            StrokePoint::new(1.0, 2.0, 0.8),
            StrokePoint::new(3.0, 4.0, 0.9),
        ]
        .into();
        let data = StrokeData::new(pts, ToolKind::Pen);
        let props = StrokeProperties::new(0xFF0000FF, 3.0, 1.0, OpId::ZERO);
        let id = doc.insert_stroke(data, props);

        let ops = std::mem::take(&mut doc.pending_ops);
        let encoded = encode_update(&ops);
        let decoded = decode_update(&encoded).unwrap();

        assert_eq!(decoded.len(), 1);
        if let Operation::InsertStroke {
            id: did,
            data: ddata,
            ..
        } = &decoded[0]
        {
            assert_eq!(*did, id);
            assert_eq!(ddata.points.len(), 2);
            assert!((ddata.points[0].x - 1.0).abs() < 1e-6);
        } else {
            panic!("wrong operation type");
        }
    }

    #[test]
    fn vector_clock_roundtrip() {
        let mut vc = VectorClock::new();
        vc.advance(ActorId(1), 100);
        vc.advance(ActorId(2), 200);
        let encoded = encode_state_vector(&vc);
        let decoded = decode_vector_clock(&encoded);
        assert_eq!(decoded.get(ActorId(1)), 100);
        assert_eq!(decoded.get(ActorId(2)), 200);
    }

    #[test]
    fn snapshot_roundtrip() {
        let mut doc = Document::new(ActorId(1));
        let pts: Box<[StrokePoint]> = vec![StrokePoint::basic(5.0, 5.0)].into();
        let data = StrokeData::new(pts, ToolKind::Pen);
        let props = StrokeProperties::new(0x00FF00FF, 2.0, 0.8, OpId::ZERO);
        let id = doc.insert_stroke(data, props);

        let snapshot = encode_snapshot(&doc);
        let doc2 = decode_snapshot(&snapshot, ActorId(99)).unwrap();

        let visible = doc2.visible_stroke_ids();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0], id);
    }
}
