//! What a backend cannot do, expressed as reviewable items.
//!
//! An item is not an error. It says what the job asked for, what this backend actually offers, and
//! what that means for the run — because the user, not the engine, decides whether a limitation is
//! acceptable. The digest over these items is what makes a granted consent specific: change any
//! field of any item and the grant no longer applies.

use super::consent::{CapabilityConsent, CapabilityScope};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapSeverity {
    /// Running would make the table or the plan lie — refuse outright.
    Block,
    /// Runnable, but only with the user's explicit consent (`--accept-caps` / a ticked box).
    NeedsAck,
    /// Stated for the record; nothing to decide.
    Info,
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
    pub fn blockers(&self) -> Vec<&CapItem> {
        self.items
            .iter()
            .filter(|i| i.severity == CapSeverity::Block)
            .collect()
    }
    pub fn needs_ack(&self) -> Vec<&CapItem> {
        self.items
            .iter()
            .filter(|i| i.severity == CapSeverity::NeedsAck)
            .collect()
    }
    pub fn infos(&self) -> Vec<&CapItem> {
        self.items
            .iter()
            .filter(|i| i.severity == CapSeverity::Info)
            .collect()
    }

    /// A stable, domain-separated identity for exactly the consent-requiring capability facts.
    /// Item insertion order is deliberately irrelevant: adding a backend probe must not invalidate
    /// an otherwise identical grant merely because two independent checks ran in another order.
    pub fn consent_digest(&self, scope: CapabilityScope) -> String {
        let mut items = self.needs_ack();
        items.sort_by(|left, right| {
            (
                left.side.as_str(),
                left.feature.as_str(),
                left.requested.as_str(),
                left.actual.as_str(),
                left.effect.as_str(),
            )
                .cmp(&(
                    right.side.as_str(),
                    right.feature.as_str(),
                    right.requested.as_str(),
                    right.actual.as_str(),
                    right.effect.as_str(),
                ))
        });
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"syncdash-capability-consent-v1\0");
        hash_field(&mut hasher, scope.as_str().as_bytes());
        for item in items {
            hash_field(&mut hasher, item.side.as_bytes());
            hash_field(&mut hasher, item.feature.as_bytes());
            hash_field(&mut hasher, item.requested.as_bytes());
            hash_field(&mut hasher, item.actual.as_bytes());
            hash_field(&mut hasher, item.effect.as_bytes());
        }
        hasher.finalize().to_hex().to_string()
    }

    pub fn consent_satisfied(&self, scope: CapabilityScope, consent: &CapabilityConsent) -> bool {
        self.needs_ack().is_empty()
            || match consent {
                CapabilityConsent::None => false,
                CapabilityConsent::ExactDigest(digest) => digest == &self.consent_digest(scope),
                CapabilityConsent::ExplicitCli => true,
            }
    }
}

fn hash_field(hasher: &mut blake3::Hasher, value: &[u8]) {
    hasher.update(&(value.len() as u64).to_le_bytes());
    hasher.update(value);
}
