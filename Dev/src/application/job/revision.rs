//! Stable configuration identity for optimistic concurrency and evidence ownership.

use crate::job::model::{Job, SCHEMA};

/// Stable identity of one effective job configuration.
///
/// Hash the migrated, current-schema `Job` value rather than the TOML bytes on disk: formatting,
/// comments, and key order are not configuration changes, while every field the engine can observe
/// is. `job_id` is registry metadata rather than engine policy, so it is cleared before encoding.
/// The domain prefix versions this canonical encoding independently of the job-file schema.
pub fn config_revision(job: &Job) -> Result<String, String> {
    for (field, value) in [
        ("min_free_pct", job.min_free_pct),
        ("max_delete_ratio", job.max_delete_ratio),
    ] {
        if !value.is_finite() {
            return Err(format!(
                "cannot identify this job configuration: {field} must be a finite number"
            ));
        }
    }
    let canonical = Job {
        schema: SCHEMA,
        job_id: String::new(),
        ..job.clone()
    };
    let bytes = serde_json::to_vec(&canonical)
        .map_err(|e| format!("cannot identify this job configuration: {e}"))?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"syncdash-job-config-v2\0");
    hasher.update(&bytes);
    Ok(hasher.finalize().to_hex().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revision_is_stable_for_the_effective_job_and_changes_with_configuration() {
        let original = Job {
            schema: 1,
            source: "source".into(),
            targets: vec!["target".into()],
            exclude: vec!["*.tmp".into()],
            ..Default::default()
        };
        let current_schema = Job {
            schema: SCHEMA,
            ..original.clone()
        };

        let revision = config_revision(&original).unwrap();
        assert_eq!(revision.len(), 64);
        assert_eq!(revision, config_revision(&current_schema).unwrap());

        let identified = Job {
            job_id: "0123456789abcdef0123456789abcdef".into(),
            ..current_schema.clone()
        };
        assert_eq!(
            revision,
            config_revision(&identified).unwrap(),
            "registry identity is not an engine configuration change"
        );

        let changed = Job {
            exclude: vec!["*.tmp".into(), "*.bak".into()],
            ..current_schema
        };
        assert_ne!(revision, config_revision(&changed).unwrap());
    }

    #[test]
    fn revision_rejects_non_finite_configuration_numbers() {
        let job = Job {
            min_free_pct: f64::NAN,
            ..Default::default()
        };
        let error = config_revision(&job).unwrap_err();
        assert!(error.contains("min_free_pct"), "{error}");
        assert!(error.contains("finite"), "{error}");
    }
}
