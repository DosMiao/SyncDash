//! Fully resolved registered-job target and its stable configuration revision.

pub(in crate::features::operations) struct ResolvedJobTarget {
    pub(in crate::features::operations) job_name: String,
    pub(in crate::features::operations) registered_job: syncdash::job::Job,
    pub(in crate::features::operations) target_index: usize,
    pub(in crate::features::operations) target_job: syncdash::job::SingleTargetJob,
    pub(in crate::features::operations) config_revision: String,
}
