//! L0 filesystem primitives that own a lifecycle.
//!
//! `chunk` streams content-defined chunks, `staged` owns atomic local writes, `lock` owns root
//! leases, and `vfs` provides local and protocol backends. `ssh` is shared by SFTP and peer
//! execution. `watch` may trigger Compare but never replaces a verified snapshot.
//!
//! This module declares those owners and nothing else. Mutation of user files goes through
//! `local_root`, whose handles resolve every path segment from a retained root descriptor, so a
//! path can never address outside the root it was opened against — an ambient-path helper here
//! would be a way around that guarantee.

pub mod chunk;
pub mod local_root;
pub mod lock;
pub mod ssh;
pub mod staged;
pub mod vfs;
pub mod watch;
