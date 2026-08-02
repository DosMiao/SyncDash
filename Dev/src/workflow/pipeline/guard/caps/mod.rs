//! Backend limitations, reported for review rather than guessed around.
//!
//! The engine never silently works around a capability a root does not have. It reports the
//! limitation, and a run proceeds only once the user has accepted that exact set — which is why
//! the report and the consent that binds to it are separate from the two probes that produce them.

mod consent;
mod read;
mod report;
mod write;

#[cfg(test)]
mod tests;

pub use consent::{CapabilityConsent, CapabilityScope};
pub use read::{cap_report_read, ReadCapsQuery};
pub use report::{CapItem, CapReport, CapSeverity};
pub use write::{cap_report_write, WriteCapsQuery};
