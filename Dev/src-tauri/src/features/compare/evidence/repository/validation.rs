//! Final Apply validation against immutable retained Compare evidence.

use crate::contracts::compare::CompareOwner;

use super::super::model::error::CompareResultRepositoryError;
use super::super::model::result::RetainedCompareResult;

pub(crate) fn validate_retained_compare(
    retained: Option<&RetainedCompareResult>,
    owner: &CompareOwner,
    job_id: &str,
    job_name: &str,
    target_index: usize,
    config_revision: &str,
    plan_digest: Option<&str>,
) -> Result<(), String> {
    if owner.identity.job_id != job_id {
        return Err(format!(
            "This Compare result belongs to a different job identity than '{job_name}' — run Compare again"
        ));
    }
    if owner.identity.target_index != target_index {
        return Err(format!(
            "This Compare result belongs to target {}, not target {} — run Compare again",
            owner.identity.target_index + 1,
            target_index + 1
        ));
    }
    if owner.identity.config_revision != config_revision {
        return Err(format!(
            "Job '{job_name}' changed since this Compare — run Compare again"
        ));
    }
    let Some(retained) = retained else {
        return Err(CompareResultRepositoryError::NotRetained.to_string());
    };
    if retained.identity() != &owner.identity {
        return Err("The retained Compare result identity changed — run Compare again".into());
    }
    if let Some(plan_digest) = plan_digest {
        if retained.plan_digest() != plan_digest {
            return Err(
                "This Compare result no longer matches the plan produced by Compare — run Compare again"
                    .into(),
            );
        }
    }
    Ok(())
}
