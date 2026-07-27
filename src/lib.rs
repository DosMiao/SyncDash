//! syncdash core library: the CLI (main.rs) and the Tauri desktop shell (src-tauri) share this logic.
//! No GUI in the library — the old egui UI belongs to the CLI bin, the new web UI to src-tauri.
//!
//! # Layering
//!
//! Dependencies point **downward only**. Reaching upward is allowed, but state why in the module header.
//!
//! ```text
//! L4 shells        src/main.rs (CLI) · src-tauri (desktop)
//! L3 orchestration config · run
//! L2 domain        filter · scan · compare · apply · pack · preflight · lock · remote · version · trash
//! L1 services      progress · logging · runlog · settings
//! L0 foundation    foundation (zero in-crate deps) · table · chunk · atomic
//! ```
//!
//! `foundation` is the one layer guaranteed to have zero in-crate dependencies: everyone may use it, it uses no one.
//! The duplicated implementations (timestamp conversion ×3, byte formatting ×3, rel→native ×8,
//! compare-key folding ×2 with inconsistent semantics) all converge there.

pub mod foundation;

pub mod apply;
pub mod atomic;
pub mod chunk;
pub mod compare;
pub mod config;
pub mod filter;
pub mod lock;
pub mod logging;
pub mod pack;
pub mod preflight;
pub mod progress;
pub mod remote;
pub mod run;
pub mod runlog;
pub mod scan;
pub mod settings;
pub mod table;
pub mod territory;
pub mod trash;
pub mod vclock;
pub mod version;
