//! Shared synchronization core for the CLI and Tauri desktop shell.
//!
//! # Layering
//!
//! ```text
//! L4  shells         shell/cli + main.rs (CLI) · Dev/src-tauri (desktop)
//! L3  orchestration  run (including durable run history) · job · boot (process startup)
//! L2  domain         pipeline (scan/compare/apply/filter/guard) · transfer (peer/pack)
//! L1  services       obs (progress/logging) · store (settings/trash/version)
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

#[path = "base/foundation/mod.rs"]
pub mod foundation;

#[path = "base/fs/mod.rs"]
pub mod fs;
#[path = "base/model/mod.rs"]
pub mod model;

#[path = "services/obs/mod.rs"]
pub mod obs;
#[path = "services/store/mod.rs"]
pub mod store;

#[path = "workflow/pipeline/mod.rs"]
pub mod pipeline;
#[path = "workflow/transfer/mod.rs"]
pub mod transfer;

#[path = "application/boot.rs"]
pub mod boot;
#[path = "application/job/mod.rs"]
pub mod job;
#[path = "application/run/mod.rs"]
pub mod run;
