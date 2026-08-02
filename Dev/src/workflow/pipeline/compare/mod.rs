//! compare: two snapshots in, a plan out — the middle of the three stages.
//!
//! Modes: `mirror` makes target match source; `sync` merges both ways using the archive as the
//! record of what was last agreed; `enrich` only ever adds. The decision for one path is made
//! once, from the evidence both snapshots carry, and every op records the reason it exists.
//!
//! Around the decision itself:
//! - `matching` — equality keys, move pairing, and recorded naming semantics
//! - `planning` — mode decisions plus the ordered safety-policy passes
//! - `evidence` — the read-only layer the UI reads; `compare` itself is unaffected by it

mod conflicts;
mod entries;
pub mod evidence;
mod matching;
mod planning;
mod policy;

pub use conflicts::{conflict_name, is_conflict_copy};
pub use planning::compare;
pub use policy::{CompareOptions, ConflictPolicy};

#[cfg(test)]
mod tests;
