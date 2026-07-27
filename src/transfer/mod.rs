//! L2 moving work to another machine.
//!
//! `remote` is the ssh transport and shell-dialect quoting; `pack` is the tar container
//! (plan + payload + manifest with per-file and combined hashes) that the far side
//! verifies before it executes anything.

pub mod pack;
pub mod remote;
