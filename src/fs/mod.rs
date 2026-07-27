//! L0 filesystem primitives that own a lifecycle.
//!
//! `staged` is the atomic-write staging handle (same-directory temp file, fsync, rename);
//! `lock` is the root heartbeat lock. Named `fs::staged` rather than `atomic` because
//! `crate::atomic` read as `std::sync::atomic`, which several of these modules also import.

pub mod lock;
pub mod staged;
