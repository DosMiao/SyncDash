//! syncdash core library: the CLI (main.rs) and the Tauri desktop shell (src-tauri) share this logic.
//! No GUI in the library — the web UI belongs to src-tauri.
//!
//! # Layering
//!
//! One directory per layer, dependencies pointing **downward only**. Reaching upward is allowed,
//! but the module header has to say why.
//!
//! ```text
//! L4  shells         src/main.rs (CLI) · src-tauri (desktop)
//! L3  orchestration  run · job · boot (process startup)
//! L2  domain         pipeline (scan/compare/apply/filter/guard) · transfer (peer/pack)
//! L1  services       obs (progress/logging/runlog) · store (settings/trash/version)
//! L0  foundation     foundation · model (plan/event/table/chunk) · fs (staged/lock)
//! ```
//!
//! Verified mechanically, not by assertion: Tarjan over the comment-stripped **files** reports no
//! strongly-connected component larger than one. Two things that reading `use crate::` will not
//! show, both accepted rather than engineered away:
//! - The `log_*!` macros are `#[macro_export]` and expand to `$crate::obs::logging::emit`, so a
//!   call site carries an edge to L1 that no `use` declares. `fs::lock` and `fs::vfs::local` each
//!   have one, and each header says why.
//! - Counting those, the *directory* graph has one cycle: `fs → obs → store → fs`. Being able to
//!   log from anywhere is worth more than an acyclic directory graph. It is still a cycle — the
//!   file-level claim above is the one that holds unqualified.
//!
//! `model` holds **vocabulary** — the plan format and the event schema — while the engines that
//! produce them live in `pipeline` and `obs`. That split is what keeps the *file* graph acyclic:
//! both `store::version` and `obs::runlog` persist ops, and `store::settings` needs `LogLevel`, so
//! leaving those types inside their engines forced service modules to reach up into the domain
//! layer for a struct definition.
//!
//! `foundation` is the one layer guaranteed to have zero in-crate dependencies: everyone may use
//! it, it uses no one. Timestamp conversion (was 3 copies), rel→native (8) and compare-key folding
//! (2, with inconsistent semantics) all converge there. Byte formatting converges too, but with a
//! deliberate second implementation: `typescript/core/format.ts` renders the same units for the
//! GUI, and the two are kept in step by hand — see `foundation::fmt`.
//!
//! Two shape rules, both taken from the reference project:
//! - A single-file domain stays flat at its parent (`boot.rs`, `pipeline/filter.rs`); only a
//!   multi-file domain earns a directory. Nesting for its own sake just lengthens paths.
//! - **No re-export hubs.** Every `mod.rs` carries real content, and callers write the full path
//!   (`foundation::fmt::human_bytes`, never a re-export from wherever happens to be convenient).
//!   A barrel erases who depends on whom, which is precisely what this layering exists to show.
//!
//! # No file is ever memory-mapped
//!
//! `blake3` is depended on without its `mmap` and `rayon` features on purpose, which makes
//! `update_mmap`, `update_mmap_rayon` and `update_rayon` compile errors rather than conventions.
//! Every hash that reaches a file's bytes reaches them through an explicit `File::read` loop or a
//! `Vfs` read stream. (`blake3::hash` over a buffer already in memory is untouched by this — the
//! hazard is the mapping, not the hashing.)
//!
//! Reading a mapped page whose backing file was truncated, or whose volume went away, raises
//! **SIGBUS**. That is a signal, not an `io::Error`: the surrounding `?` and `Err` arms are
//! unreachable and the process is killed outright. On macOS a mounted SMB/AFP share under
//! `/Volumes` is an ordinary local path, so a Wi-Fi roam was enough to trigger it. In `apply` the
//! consequence is not a lost hash — the process dies mid-write holding both root lease ledgers,
//! `fs::lock::RootLock::drop` never runs, and the locks are left on disk for a human to clear. The
//! answer is not a SIGBUS handler in a process that writes user files; it is to not map the file,
//! so a vanishing file arrives as the ordinary `Err` each site already handles.
//!
//! This is paid for. Measured on a 15-core M-series with the file in page cache, `update_mmap_rayon`
//! hashes at ~19 GiB/s against ~1.9 GiB/s for a single-threaded read loop. Blake3 gets no
//! intra-file multicore any more; ~1.9 GiB/s becomes the ceiling, above most disks but far below
//! cache. The cost was accepted rather than gated on a probed `Medium`, because `Medium::FixedDisk`
//! would narrow the window without closing it (a read error or a detached volume still raises
//! SIGBUS on a fixed disk), `Medium::Unknown` is a normal probe result that would silently lose the
//! fast path anyway, and correctness outranks throughput here by the priority order this tool is
//! built on.
//!
//! One word to keep straight, because it used to name two opposite things. A **peer** job is one
//! the far side's own syncdash executes (`peer://`, `run::peer`, `transfer::peer`); everything
//! else runs in this process, however distant its roots — an `sftp://` root is reached over a
//! network but read and written *here*, down `pipeline::scan::vfs`. "Remote" answered both
//! questions and so answered neither; it survives only in the run-log's stored `kind` strings,
//! where changing it would make a user's existing history read differently.

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
