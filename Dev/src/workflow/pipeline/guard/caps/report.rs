//! What a backend cannot do, expressed as reviewable items.
//!
//! An item is not an error and it is not a gate. It says what the job asked for, what this backend
//! actually offers, and what that means for the run. The list is shown before the run and the run
//! then proceeds — a single operator working on their own machines does not need the engine to
//! withhold their data until they countersign its own findings. Severity orders the list by how
//! much the run departs from what the job asked for; nothing consumes it as a decision.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapSeverity {
    /// Stated for the record; the run is what the job asked for.
    Info,
    /// The run still does the job, by different means than the job named.
    Degraded,
    /// The safeguard the job asked for cannot be provided by this backend at all.
    Unavailable,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CapItem {
    /// The job feature concerned ("evidence=sampled", "mtime window", …)
    pub feature: String,
    /// "source" | "target" | "both"
    pub side: String,
    pub severity: CapSeverity,
    /// What the job asked for
    pub requested: String,
    /// What the backend can give
    pub actual: String,
    /// What this run will therefore do — in plain words, shown verbatim
    pub effect: String,
}

impl CapItem {
    pub fn render(&self) -> String {
        format!(
            "[{}] {}: wanted {}, backend has {} — {}",
            self.side, self.feature, self.requested, self.actual, self.effect
        )
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CapReport {
    pub items: Vec<CapItem>,
}

impl CapReport {
    pub fn unavailable(&self) -> Vec<&CapItem> {
        self.items
            .iter()
            .filter(|item| item.severity == CapSeverity::Unavailable)
            .collect()
    }

    /// The pre-run list, most-departing first. Ordering is presentation only, and it is stable
    /// within a severity so that adding a backend probe cannot reshuffle the rest of the list.
    pub fn listed(&self) -> Vec<&CapItem> {
        let mut items: Vec<&CapItem> = self.items.iter().collect();
        items.sort_by_key(|right| std::cmp::Reverse(right.severity));
        items
    }
}
