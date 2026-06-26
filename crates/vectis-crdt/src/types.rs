use std::cmp::Ordering;
use std::collections::BTreeMap;

/// Unique actor/client identifier.
/// u64 instead of UUID for wire compactness.
/// Assigned by the server on first connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ActorId(pub u64);

/// Lamport logical timestamp. Monotonically increasing per actor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LamportTs(pub u64);

impl LamportTs {
    #[inline]
    pub fn tick(&mut self) -> Self {
        self.0 += 1;
        *self
    }

    #[inline]
    pub fn merge(&mut self, other: LamportTs) {
        if other.0 > self.0 {
            self.0 = other.0;
        }
    }
}

/// Globally unique operation identifier.
/// Also serves as the "ID" of each item in the CRDT.
/// Total order: (LamportTs DESC, ActorId DESC) — higher Lamport wins;
/// on tie, higher ActorId wins (deterministic).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct OpId {
    pub lamport: LamportTs,
    pub actor: ActorId,
}

impl OpId {
    pub const ZERO: OpId = OpId {
        lamport: LamportTs(0),
        actor: ActorId(0),
    };

    #[inline]
    pub fn is_zero(&self) -> bool {
        self.lamport.0 == 0 && self.actor.0 == 0
    }
}

/// Total deterministic order: higher Lamport wins; tie-break by ActorId.
impl Ord for OpId {
    fn cmp(&self, other: &Self) -> Ordering {
        self.lamport
            .0
            .cmp(&other.lamport.0)
            .then_with(|| self.actor.0.cmp(&other.actor.0))
    }
}

impl PartialOrd for OpId {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Vector Clock — tracks the last operation seen from each actor.
/// Used for: causal consistency, delta sync, GC.
#[derive(Debug, Clone, Default)]
pub struct VectorClock {
    /// actor -> max lamport timestamp seen from that actor.
    /// `pub(crate)` — iterate via methods; direct access breaks encapsulation.
    pub(crate) clocks: BTreeMap<ActorId, u64>,
}

impl VectorClock {
    pub fn new() -> Self {
        Self::default()
    }

    #[inline]
    pub fn get(&self, actor: ActorId) -> u64 {
        self.clocks.get(&actor).copied().unwrap_or(0)
    }

    pub fn advance(&mut self, actor: ActorId, ts: u64) {
        let entry = self.clocks.entry(actor).or_insert(0);
        if ts > *entry {
            *entry = ts;
        }
    }

    /// Returns true if self has seen everything other has seen.
    pub fn dominates(&self, other: &VectorClock) -> bool {
        other
            .clocks
            .iter()
            .all(|(actor, &ts)| self.get(*actor) >= ts)
    }

    /// Point-wise merge: take max of each component.
    pub fn merge(&mut self, other: &VectorClock) {
        for (&actor, &ts) in &other.clocks {
            self.advance(actor, ts);
        }
    }

    /// Iterate over `(actor, max_lamport_seen)` entries in actor order.
    pub fn iter(&self) -> impl Iterator<Item = (ActorId, u64)> + '_ {
        self.clocks.iter().map(|(&actor, &ts)| (actor, ts))
    }

    /// Compute operations that `other` has NOT seen yet.
    /// Returns ranges `(actor, from_inclusive, to_inclusive)`.
    pub fn diff(&self, other: &VectorClock) -> Vec<(ActorId, u64, u64)> {
        let mut diffs = Vec::new();
        for (&actor, &my_ts) in &self.clocks {
            let their_ts = other.get(actor);
            if my_ts > their_ts {
                diffs.push((actor, their_ts + 1, my_ts));
            }
        }
        diffs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lamport_tick() {
        let mut ts = LamportTs(0);
        let t1 = ts.tick();
        assert_eq!(t1.0, 1);
        let t2 = ts.tick();
        assert_eq!(t2.0, 2);
    }

    #[test]
    fn opid_ordering() {
        let a = OpId {
            lamport: LamportTs(5),
            actor: ActorId(1),
        };
        let b = OpId {
            lamport: LamportTs(5),
            actor: ActorId(2),
        };
        let c = OpId {
            lamport: LamportTs(6),
            actor: ActorId(1),
        };
        assert!(b > a); // same lamport, higher actor wins
        assert!(c > b); // higher lamport wins
    }

    #[test]
    fn vector_clock_dominates() {
        let mut vc1 = VectorClock::new();
        vc1.advance(ActorId(1), 5);
        vc1.advance(ActorId(2), 3);

        let mut vc2 = VectorClock::new();
        vc2.advance(ActorId(1), 4);
        vc2.advance(ActorId(2), 3);

        assert!(vc1.dominates(&vc2));
        assert!(!vc2.dominates(&vc1));
    }
}
