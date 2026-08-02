//! L2 the three-stage main line: scan -> compare -> apply.
//!
//! Each stage's input and output is a human-readable JSONL table, which is what lets an
//! ssh pipe, an archive and an audit all use the same format. `filter` decides what takes
//! part at all; `guard` is the set of gates that must pass before anything is written.

pub mod apply;
pub mod compare;
pub mod filter;
pub mod guard;
pub(crate) mod name_safety;
pub mod scan;
