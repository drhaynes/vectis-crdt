//! Ephemeral awareness state — cursor positions and actor presence.
//!
//! Awareness is NOT part of the CRDT. It is not persisted, does not
//! generate operations, and is not replayed on reconnect. It exists
//! purely for real-time UX: showing where other users' cursors are.
//!
//! ## Protocol
//!
//! Each actor broadcasts `CursorState` periodically (e.g. every 50ms).
//! The server relays it to all other connected peers.
//! States are evicted after a configurable TTL.
//!
//! ## Wire format
//!
//! Fixed 28-byte struct (no varints needed — low priority, low frequency):
//!   [actor: u64 LE][x: f32 LE][y: f32 LE][ts_ms: u64 LE][color: u32 LE]
//!   = 8 + 4 + 4 + 8 + 4 = 28 bytes

use crate::types::ActorId;
use std::collections::HashMap;

// ─── CursorState ─────────────────────────────────────────────────────────────

/// Ephemeral cursor / pointer state for one actor.
#[derive(Debug, Clone)]
pub struct CursorState {
    pub actor: ActorId,
    /// Canvas x coordinate (in document space, not screen space).
    pub x: f32,
    /// Canvas y coordinate.
    pub y: f32,
    /// Unix timestamp in milliseconds when this state was captured.
    /// Used for LWW ordering and TTL-based eviction.
    pub updated_at_ms: u64,
    /// Display color hint for this actor's cursor (RGBA packed).
    /// The server may assign this based on ActorId to ensure uniqueness.
    pub color: u32,
}

impl CursorState {
    pub fn new(actor: ActorId, x: f32, y: f32, updated_at_ms: u64, color: u32) -> Self {
        Self {
            actor,
            x,
            y,
            updated_at_ms,
            color,
        }
    }
}

// ─── AwarenessStore ──────────────────────────────────────────────────────────

/// Holds the latest known cursor state for all connected peers.
/// LWW semantics: newer `updated_at_ms` always wins.
pub struct AwarenessStore {
    cursors: HashMap<ActorId, CursorState>,
    /// TTL in milliseconds. States older than this are evicted.
    pub ttl_ms: u64,
}

impl AwarenessStore {
    pub fn new() -> Self {
        Self {
            cursors: HashMap::new(),
            ttl_ms: 30_000,
        }
    }

    pub fn with_ttl(ttl_ms: u64) -> Self {
        Self {
            cursors: HashMap::new(),
            ttl_ms,
        }
    }

    /// Insert or update a cursor state. Only accepted if newer than current.
    /// Returns true if the state was updated.
    pub fn update(&mut self, state: CursorState) -> bool {
        match self.cursors.entry(state.actor) {
            std::collections::hash_map::Entry::Vacant(e) => {
                e.insert(state);
                true
            }
            std::collections::hash_map::Entry::Occupied(mut e) => {
                if state.updated_at_ms >= e.get().updated_at_ms {
                    *e.get_mut() = state;
                    true
                } else {
                    false
                }
            }
        }
    }

    /// Remove a specific actor (e.g. on disconnect).
    pub fn remove(&mut self, actor: ActorId) -> Option<CursorState> {
        self.cursors.remove(&actor)
    }

    pub fn get(&self, actor: ActorId) -> Option<&CursorState> {
        self.cursors.get(&actor)
    }

    /// Iterate over all known cursor states (including potentially stale ones).
    pub fn all(&self) -> impl Iterator<Item = &CursorState> {
        self.cursors.values()
    }

    /// Evict actors whose state is older than `ttl_ms`.
    /// `now_ms`: current Unix time in milliseconds.
    /// Returns the count of evicted actors.
    pub fn evict_stale(&mut self, now_ms: u64) -> usize {
        let ttl = self.ttl_ms;
        let before = self.cursors.len();
        self.cursors
            .retain(|_, state| now_ms.saturating_sub(state.updated_at_ms) < ttl);
        before - self.cursors.len()
    }

    /// Number of currently tracked actors.
    pub fn actor_count(&self) -> usize {
        self.cursors.len()
    }

    /// Returns all cursors as a flat byte buffer for bulk broadcast.
    /// Layout: N × 28 bytes.
    pub fn encode_all(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.cursors.len() * 28);
        for state in self.cursors.values() {
            out.extend_from_slice(&encode_cursor(state));
        }
        out
    }

    /// Bulk-apply a received cursor buffer (N × 28 bytes).
    /// Returns the number of states that were updated.
    pub fn apply_bulk(&mut self, data: &[u8]) -> usize {
        let mut updated = 0;
        for chunk in data.chunks_exact(28) {
            if let Some(state) = decode_cursor(chunk)
                && self.update(state) {
                    updated += 1;
                }
        }
        updated
    }
}

impl Default for AwarenessStore {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Wire encoding ────────────────────────────────────────────────────────────

/// Encode a cursor state to a fixed 28-byte array.
/// Layout: [actor u64 LE][x f32 LE][y f32 LE][ts_ms u64 LE][color u32 LE]
pub fn encode_cursor(state: &CursorState) -> [u8; 28] {
    let mut out = [0u8; 28];
    out[0..8].copy_from_slice(&state.actor.0.to_le_bytes());
    out[8..12].copy_from_slice(&state.x.to_le_bytes());
    out[12..16].copy_from_slice(&state.y.to_le_bytes());
    out[16..24].copy_from_slice(&state.updated_at_ms.to_le_bytes());
    out[24..28].copy_from_slice(&state.color.to_le_bytes());
    out
}

/// Decode a cursor state from exactly 28 bytes.
/// Returns None if the slice is too short or data is invalid.
pub fn decode_cursor(bytes: &[u8]) -> Option<CursorState> {
    if bytes.len() < 28 {
        return None;
    }
    let actor = u64::from_le_bytes(bytes[0..8].try_into().ok()?);
    let x = f32::from_le_bytes(bytes[8..12].try_into().ok()?);
    let y = f32::from_le_bytes(bytes[12..16].try_into().ok()?);
    let ts = u64::from_le_bytes(bytes[16..24].try_into().ok()?);
    let color = u32::from_le_bytes(bytes[24..28].try_into().ok()?);
    Some(CursorState {
        actor: ActorId(actor),
        x,
        y,
        updated_at_ms: ts,
        color,
    })
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_encode_decode_roundtrip() {
        let state = CursorState::new(ActorId(42), 100.5, 200.75, 1_700_000_000_000, 0xFF6600FF);
        let bytes = encode_cursor(&state);
        let decoded = decode_cursor(&bytes).unwrap();
        assert_eq!(decoded.actor.0, 42);
        assert!((decoded.x - 100.5).abs() < 1e-6);
        assert!((decoded.y - 200.75).abs() < 1e-6);
        assert_eq!(decoded.updated_at_ms, 1_700_000_000_000);
        assert_eq!(decoded.color, 0xFF6600FF);
    }

    #[test]
    fn awareness_lww_ordering() {
        let mut store = AwarenessStore::new();
        let actor = ActorId(1);
        store.update(CursorState::new(actor, 10.0, 10.0, 1000, 0));
        // Newer timestamp wins
        store.update(CursorState::new(actor, 20.0, 20.0, 2000, 0));
        // Older timestamp must be rejected
        store.update(CursorState::new(actor, 5.0, 5.0, 500, 0));
        let s = store.get(actor).unwrap();
        assert!((s.x - 20.0).abs() < 1e-6, "newer position must win");
    }

    #[test]
    fn awareness_evict_stale() {
        let mut store = AwarenessStore::with_ttl(1000);
        store.update(CursorState::new(ActorId(1), 0.0, 0.0, 0, 0));
        store.update(CursorState::new(ActorId(2), 0.0, 0.0, 500, 0));
        // now_ms=1100: actor 1 (ts=0) is 1100ms old → evict; actor 2 (ts=500) is 600ms → keep
        let evicted = store.evict_stale(1100);
        assert_eq!(evicted, 1);
        assert_eq!(store.actor_count(), 1);
        assert!(store.get(ActorId(1)).is_none());
        assert!(store.get(ActorId(2)).is_some());
    }

    #[test]
    fn awareness_bulk_encode_decode() {
        let mut store = AwarenessStore::new();
        store.update(CursorState::new(ActorId(1), 1.0, 1.0, 100, 0xFF0000FF));
        store.update(CursorState::new(ActorId(2), 2.0, 2.0, 200, 0x00FF00FF));
        let bulk = store.encode_all();
        assert_eq!(bulk.len(), 56); // 2 × 28 bytes

        let mut store2 = AwarenessStore::new();
        let updated = store2.apply_bulk(&bulk);
        assert_eq!(updated, 2);
        assert_eq!(store2.actor_count(), 2);
    }
}
