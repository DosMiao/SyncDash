//! Backend limitations, reported for review rather than guessed around.
//!
//! The engine never silently works around a capability a root does not have: it states the
//! limitation and what the run will therefore do, then runs. The report is produced by two probes,
//! one per direction, and is consumed only for display.

mod read;
mod report;
mod write;

#[cfg(test)]
mod tests;

pub use read::{cap_report_read, ReadCapsQuery};
pub use report::{CapItem, CapReport, CapSeverity};
pub use write::{cap_report_write, WriteCapsQuery};
