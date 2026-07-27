//! L0 filesystem primitives that own a lifecycle.
//!
//! `staged` is the atomic-write staging handle (same-directory temp file, fsync, rename);
//! `lock` is the root heartbeat lock. Named `fs::staged` rather than `atomic` because
//! `crate::atomic` read as `std::sync::atomic`, which several of these modules also import.
//! `vfs` is the virtual filesystem a sync root lives on — local disk today, SMB/SFTP/FTP
//! backends behind the same trait; its write side wraps `staged` rather than reimplementing it.

pub mod lock;
pub mod staged;
pub mod vfs;
