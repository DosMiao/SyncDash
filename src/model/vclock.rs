//! Vector clocks (P2-1). Semantics map 1:1 onto syncthing `lib/protocol/vector.go`, rewritten in Rust.
//!
//! Why it is needed: the archive (last-sync snapshot) can only answer "did this side change relative to
//! last time", which degrades past two nodes —— A→B syncs, then B→C, then C edits it back, and nobody can
//! say who based their edit on whom. A vector clock encodes causality directly into every file's version:
//!   - `Greater` / `Lesser`: one side is a descendant of the other → one-way propagation, not a conflict
//!   - `Concurrent`: neither is an ancestor of the other → **a real conflict**
//!
//! The counter takes `max(old+1, unix_now)` (syncthing `updateWithNow`): with the timestamp as a lower bound,
//! a node's counter never rewinds to an already-used value even if its store is overwritten by an old backup.
//!
//! **Current status**: the mathematical core and node identity are complete and tested; `compare` is
//! enabled when `archive_format = "index"` (see config.rs). The default still runs the archive snapshot
//! path —— true N-way requires the vector to be maintained exactly after every apply, which is v1.0 convergence work.
//!
//! **Stale-doc note (added during translation)**: the activation claim above no longer matches the repo.
//! The `archive_format` key exists nowhere in the codebase (config.rs has no such setting), so `compare`
//! is never switched on that way. This module has zero callers — it is reachable only through
//! `pub mod vclock;` in lib.rs. Read the paragraph above as intent, not as current behavior.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Short node ID. Derived from the local node-id file: stable and decoupled from the hostname
/// (renaming the host must not turn every historical version into "someone else's write").
pub type ShortId = u64;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Counter {
    pub id: ShortId,
    pub value: u64,
}

/// Vector clock. counters is always sorted ascending by id —— comparison is a two-pointer linear scan, no map needed.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct Vector {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub counters: Vec<Counter>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ordering {
    Equal,
    Greater,
    Lesser,
    /// Concurrent, with the first divergence pointing "mine is greater" —— purely for stable sorting; semantically this is just concurrent
    ConcurrentGreater,
    /// Concurrent, with the first divergence pointing "mine is lesser"
    ConcurrentLesser,
}

impl Ordering {
    pub fn is_concurrent(self) -> bool {
        matches!(self, Ordering::ConcurrentGreater | Ordering::ConcurrentLesser)
    }
}

fn unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(1)
}

impl Vector {
    pub fn new() -> Vector {
        Vector::default()
    }

    pub fn is_empty(&self) -> bool {
        self.counters.is_empty()
    }

    pub fn counter(&self, id: ShortId) -> u64 {
        self.counters.iter().find(|c| c.id == id).map(|c| c.value).unwrap_or(0)
    }

    /// "I changed this file": advance my own counter by one.
    pub fn update(&mut self, id: ShortId) {
        self.update_with_now(id, unix_secs());
    }

    pub fn update_with_now(&mut self, id: ShortId, now: u64) {
        match self.counters.binary_search_by_key(&id, |c| c.id) {
            Ok(i) => {
                let c = &mut self.counters[i];
                c.value = (c.value + 1).max(now);
            }
            Err(i) => self.counters.insert(i, Counter { id, value: now.max(1) }),
        }
    }

    /// "I have taken in their version": per-id maximum.
    pub fn merge(&mut self, other: &Vector) {
        for c in &other.counters {
            match self.counters.binary_search_by_key(&c.id, |x| x.id) {
                Ok(i) => {
                    if c.value > self.counters[i].value {
                        self.counters[i].value = c.value;
                    }
                }
                Err(i) => self.counters.insert(i, *c),
            }
        }
    }

    pub fn merged(&self, other: &Vector) -> Vector {
        let mut v = self.clone();
        v.merge(other);
        v
    }

    /// This vector's relation to `other`.
    pub fn compare(&self, other: &Vector) -> Ordering {
        let (mut i, mut j) = (0usize, 0usize);
        let mut saw_greater = false;
        let mut saw_lesser = false;
        // Remember the first divergence direction so concurrency gets a stable sort representation (same handling as syncthing)
        let mut first_greater: Option<bool> = None;
        let mark = |greater: bool, sg: &mut bool, sl: &mut bool, first: &mut Option<bool>| {
            if greater {
                *sg = true;
            } else {
                *sl = true;
            }
            first.get_or_insert(greater);
        };

        while i < self.counters.len() || j < other.counters.len() {
            match (self.counters.get(i), other.counters.get(j)) {
                (Some(a), Some(b)) if a.id == b.id => {
                    if a.value > b.value {
                        mark(true, &mut saw_greater, &mut saw_lesser, &mut first_greater);
                    } else if a.value < b.value {
                        mark(false, &mut saw_greater, &mut saw_lesser, &mut first_greater);
                    }
                    i += 1;
                    j += 1;
                }
                // Only I have this id → I am greater on this axis
                (Some(a), Some(b)) if a.id < b.id => {
                    if a.value > 0 {
                        mark(true, &mut saw_greater, &mut saw_lesser, &mut first_greater);
                    }
                    i += 1;
                }
                // Only they have it → I am lesser on this axis
                (Some(_), Some(b)) => {
                    if b.value > 0 {
                        mark(false, &mut saw_greater, &mut saw_lesser, &mut first_greater);
                    }
                    j += 1;
                }
                (Some(a), None) => {
                    if a.value > 0 {
                        mark(true, &mut saw_greater, &mut saw_lesser, &mut first_greater);
                    }
                    i += 1;
                }
                (None, Some(b)) => {
                    if b.value > 0 {
                        mark(false, &mut saw_greater, &mut saw_lesser, &mut first_greater);
                    }
                    j += 1;
                }
                (None, None) => break,
            }
        }

        match (saw_greater, saw_lesser) {
            (false, false) => Ordering::Equal,
            (true, false) => Ordering::Greater,
            (false, true) => Ordering::Lesser,
            (true, true) => {
                if first_greater == Some(true) {
                    Ordering::ConcurrentGreater
                } else {
                    Ordering::ConcurrentLesser
                }
            }
        }
    }

    pub fn equal(&self, other: &Vector) -> bool {
        self.compare(other) == Ordering::Equal
    }
    pub fn greater_equal(&self, other: &Vector) -> bool {
        matches!(self.compare(other), Ordering::Greater | Ordering::Equal)
    }
    pub fn lesser_equal(&self, other: &Vector) -> bool {
        matches!(self.compare(other), Ordering::Lesser | Ordering::Equal)
    }
    pub fn concurrent(&self, other: &Vector) -> bool {
        self.compare(other).is_concurrent()
    }

    /// Human-readable: `a1b2c3d4:17,e5f6:3`
    pub fn to_compact(&self) -> String {
        self.counters
            .iter()
            .map(|c| format!("{:x}:{}", c.id, c.value))
            .collect::<Vec<_>>()
            .join(",")
    }
}

fn node_id_path() -> PathBuf {
    crate::foundation::dirs::jobs_dir()
        .parent()
        .map(|p| p.join("node-id"))
        .unwrap_or_else(|| PathBuf::from("syncdash-node-id"))
}

/// Stable local node ID. Generated and written to disk on the first call; reused ever after.
/// Seed material = hostname + first-call time + process id, taking the first 8 bytes of blake3.
pub fn local_node_id() -> ShortId {
    let p = node_id_path();
    if let Ok(text) = std::fs::read_to_string(&p) {
        if let Ok(v) = u64::from_str_radix(text.trim(), 16) {
            if v != 0 {
                return v;
            }
        }
    }
    let seed = format!("{}|{}|{}", crate::model::table::host_name(), crate::foundation::time::now_ms(), std::process::id());
    let h = blake3::hash(seed.as_bytes());
    let mut b = [0u8; 8];
    b.copy_from_slice(&h.as_bytes()[..8]);
    // 0 is the semantic value for "absent" — steer clear of it
    let id = u64::from_be_bytes(b).max(1);
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&p, format!("{id:x}\n"));
    id
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(pairs: &[(ShortId, u64)]) -> Vector {
        let mut c: Vec<Counter> = pairs.iter().map(|&(id, value)| Counter { id, value }).collect();
        c.sort_by_key(|x| x.id);
        Vector { counters: c }
    }

    #[test]
    fn empty_vectors_are_equal() {
        assert_eq!(Vector::new().compare(&Vector::new()), Ordering::Equal);
    }

    #[test]
    fn update_makes_it_greater() {
        let base = Vector::new();
        let mut a = base.clone();
        a.update_with_now(7, 100);
        assert_eq!(a.compare(&base), Ordering::Greater);
        assert_eq!(base.compare(&a), Ordering::Lesser);
    }

    #[test]
    fn update_is_monotonic_even_with_a_stale_clock() {
        let mut a = v(&[(1, 500)]);
        a.update_with_now(1, 10); // clock went backwards
        assert_eq!(a.counter(1), 501, "counter must never go backwards");
        let mut b = v(&[(1, 5)]);
        b.update_with_now(1, 900); // clock jumped forward
        assert_eq!(b.counter(1), 900, "wall clock is a lower bound");
    }

    #[test]
    fn independent_updates_are_concurrent() {
        let base = v(&[(1, 10)]);
        let mut a = base.clone();
        let mut b = base.clone();
        a.update_with_now(1, 11);
        b.update_with_now(2, 11);
        assert!(a.concurrent(&b), "different devices editing from a common base => conflict");
        assert!(b.concurrent(&a));
    }

    #[test]
    fn merge_resolves_concurrency() {
        let mut a = v(&[(1, 5)]);
        let b = v(&[(2, 7)]);
        assert!(a.concurrent(&b));
        a.merge(&b);
        assert_eq!(a.compare(&b), Ordering::Greater, "after merging, a dominates b");
        assert_eq!(a.counter(1), 5);
        assert_eq!(a.counter(2), 7);
    }

    #[test]
    fn descendant_is_not_a_conflict() {
        // A edits → hands off to B → B edits again: a linear history; no step should ever be a conflict
        let mut a = Vector::new();
        a.update_with_now(1, 100);
        let mut b = a.clone();
        b.update_with_now(2, 101);
        assert_eq!(b.compare(&a), Ordering::Greater);
        assert!(!b.concurrent(&a), "a linear history must never be reported as a conflict");
    }

    #[test]
    fn missing_counter_with_zero_value_does_not_diverge() {
        let a = v(&[(1, 3), (2, 0)]);
        let b = v(&[(1, 3)]);
        assert_eq!(a.compare(&b), Ordering::Equal, "a zero counter is the same as absent");
    }

    #[test]
    fn concurrent_variant_is_stable_for_sorting() {
        let a = v(&[(1, 9), (2, 1)]);
        let b = v(&[(1, 1), (2, 9)]);
        // The first divergence, at id=1, is "mine is greater" → ConcurrentGreater; the reverse must be the other variant
        assert_eq!(a.compare(&b), Ordering::ConcurrentGreater);
        assert_eq!(b.compare(&a), Ordering::ConcurrentLesser);
    }

    #[test]
    fn ordering_is_antisymmetric_and_reflexive() {
        // A small exhaustive sweep in place of a property test: every vector is Equal to itself,
        // and a>b implies b<a, a||b implies b||a
        let space: Vec<Vector> = (0..3)
            .flat_map(|x| (0..3).map(move |y| v(&[(1, x), (2, y)])))
            .collect();
        for a in &space {
            assert_eq!(a.compare(a), Ordering::Equal, "reflexive");
            for b in &space {
                let ab = a.compare(b);
                let ba = b.compare(a);
                match ab {
                    Ordering::Equal => assert_eq!(ba, Ordering::Equal),
                    Ordering::Greater => assert_eq!(ba, Ordering::Lesser),
                    Ordering::Lesser => assert_eq!(ba, Ordering::Greater),
                    Ordering::ConcurrentGreater => assert_eq!(ba, Ordering::ConcurrentLesser),
                    Ordering::ConcurrentLesser => assert_eq!(ba, Ordering::ConcurrentGreater),
                }
            }
        }
    }

    #[test]
    fn merge_is_a_least_upper_bound() {
        let space: Vec<Vector> = (0..3)
            .flat_map(|x| (0..3).map(move |y| v(&[(1, x), (2, y)])))
            .collect();
        for a in &space {
            for b in &space {
                let m = a.merged(b);
                assert!(m.greater_equal(a), "merge must dominate a");
                assert!(m.greater_equal(b), "merge must dominate b");
            }
        }
    }

    #[test]
    fn compact_string_is_readable() {
        assert_eq!(v(&[(0xab, 3), (0xcd, 1)]).to_compact(), "ab:3,cd:1");
    }
}
