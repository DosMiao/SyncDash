//! Shared synchronization core for the CLI and Tauri desktop shell.
//!
//! # Layering
//!
//! ```text
//! L4  shells         src/main.rs (CLI) · src-tauri (desktop)
//! L3  orchestration  run · job · boot (process startup)
//! L2  domain         pipeline (scan/compare/apply/filter/guard) · transfer (peer/pack)
//! L1  services       obs (progress/logging/runlog) · store (settings/trash/version)
//! L0  foundation     foundation · model (plan/event/table/chunk) · fs (staged/lock)
//! ```
//!
//! Dependencies point downward. Exported logging macros are the accepted upward edge from
//! filesystem code into observability. `model` owns persisted vocabulary, not engines.
//! Single-file domains stay flat, `mod.rs` files carry behavior, and callers use full module paths
//! instead of re-export hubs.
//!
//! # User files are never memory-mapped
//!
//! User-file hashing uses explicit reads. A mapped page on a detached or truncated volume can raise
//! SIGBUS instead of an `io::Error`, killing Apply before lease cleanup. Keeping blake3's mmap
//! features disabled makes that unsafe path a compile error.
//!
//! A `peer://` root executes on the far-side SyncDash. Protocol roots such as `sftp://` are
//! scanned and written by this process through `Vfs`.

pub mod foundation;

pub mod fs;
pub mod model;

pub mod obs;
pub mod store;

pub mod pipeline;
pub mod transfer;

pub mod boot;
pub mod job;
pub mod run;
