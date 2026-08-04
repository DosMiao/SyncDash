//! AutoScan identity, wire status, trigger, and terminal outcome vocabulary.

use std::time::Duration;

use serde::Serialize;

use crate::contracts::compare::CompareOwner;
use crate::features::compare::evidence::model::scope::CompareScope;
use crate::features::compare::evidence::model::verification::CompareVerificationTerminalOutcome;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AutoScanBinding {
    pub(crate) job_id: String,
    pub(crate) job_name: String,
    pub(crate) config_revision: String,
    pub(crate) target_index: usize,
    pub(crate) interval_secs: u64,
    pub(crate) auto_apply: bool,
    pub(crate) rigor: String,
}

impl AutoScanBinding {
    pub(crate) fn interval(&self) -> Duration {
        Duration::from_secs(self.interval_secs.max(1))
    }

    /// Only the native FSEvents lane persists a resume cursor, so only it names a checkpoint owner.
    #[cfg(target_os = "macos")]
    pub(super) fn checkpoint_owner(&self) -> String {
        format!(
            "autoscan-v2\0{}\0{}\0{}",
            self.job_id, self.config_revision, self.target_index
        )
    }

    pub(crate) fn owns_compare(&self, owner: &CompareOwner) -> bool {
        owner.identity.job_id == self.job_id
            && owner.identity.config_revision == self.config_revision
            && owner.identity.target_index == self.target_index
    }

    pub(super) fn compare_scope(&self) -> CompareScope {
        CompareScope::new(&self.job_id, self.target_index, &self.config_revision)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../Dev/typescript/core/types/generated/")]
pub(crate) enum AutoScanStatusMode {
    Starting,
    NativeFsevents,
    Polling,
}

/// The generated TypeScript union is one contract for every platform, so the full variant set is
/// compiled everywhere even though only macOS builds contain a producer for `NativeFsevents`.
#[cfg_attr(
    not(target_os = "macos"),
    expect(dead_code, reason = "no native detection lane on this platform")
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../Dev/typescript/core/types/generated/")]
pub(crate) enum AutoScanDetectionMode {
    NativeFsevents,
    Polling,
}

impl From<AutoScanDetectionMode> for AutoScanStatusMode {
    fn from(mode: AutoScanDetectionMode) -> Self {
        match mode {
            AutoScanDetectionMode::NativeFsevents => Self::NativeFsevents,
            AutoScanDetectionMode::Polling => Self::Polling,
        }
    }
}

/// Both filesystem-event reasons are reported by the native lane alone; the remaining two are
/// reported everywhere. The union stays whole so the wire vocabulary does not vary by platform.
#[cfg_attr(
    not(target_os = "macos"),
    expect(dead_code, reason = "no native detection lane on this platform")
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../Dev/typescript/core/types/generated/")]
pub(crate) enum AutoScanTriggerReason {
    Bootstrap,
    FilesystemChange,
    WatchInvalidated,
    PeriodicVerification,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq, ts_rs::TS)]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../Dev/typescript/core/types/generated/")]
pub(crate) struct AutoScanStatusDto {
    pub(crate) active: bool,
    #[ts(type = "number")]
    pub(crate) generation: u64,
    pub(crate) job_id: Option<String>,
    pub(crate) job_name: Option<String>,
    pub(crate) config_revision: Option<String>,
    #[ts(type = "number | null")]
    pub(crate) target_index: Option<usize>,
    #[ts(type = "number | null")]
    pub(crate) interval_secs: Option<u64>,
    pub(crate) auto_apply: bool,
    pub(crate) mode: Option<AutoScanStatusMode>,
    pub(crate) detail: String,
    /// Monotonic within one generation and retained after completion, so delayed same-generation
    /// IPC snapshots cannot make older ticket state appear current.
    #[ts(type = "number")]
    pub(crate) latest_ticket_id: u64,
    #[ts(type = "number | null")]
    pub(crate) active_ticket: Option<u64>,
    pub(crate) pending_trigger: Option<AutoScanTriggerDto>,
}

impl AutoScanStatusDto {
    pub(super) fn inactive() -> Self {
        Self {
            active: false,
            generation: 0,
            job_id: None,
            job_name: None,
            config_revision: None,
            target_index: None,
            interval_secs: None,
            auto_apply: false,
            mode: None,
            detail: "AutoScan is off".into(),
            latest_ticket_id: 0,
            active_ticket: None,
            pending_trigger: None,
        }
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq, ts_rs::TS)]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../Dev/typescript/core/types/generated/")]
pub(crate) struct AutoScanTriggerDto {
    #[ts(type = "number")]
    pub(crate) generation: u64,
    #[ts(type = "number")]
    pub(crate) ticket_id: u64,
    pub(crate) job_id: String,
    pub(crate) job_name: String,
    pub(crate) config_revision: String,
    #[ts(type = "number")]
    pub(crate) target_index: usize,
    pub(crate) auto_apply: bool,
    pub(crate) mode: AutoScanDetectionMode,
    pub(crate) reason: AutoScanTriggerReason,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum AutoScanVerificationTerminal {
    Failed(String),
    Cancelled,
}

impl AutoScanVerificationTerminal {
    pub(super) fn repository_outcome(&self) -> CompareVerificationTerminalOutcome {
        match self {
            Self::Failed(message) => CompareVerificationTerminalOutcome::Failed {
                message: format!("AutoScan verification failed: {message}"),
            },
            Self::Cancelled => CompareVerificationTerminalOutcome::Cancelled,
        }
    }

    pub(super) fn status_detail(&self) -> String {
        match self {
            Self::Failed(message) => format!("Verification failed: {message}"),
            Self::Cancelled => "Verification was cancelled; waiting for changes".into(),
        }
    }
}
