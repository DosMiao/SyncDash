//! Atomic, identity- and revision-fenced mutations of registered jobs.

use std::path::Path;

use crate::job::model::{Job, SCHEMA};
use crate::job::persistence::codec::{file_schema_at, load_path, staged_job};
use crate::job::persistence::registry::{
    load_registered_path, lock_job_mutations, new_job_id, registered_job_path_in, validate_job_id,
};
use crate::job::persistence::types::{
    DeletedJob, JobMutationEffect, JobRootField, JobRootMutation, SavedJob,
};
use crate::job::revision::config_revision;

fn invalid_job(reason: String) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        format!("invalid job: {reason}"),
    )
}

fn current_revision_at(path: &Path) -> std::io::Result<String> {
    let (_, current) = load_path(path)?;
    config_revision(&current).map_err(invalid_job)
}

fn require_revision(path: &Path, name: &str, expected: &str) -> std::io::Result<()> {
    if !path.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("job not found: {name}"),
        ));
    }
    let current = current_revision_at(path)?;
    if current != expected {
        return Err(std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            format!(
                "job '{name}' changed on disk (expected revision {expected}, found {current}) — reload before saving"
            ),
        ));
    }
    Ok(())
}

fn load_expected_job(
    path: &Path,
    name: &str,
    expected_job_id: &str,
    expected_config_revision: &str,
) -> std::io::Result<Job> {
    if !path.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("job not found: {name}"),
        ));
    }
    let (_, current) = load_path(path)?;
    validate_job_id(&current.job_id).map_err(|error| {
        std::io::Error::new(
            error.kind(),
            format!("bad job file {}: {error}", path.display()),
        )
    })?;
    if current.job_id != expected_job_id {
        return Err(std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            format!(
                "job '{name}' was replaced since this editor loaded it (expected job_id '{expected_job_id}', found '{}') — reload before saving",
                current.job_id
            ),
        ));
    }
    let current_revision = config_revision(&current).map_err(invalid_job)?;
    if current_revision != expected_config_revision {
        return Err(std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            format!(
                "job '{name}' changed on disk (expected revision {expected_config_revision}, found {current_revision}) — reload before saving"
            ),
        ));
    }
    Ok(current)
}

fn rename_without_overwrite(source: &Path, destination: &Path) -> std::io::Result<()> {
    std::fs::hard_link(source, destination)?;
    if let Err(error) = std::fs::remove_file(source) {
        if let Err(rollback) = std::fs::remove_file(destination) {
            return Err(std::io::Error::new(
                error.kind(),
                format!(
                    "cannot remove the old job name: {error}; removing the collision-safe link also failed: {rollback}"
                ),
            ));
        }
        return Err(error);
    }
    Ok(())
}

fn require_target(job: &Job, target_index: usize) -> std::io::Result<()> {
    if target_index >= job.targets.len() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "target_index {target_index} is out of range for {} target(s)",
                job.targets.len()
            ),
        ));
    }
    Ok(())
}

fn target_mut(job: &mut Job, target_index: usize) -> &mut String {
    &mut job.targets[target_index]
}

fn mutate_job_roots_in<F>(
    dir: &Path,
    name: &str,
    expected_job_id: &str,
    expected_config_revision: &str,
    target_index: usize,
    mutate: F,
) -> std::io::Result<JobRootMutation>
where
    F: FnOnce(&mut Job) -> std::io::Result<()>,
{
    validate_job_id(expected_job_id)?;
    let _lock = lock_job_mutations(dir)?;
    let path = registered_job_path_in(dir, name)?;
    let mut current = load_expected_job(&path, name, expected_job_id, expected_config_revision)?;
    require_target(&current, target_index)?;
    mutate(&mut current)?;
    current.schema = SCHEMA;
    current.validate().map_err(invalid_job)?;
    let config_revision = config_revision(&current).map_err(invalid_job)?;
    let effect = if config_revision == expected_config_revision && file_schema_at(&path)? == SCHEMA
    {
        JobMutationEffect::NoOp
    } else {
        let staged = staged_job(&path, &current)?;
        load_expected_job(&path, name, expected_job_id, expected_config_revision)?;
        staged.commit()?;
        JobMutationEffect::Updated
    };
    Ok(JobRootMutation {
        mutation: SavedJob {
            name: name.to_string(),
            path,
            job_id: current.job_id,
            config_revision,
            effect,
            previous_name: None,
        },
        source: current.source,
        targets: current.targets,
    })
}

pub fn update_job_root(
    name: &str,
    expected_job_id: &str,
    expected_config_revision: &str,
    target_index: usize,
    field: JobRootField,
    value: &str,
) -> std::io::Result<JobRootMutation> {
    update_job_root_in(
        &crate::foundation::dirs::jobs_dir(),
        name,
        expected_job_id,
        expected_config_revision,
        target_index,
        field,
        value,
    )
}

fn update_job_root_in(
    dir: &Path,
    name: &str,
    expected_job_id: &str,
    expected_config_revision: &str,
    target_index: usize,
    field: JobRootField,
    value: &str,
) -> std::io::Result<JobRootMutation> {
    if value.trim().is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "root value cannot be empty",
        ));
    }
    mutate_job_roots_in(
        dir,
        name,
        expected_job_id,
        expected_config_revision,
        target_index,
        |current| {
            match field {
                JobRootField::Source => current.source = value.to_string(),
                JobRootField::Target => {
                    *target_mut(current, target_index) = value.to_string();
                }
            }
            Ok(())
        },
    )
}

pub fn swap_job_roots(
    name: &str,
    expected_job_id: &str,
    expected_config_revision: &str,
    target_index: usize,
) -> std::io::Result<JobRootMutation> {
    swap_job_roots_in(
        &crate::foundation::dirs::jobs_dir(),
        name,
        expected_job_id,
        expected_config_revision,
        target_index,
    )
}

fn swap_job_roots_in(
    dir: &Path,
    name: &str,
    expected_job_id: &str,
    expected_config_revision: &str,
    target_index: usize,
) -> std::io::Result<JobRootMutation> {
    mutate_job_roots_in(
        dir,
        name,
        expected_job_id,
        expected_config_revision,
        target_index,
        |current| {
            let previous_source = std::mem::take(&mut current.source);
            let previous_target =
                std::mem::replace(target_mut(current, target_index), previous_source);
            current.source = previous_target;
            Ok(())
        },
    )
}

/// Create, update, or rename one registered job without overwriting an unseen revision.
pub fn save_job(
    name: &str,
    job: &Job,
    original_name: Option<&str>,
    expected_revision: Option<&str>,
) -> std::io::Result<SavedJob> {
    job.validate().map_err(invalid_job)?;
    let job = Job {
        schema: SCHEMA,
        ..job.clone()
    };
    let config_revision = config_revision(&job).map_err(invalid_job)?;
    save_job_in(
        &crate::foundation::dirs::jobs_dir(),
        name,
        &job,
        original_name,
        expected_revision,
        config_revision,
    )
}

fn save_job_in(
    dir: &Path,
    name: &str,
    job: &Job,
    original_name: Option<&str>,
    expected_revision: Option<&str>,
    config_revision: String,
) -> std::io::Result<SavedJob> {
    let _lock = lock_job_mutations(dir)?;
    let destination = registered_job_path_in(dir, name)?;
    let mut persisted = job.clone();
    let mut effect = JobMutationEffect::Created;
    let mut previous_name = None;
    match (original_name, expected_revision) {
        (None, None) => {
            if !persisted.job_id.is_empty() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "a new job must not supply job_id; the registry assigns a fresh identity",
                ));
            }
            if destination.exists() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    format!("job '{name}' already exists — reload it before saving"),
                ));
            }
            persisted.job_id = new_job_id()?;
            let staged = staged_job(&destination, &persisted)?;
            if destination.exists() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    format!("job '{name}' was created while this save was being prepared"),
                ));
            }
            staged.commit()?;
        }
        (Some(original_name), Some(expected_revision)) => {
            let original = registered_job_path_in(dir, original_name)?;
            require_revision(&original, original_name, expected_revision)?;
            let (_, current) = load_registered_path(&original)?;
            if persisted.job_id != current.job_id {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::WouldBlock,
                    format!(
                        "job '{original_name}' was replaced since this editor loaded it (expected job_id '{}', found '{}') — reload before saving",
                        persisted.job_id, current.job_id
                    ),
                ));
            }
            if original == destination {
                if config_revision == expected_revision && file_schema_at(&original)? == SCHEMA {
                    return Ok(SavedJob {
                        name: name.to_string(),
                        path: destination,
                        job_id: persisted.job_id,
                        config_revision,
                        effect: JobMutationEffect::NoOp,
                        previous_name: None,
                    });
                }
                effect = JobMutationEffect::Updated;
                let staged = staged_job(&destination, &persisted)?;
                require_revision(&original, original_name, expected_revision)?;
                staged.commit()?;
            } else {
                effect = JobMutationEffect::Renamed;
                previous_name = Some(original_name.to_string());
                if destination.exists() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::AlreadyExists,
                        format!("cannot rename job '{original_name}' to '{name}': destination already exists"),
                    ));
                }
                let staged = staged_job(&destination, &persisted)?;
                require_revision(&original, original_name, expected_revision)?;
                if destination.exists() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::AlreadyExists,
                        format!("job '{name}' was created while this rename was being prepared"),
                    ));
                }
                rename_without_overwrite(&original, &destination)?;
                if let Err(error) = staged.commit() {
                    if let Err(rollback) = rename_without_overwrite(&destination, &original) {
                        return Err(std::io::Error::new(
                            error.kind(),
                            format!("cannot save renamed job: {error}; restoring the original name also failed: {rollback}"),
                        ));
                    }
                    return Err(error);
                }
            }
        }
        _ => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "original_name and expected_revision must be supplied together for an update",
            ));
        }
    }
    Ok(SavedJob {
        name: name.to_string(),
        path: destination,
        job_id: persisted.job_id,
        config_revision,
        effect,
        previous_name,
    })
}

pub fn delete_job(
    name: &str,
    expected_job_id: &str,
    expected_revision: &str,
) -> std::io::Result<DeletedJob> {
    delete_job_in(
        &crate::foundation::dirs::jobs_dir(),
        name,
        expected_job_id,
        expected_revision,
    )
}

fn delete_job_in(
    dir: &Path,
    name: &str,
    expected_job_id: &str,
    expected_revision: &str,
) -> std::io::Result<DeletedJob> {
    let _lock = lock_job_mutations(dir)?;
    let path = registered_job_path_in(dir, name)?;
    require_revision(&path, name, expected_revision)?;
    let (_, current) = load_registered_path(&path)?;
    if current.job_id != expected_job_id {
        return Err(std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            format!(
                "job '{name}' was replaced since this editor loaded it (expected job_id '{expected_job_id}', found '{}') — reload before deleting",
                current.job_id
            ),
        ));
    }
    std::fs::remove_file(path)?;
    Ok(DeletedJob {
        name: name.to_string(),
        job_id: current.job_id,
        config_revision: expected_revision.to_string(),
        effect: JobMutationEffect::Deleted,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn valid_job() -> Job {
        Job {
            source: "/data/source".into(),
            targets: vec!["/data/target".into()],
            ..Default::default()
        }
    }

    #[test]
    fn persistence_uses_atomic_create_and_revision_checked_update_rename_delete() {
        let dir = std::env::temp_dir().join(format!(
            "syncdash-job-cas-{}-{}",
            std::process::id(),
            crate::foundation::time::now_ms()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let first = valid_job();
        let first_revision = config_revision(&first).unwrap();
        let created =
            save_job_in(&dir, "photos", &first, None, None, first_revision.clone()).unwrap();
        assert_eq!(created.config_revision, first_revision);
        assert_eq!(created.effect, JobMutationEffect::Created);
        validate_job_id(&created.job_id).unwrap();

        let identified_first = Job {
            job_id: created.job_id.clone(),
            ..first.clone()
        };
        let no_op = save_job_in(
            &dir,
            "photos",
            &identified_first,
            Some("photos"),
            Some(&first_revision),
            first_revision.clone(),
        )
        .unwrap();
        assert_eq!(no_op.effect, JobMutationEffect::NoOp);

        let mut second = identified_first;
        second.exclude.push("*.tmp".into());
        let second_revision = config_revision(&second).unwrap();
        let stale = save_job_in(
            &dir,
            "photos",
            &second,
            Some("photos"),
            Some("stale-revision"),
            second_revision.clone(),
        )
        .unwrap_err();
        assert_eq!(stale.kind(), std::io::ErrorKind::WouldBlock);
        assert_eq!(current_revision_at(&created.path).unwrap(), first_revision);

        let updated = save_job_in(
            &dir,
            "photos",
            &second,
            Some("photos"),
            Some(&first_revision),
            second_revision.clone(),
        )
        .unwrap();
        assert_eq!(updated.effect, JobMutationEffect::Updated);
        assert_eq!(updated.job_id, created.job_id);

        let renamed = save_job_in(
            &dir,
            "archive",
            &second,
            Some("photos"),
            Some(&second_revision),
            second_revision.clone(),
        )
        .unwrap();
        assert_eq!(renamed.effect, JobMutationEffect::Renamed);
        assert_eq!(renamed.previous_name.as_deref(), Some("photos"));
        assert_eq!(renamed.job_id, created.job_id);
        assert!(!dir.join("photos.toml").exists());
        assert_eq!(current_revision_at(&renamed.path).unwrap(), second_revision);

        let stale_delete =
            delete_job_in(&dir, "archive", &renamed.job_id, &first_revision).unwrap_err();
        assert_eq!(stale_delete.kind(), std::io::ErrorKind::WouldBlock);
        let replaced_delete = delete_job_in(
            &dir,
            "archive",
            "ffffffffffffffffffffffffffffffff",
            &second_revision,
        )
        .unwrap_err();
        assert_eq!(replaced_delete.kind(), std::io::ErrorKind::WouldBlock);
        let deleted = delete_job_in(&dir, "archive", &renamed.job_id, &second_revision).unwrap();
        assert_eq!(deleted.effect, JobMutationEffect::Deleted);
        assert_eq!(deleted.job_id, renamed.job_id);
        assert!(!renamed.path.exists());

        let recreated =
            save_job_in(&dir, "archive", &first, None, None, first_revision.clone()).unwrap();
        assert_ne!(recreated.job_id, deleted.job_id);
        assert!(std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .all(|entry| !crate::fs::staged::is_temp_name(&entry.file_name().to_string_lossy())));
        let _ = std::fs::remove_dir_all(dir);
    }

    fn create_root_mutation_fixture(tag: &str) -> (PathBuf, SavedJob, String) {
        let dir = std::env::temp_dir().join(format!(
            "syncdash-job-root-{tag}-{}-{}",
            std::process::id(),
            crate::foundation::time::now_ms()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let job = Job {
            source: "/data/source".into(),
            targets: vec!["/data/one".into(), "/data/two".into()],
            ..Default::default()
        };
        let revision = config_revision(&job).unwrap();
        let saved = save_job_in(&dir, "photos", &job, None, None, revision.clone()).unwrap();
        (dir, saved, revision)
    }

    #[test]
    fn root_mutations_fence_job_identity_revision_and_target_index() {
        let (dir, saved, revision) = create_root_mutation_fixture("fencing");
        let wrong_identity = update_job_root_in(
            &dir,
            "photos",
            "ffffffffffffffffffffffffffffffff",
            &revision,
            0,
            JobRootField::Source,
            "/data/revised",
        )
        .unwrap_err();
        assert_eq!(wrong_identity.kind(), std::io::ErrorKind::WouldBlock);

        let stale_revision = update_job_root_in(
            &dir,
            "photos",
            &saved.job_id,
            "stale-revision",
            0,
            JobRootField::Source,
            "/data/revised",
        )
        .unwrap_err();
        assert_eq!(stale_revision.kind(), std::io::ErrorKind::WouldBlock);

        let missing_target = update_job_root_in(
            &dir,
            "photos",
            &saved.job_id,
            &revision,
            2,
            JobRootField::Target,
            "/data/revised",
        )
        .unwrap_err();
        assert_eq!(missing_target.kind(), std::io::ErrorKind::InvalidInput);
        assert_eq!(current_revision_at(&saved.path).unwrap(), revision);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn root_updates_preserve_multi_target_storage_and_report_no_op() {
        let (dir, saved, revision) = create_root_mutation_fixture("update");
        let updated = update_job_root_in(
            &dir,
            "photos",
            &saved.job_id,
            &revision,
            1,
            JobRootField::Target,
            "/data/revised-two",
        )
        .unwrap();
        assert_eq!(updated.mutation.effect, JobMutationEffect::Updated);
        assert_eq!(
            updated.targets,
            vec!["/data/one".to_string(), "/data/revised-two".to_string()]
        );
        let (_, persisted) = load_path(&updated.mutation.path).unwrap();
        assert_eq!(persisted.targets, updated.targets);

        let no_op = update_job_root_in(
            &dir,
            "photos",
            &saved.job_id,
            &updated.mutation.config_revision,
            1,
            JobRootField::Target,
            "/data/revised-two",
        )
        .unwrap();
        assert_eq!(no_op.mutation.effect, JobMutationEffect::NoOp);
        assert_eq!(
            no_op.mutation.config_revision,
            updated.mutation.config_revision
        );

        let source_updated = update_job_root_in(
            &dir,
            "photos",
            &saved.job_id,
            &no_op.mutation.config_revision,
            0,
            JobRootField::Source,
            "/data/revised-source",
        )
        .unwrap();
        assert_eq!(source_updated.source, "/data/revised-source");
        assert_eq!(source_updated.targets, no_op.targets);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn root_swap_is_atomic_and_root_updates_are_fully_validated() {
        let (dir, saved, revision) = create_root_mutation_fixture("swap");
        let swapped = swap_job_roots_in(&dir, "photos", &saved.job_id, &revision, 1).unwrap();
        assert_eq!(swapped.source, "/data/two");
        assert_eq!(
            swapped.targets,
            vec!["/data/one".to_string(), "/data/source".to_string()]
        );
        assert_eq!(swapped.mutation.effect, JobMutationEffect::Updated);

        let empty = update_job_root_in(
            &dir,
            "photos",
            &saved.job_id,
            &swapped.mutation.config_revision,
            0,
            JobRootField::Source,
            "  ",
        )
        .unwrap_err();
        assert_eq!(empty.kind(), std::io::ErrorKind::InvalidInput);

        let duplicate = update_job_root_in(
            &dir,
            "photos",
            &saved.job_id,
            &swapped.mutation.config_revision,
            0,
            JobRootField::Target,
            "/data/source",
        )
        .unwrap_err();
        assert_eq!(duplicate.kind(), std::io::ErrorKind::InvalidInput);
        assert_eq!(
            current_revision_at(&swapped.mutation.path).unwrap(),
            swapped.mutation.config_revision
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn single_target_root_mutations_keep_canonical_list_storage() {
        let dir = std::env::temp_dir().join(format!(
            "syncdash-job-root-single-{}-{}",
            std::process::id(),
            crate::foundation::time::now_ms()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let job = valid_job();
        let revision = config_revision(&job).unwrap();
        let saved = save_job_in(&dir, "photos", &job, None, None, revision.clone()).unwrap();

        let updated = update_job_root_in(
            &dir,
            "photos",
            &saved.job_id,
            &revision,
            0,
            JobRootField::Target,
            "/data/revised-target",
        )
        .unwrap();
        assert_eq!(updated.targets, vec!["/data/revised-target"]);
        let (_, persisted_update) = load_path(&updated.mutation.path).unwrap();
        assert_eq!(persisted_update.targets, vec!["/data/revised-target"]);

        let swapped = swap_job_roots_in(
            &dir,
            "photos",
            &saved.job_id,
            &updated.mutation.config_revision,
            0,
        )
        .unwrap();
        assert_eq!(swapped.source, "/data/revised-target");
        assert_eq!(swapped.targets, vec!["/data/source"]);
        let (_, persisted_swap) = load_path(&swapped.mutation.path).unwrap();
        assert_eq!(persisted_swap.targets, vec!["/data/source"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn create_and_rename_never_overwrite_an_existing_job() {
        let dir = std::env::temp_dir().join(format!(
            "syncdash-job-collision-{}-{}",
            std::process::id(),
            crate::foundation::time::now_ms()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let job = valid_job();
        let revision = config_revision(&job).unwrap();
        let one = save_job_in(&dir, "one", &job, None, None, revision.clone()).unwrap();
        save_job_in(&dir, "two", &job, None, None, revision.clone()).unwrap();

        let create = save_job_in(&dir, "one", &job, None, None, revision.clone()).unwrap_err();
        assert_eq!(create.kind(), std::io::ErrorKind::AlreadyExists);
        let identified = Job {
            job_id: one.job_id,
            ..job
        };
        let rename = save_job_in(
            &dir,
            "two",
            &identified,
            Some("one"),
            Some(&revision),
            revision.clone(),
        )
        .unwrap_err();
        assert_eq!(rename.kind(), std::io::ErrorKind::AlreadyExists);
        assert!(dir.join("one.toml").is_file());
        assert!(dir.join("two.toml").is_file());
        let _ = std::fs::remove_dir_all(dir);
    }
}
