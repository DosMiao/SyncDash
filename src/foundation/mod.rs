//! L0 foundation layer: zero in-crate dependencies, std and third-party crates only.
//!
//! Contract: it depends on nothing, everything may depend on it. Any `use crate::…` breaks the layering; review sends it straight back.
//! No re-export hubs — callers write `foundation::fmt::human_bytes`; a hub would erase the dependency relationship.

pub mod fmt;
pub mod names;
pub mod dirs;
pub mod disk;
pub mod path;
pub mod text;
pub mod time;
