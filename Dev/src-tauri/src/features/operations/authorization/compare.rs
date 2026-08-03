//! Compare launch origin and exact job-target binding.

use crate::features::autoscan::authority::AutoScanComparePermit;

use super::target::JobTargetRevision;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CompareOrigin {
    Interactive,
    AutoScan(AutoScanComparePermit),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CompareAuthorization {
    target: JobTargetRevision,
    origin: CompareOrigin,
}

impl CompareAuthorization {
    pub(crate) fn new(target: JobTargetRevision, origin: CompareOrigin) -> Result<Self, String> {
        Ok(Self { target, origin })
    }

    pub(crate) fn target(&self) -> &JobTargetRevision {
        &self.target
    }

    pub(crate) fn auto_scan_permit(&self) -> Option<&AutoScanComparePermit> {
        match &self.origin {
            CompareOrigin::Interactive => None,
            CompareOrigin::AutoScan(permit) => Some(permit),
        }
    }

    pub(crate) fn origin(&self) -> &CompareOrigin {
        &self.origin
    }

    pub(crate) fn verify_current(&self, current: &Self) -> Result<(), String> {
        if self.target != current.target {
            return Err("The authorized job, revision, or target changed — review again".into());
        }
        if self.origin != current.origin {
            return Err(
                "The Compare launch origin or AutoScan permit changed — review again".into(),
            );
        }
        Ok(())
    }
}
