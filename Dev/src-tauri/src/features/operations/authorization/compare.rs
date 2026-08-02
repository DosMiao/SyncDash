//! Compare launch origin and exact capability fingerprint.

use crate::features::operations::autoscan_authority::AutoScanComparePermit;

use super::target::JobTargetRevision;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CompareOrigin {
    Interactive,
    AutoScan(AutoScanComparePermit),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CompareAuthorization {
    target: JobTargetRevision,
    capability_review_digest: String,
    origin: CompareOrigin,
}

impl CompareAuthorization {
    pub(crate) fn new(
        target: JobTargetRevision,
        capability_review_digest: String,
        origin: CompareOrigin,
    ) -> Result<Self, String> {
        if capability_review_digest.is_empty() {
            return Err("The Compare capability review is incomplete".into());
        }
        Ok(Self {
            target,
            capability_review_digest,
            origin,
        })
    }

    pub(crate) fn target(&self) -> &JobTargetRevision {
        &self.target
    }

    pub(crate) fn capability_review_digest(&self) -> &str {
        &self.capability_review_digest
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
        if self.capability_review_digest != current.capability_review_digest {
            return Err("The backend capability report changed — review Compare again".into());
        }
        if self.origin != current.origin {
            return Err(
                "The Compare launch origin or AutoScan permit changed — review again".into(),
            );
        }
        Ok(())
    }
}
