//! L0 data vocabulary: the types every other layer speaks in.
//!
//! Vocabulary and the codecs that read and write it — `table` owns the on-disk snapshot format
//! including its JSONL reader and writer, `chunk` the FastCDC primitives. What does not belong
//! here is engine policy: a type may say how it is spelled on disk, never when to act on it.

pub mod chunk;
pub mod event;
pub mod plan;
pub mod table;
