//! Run-purpose vocabulary and progress-window lifecycle wire decisions.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RunPurpose {
    Compare,
    Apply,
}

impl RunPurpose {
    pub(super) const fn name(self) -> &'static str {
        match self {
            Self::Compare => "Compare",
            Self::Apply => "Apply",
        }
    }
}

#[derive(Clone, Copy, Debug, serde::Serialize, PartialEq, Eq, ts_rs::TS)]
#[serde(tag = "decision", rename_all = "snake_case")]
#[cfg_attr(feature = "export-types", ts(export))]
#[ts(export_to = "../Dev/typescript/core/types/generated/")]
pub(crate) enum ProgressWindowCloseDecisionDto {
    PendingLaunchCancelled,
    ActiveRunCancellationRequested {
        #[ts(type = "number")]
        run_id: u64,
    },
    NoInteractiveLaunch,
}
