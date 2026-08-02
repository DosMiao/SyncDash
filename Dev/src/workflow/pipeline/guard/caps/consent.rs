//! Whether the user has accepted this exact set of limitations.
//!
//! Consent is scoped and digest-bound. A blanket --accept-caps covers NeedsAck items only; nothing
//! buys past a Block, because a Block means the safeguard the job asked for cannot be provided at
//! all.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityScope {
    CompareRead,
    ApplyWrite,
}

impl CapabilityScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CompareRead => "compare_read",
            Self::ApplyWrite => "apply_write",
        }
    }
}

/// Capability consent is explicit about its authority boundary.
///
/// The CLI flag remains intentionally broad for that one foreground invocation. Desktop review
/// uses only `ExactDigest`; a boolean from a webview can never accept a report that changed after
/// it was displayed.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum CapabilityConsent {
    #[default]
    None,
    ExactDigest(String),
    ExplicitCli,
}

impl CapabilityConsent {
    pub fn explicit_cli(accepted: bool) -> Self {
        if accepted {
            Self::ExplicitCli
        } else {
            Self::None
        }
    }
}
