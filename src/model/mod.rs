//! L0 data vocabulary: the types every other layer speaks in.
//!
//! Definitions only — no I/O, no policy. `table` is the on-disk snapshot format,
//! `chunk` the FastCDC primitives, `vclock` the (not yet wired) causal clock.

pub mod chunk;
pub mod event;
pub mod plan;
pub mod table;
pub mod vclock;
