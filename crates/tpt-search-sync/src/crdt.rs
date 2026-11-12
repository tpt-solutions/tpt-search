//! Conflict-free replicated data types. Each one merges any two replicas' states into
//! the same result regardless of the order the states arrive in, so replicas converge
//! without coordination.
//!
//! * [`GCounter`] and [`PnCounter`]: counters that only grow, or grow and shrink.
//! * [`OrSet`]: a set where an add wins over a concurrent remove.
//! * [`LwwDocument`]: a document whose fields are last-writer-wins registers, with
//!   tombstones for deletes.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use tpt_search_core::FieldValue;

/// Identifies a replica (an edge device, or the cloud).
pub type ReplicaId = u64;

/// A state that can absorb another replica's state.
pub trait CrdtDocument {
    /// Merges `other` into `self`. Must be commutative, associative, and idempotent.
    fn merge(&mut self, other: &Self);
}

/// A counter that only increases. Each replica owns one slot.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GCounter {
    slots: BTreeMap<ReplicaId, u64>,
}

impl GCounter {
    /// Adds `n` on behalf of `replica`.
    pub fn increment(&mut self, replica: ReplicaId, n: u64) {
        *self.slots.entry(replica).or_default() += n;
    }

    /// The total across replicas.
    pub fn value(&self) -> u64 {
        self.slots.values().sum()
    }
}

impl CrdtDocument for GCounter {
    fn merge(&mut self, other: &Self) {
        for (&replica, &count) in &other.slots {
            let slot = self.slots.entry(replica).or_default();
            *slot = (*slot).max(count);
        }
    }
}

/// A counter that can go up and down: a pair of grow-only counters.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PnCounter {
    positive: GCounter,
    negative: GCounter,
}

impl PnCounter {
    /// Adds `n` (which may be negative) on behalf of `replica`.
    pub fn add(&mut self, replica: ReplicaId, n: i64) {
        if n >= 0 {
            self.positive.increment(replica, n as u64);
        } else {
            self.negative.increment(replica, n.unsigned_abs());
        }
    }

    /// The net value.
    pub fn value(&self) -> i64 {
        self.positive.value() as i64 - self.negative.value() as i64
    }
}

impl CrdtDocument for PnCounter {
    fn merge(&mut self, other: &Self) {
        self.positive.merge(&other.positive);
        self.negative.merge(&other.negative);
    }
}

/// A unique tag for one add. Replica and per-replica counter make it globally unique.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Tag {
    /// Replica that performed the add.
    pub replica: ReplicaId,
    /// That replica's add counter.
    pub counter: u64,
}

/// Observed-remove set. A remove deletes only the adds it has seen, so an add made
/// concurrently with a remove survives.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrSet<T: Ord> {
    adds: BTreeMap<T, BTreeSet<Tag>>,
    removed: BTreeSet<Tag>,
    next: BTreeMap<ReplicaId, u64>,
}

impl<T: Ord + Clone> OrSet<T> {
    /// Adds `value` on behalf of `replica`.
    pub fn add(&mut self, replica: ReplicaId, value: T) {
        let counter = self.next.entry(replica).or_default();
        *counter += 1;
        let tag = Tag {
            replica,
            counter: *counter,
        };
        self.adds.entry(value).or_default().insert(tag);
    }

    /// Removes the adds of `value` observed so far.
    pub fn remove(&mut self, value: &T) {
        if let Some(tags) = self.adds.remove(value) {
            self.removed.extend(tags);
        }
    }

    /// Whether `value` is in the set: it has an add that no remove has covered.
    pub fn contains(&self, value: &T) -> bool {
        self.adds
            .get(value)
            .is_some_and(|tags| tags.iter().any(|t| !self.removed.contains(t)))
    }

    /// The members, in order.
    pub fn members(&self) -> Vec<T> {
        self.adds
            .iter()
            .filter(|(_, tags)| tags.iter().any(|t| !self.removed.contains(t)))
            .map(|(v, _)| v.clone())
            .collect()
    }
}

impl<T: Ord + Clone> CrdtDocument for OrSet<T> {
    fn merge(&mut self, other: &Self) {
        for (value, tags) in &other.adds {
            self.adds
                .entry(value.clone())
                .or_default()
                .extend(tags.iter().copied());
        }
        self.removed.extend(other.removed.iter().copied());
        for (&replica, &counter) in &other.next {
            let slot = self.next.entry(replica).or_default();
            *slot = (*slot).max(counter);
        }
        // Drop tags that are now removed, and values left with no tags.
        let removed = &self.removed;
        for tags in self.adds.values_mut() {
            tags.retain(|t| !removed.contains(t));
        }
        self.adds.retain(|_, tags| !tags.is_empty());
    }
}

/// A logical timestamp: a Lamport clock value, with the replica as the tie-breaker.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
pub struct Stamp {
    /// Lamport clock value.
    pub clock: u64,
    /// Replica that made the write.
    pub replica: ReplicaId,
}

/// One field of a [`LwwDocument`]: its value and the stamp of the write that set it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LwwField {
    /// The value, or `None` if the field was cleared.
    pub value: Option<FieldValue>,
    /// When it was written.
    pub stamp: Stamp,
}

/// A document whose fields are last-writer-wins registers.
///
/// Writes to different fields do not conflict and both survive. Concurrent writes to the
/// same field resolve to the write with the larger [`Stamp`]. A delete is stamped too,
/// so a later write brings the document back and an earlier one does not.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LwwDocument {
    /// Fields by name.
    pub fields: BTreeMap<String, LwwField>,
    /// Stamp of the newest delete, if the document has ever been deleted.
    pub deleted: Option<Stamp>,
}

impl LwwDocument {
    /// Sets (or clears, with `None`) a field if `stamp` is newer than what is stored.
    /// Returns whether the write took effect.
    pub fn set_field(&mut self, name: &str, value: Option<FieldValue>, stamp: Stamp) -> bool {
        match self.fields.get(name) {
            Some(existing) if existing.stamp >= stamp => false,
            _ => {
                self.fields
                    .insert(name.to_owned(), LwwField { value, stamp });
                true
            }
        }
    }

    /// Records a delete if `stamp` is newer than the last delete.
    pub fn delete(&mut self, stamp: Stamp) -> bool {
        match self.deleted {
            Some(d) if d >= stamp => false,
            _ => {
                self.deleted = Some(stamp);
                true
            }
        }
    }

    /// Whether the document exists: some field was written after the last delete.
    pub fn is_live(&self) -> bool {
        match self.deleted {
            None => true,
            Some(d) => self
                .fields
                .values()
                .any(|f| f.value.is_some() && f.stamp > d),
        }
    }

    /// Current value of a field, if the document is live and the field is set.
    pub fn get(&self, name: &str) -> Option<&FieldValue> {
        if !self.is_live() {
            return None;
        }
        self.fields.get(name).and_then(|f| f.value.as_ref())
    }
}

impl CrdtDocument for LwwDocument {
    fn merge(&mut self, other: &Self) {
        for (name, field) in &other.fields {
            self.set_field(name, field.value.clone(), field.stamp);
        }
        if let Some(stamp) = other.deleted {
            self.delete(stamp);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gcounter_merge_is_max_per_replica() {
        let mut a = GCounter::default();
        a.increment(1, 3);
        let mut b = GCounter::default();
        b.increment(1, 5);
        b.increment(2, 2);
        a.merge(&b);
        assert_eq!(a.value(), 7);
        let before = a.clone();
        a.merge(&b);
        assert_eq!(a, before, "merge is idempotent");
    }

    #[test]
    fn pncounter_goes_up_and_down_and_converges() {
        let mut a = PnCounter::default();
        let mut b = PnCounter::default();
        a.add(1, 10);
        b.add(2, -4);
        a.merge(&b);
        b.merge(&a);
        assert_eq!(a.value(), 6);
        assert_eq!(a, b);
    }

    #[test]
    fn orset_add_wins_over_concurrent_remove() {
        let mut a: OrSet<&str> = OrSet::default();
        a.add(1, "x");
        let mut b = a.clone();
        // Replica b removes "x" while replica a adds it again concurrently.
        b.remove(&"x");
        a.add(1, "x");
        a.merge(&b);
        b.merge(&a);
        assert!(a.contains(&"x"));
        assert!(b.contains(&"x"));
        assert_eq!(a.members(), b.members());
    }

    #[test]
    fn orset_remove_applies_to_observed_adds() {
        let mut a: OrSet<&str> = OrSet::default();
        a.add(1, "x");
        let mut b = a.clone();
        b.remove(&"x");
        a.merge(&b);
        assert!(!a.contains(&"x"));
    }

    #[test]
    fn lww_field_keeps_the_newest_stamp() {
        let mut d = LwwDocument::default();
        let older = Stamp {
            clock: 1,
            replica: 9,
        };
        let newer = Stamp {
            clock: 2,
            replica: 1,
        };
        assert!(d.set_field("title", Some(FieldValue::Text("new".into())), newer));
        assert!(!d.set_field("title", Some(FieldValue::Text("old".into())), older));
        assert_eq!(d.get("title"), Some(&FieldValue::Text("new".into())));
    }

    #[test]
    fn lww_ties_break_by_replica() {
        let mut a = LwwDocument::default();
        let mut b = LwwDocument::default();
        a.set_field(
            "t",
            Some("from-1".into()),
            Stamp {
                clock: 5,
                replica: 1,
            },
        );
        b.set_field(
            "t",
            Some("from-2".into()),
            Stamp {
                clock: 5,
                replica: 2,
            },
        );
        a.merge(&b);
        b.merge(&a);
        assert_eq!(a, b);
        assert_eq!(a.get("t"), Some(&FieldValue::Text("from-2".into())));
    }

    #[test]
    fn delete_hides_older_writes_and_a_later_write_revives() {
        let mut d = LwwDocument::default();
        d.set_field(
            "t",
            Some("v1".into()),
            Stamp {
                clock: 1,
                replica: 1,
            },
        );
        d.delete(Stamp {
            clock: 2,
            replica: 1,
        });
        assert!(!d.is_live());
        assert_eq!(d.get("t"), None);
        d.set_field(
            "t",
            Some("v2".into()),
            Stamp {
                clock: 3,
                replica: 1,
            },
        );
        assert!(d.is_live());
        assert_eq!(d.get("t"), Some(&FieldValue::Text("v2".into())));
    }
}
