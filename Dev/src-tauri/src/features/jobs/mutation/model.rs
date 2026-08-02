use crate::contracts::compare::CompareScopeExecutionStatusDto;
use crate::features::autoscan::model::AutoScanStatusDto;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct SavedJobMutationFacts {
    pub(super) renamed: bool,
    pub(super) configuration_changed: bool,
}

pub(crate) struct JobMutationStatusEvents {
    pub(crate) autoscan: Option<AutoScanStatusDto>,
    pub(crate) compare_execution: Vec<CompareScopeExecutionStatusDto>,
}
