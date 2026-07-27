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
//! L3  orchestration  run · job
//! L2  domain         pipeline (scan/compare/apply/filter/guard) · transfer (remote/pack)
//! L1  services       obs (progress/logging/runlog) · store (settings/trash/version)
//! L0  foundation     foundation · model (plan/event/table/chunk/vclock) · fs (staged/lock)
//! ```
//!
//! Verified mechanically, not by assertion: Tarjan over the comment-stripped sources reports no
//! strongly-connected component larger than one, and no edge points up the ladder.
//!
//! `model` holds **vocabulary** — the plan format and the event schema — while the engines that
//! produce them live in `pipeline` and `obs`. That split is what keeps the graph acyclic: both
//! `store::version` and `obs::runlog` persist ops, and `store::settings` needs `LogLevel`, so
//! leaving those types inside their engines forced service modules to reach up into the domain
//! layer for a struct definition.
//!
//! `foundation` is the one layer guaranteed to have zero in-crate dependencies: everyone may use
//! it, it uses no one. The implementations that used to be copied per call site — timestamp
//! conversion ×3, byte formatting ×3, rel→native ×8, compare-key folding ×2 with inconsistent
//! semantics — all converge there.
//!
//! Two shape rules, both taken from the reference project:
//! - A single-file domain stays flat at its parent (`transfer/remote.rs`); only a multi-file
//!   domain earns a directory. Nesting for its own sake just lengthens paths.
//! - **No re-export hubs.** Every `mod.rs` carries real content, and callers write the full path
//!   (`foundation::fmt::human_bytes`, never a re-export from wherever happens to be convenient).
//!   A barrel erases who depends on whom, which is precisely what this layering exists to show.

pub mod foundation;

pub mod fs;
pub mod model;

pub mod obs;
pub mod store;

pub mod pipeline;
pub mod transfer;

pub mod job;
pub mod run;
