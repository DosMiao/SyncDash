//! Atomic, identity- and revision-fenced mutations of registered jobs.

use super::delete::*;
use super::fence::*;
use super::roots::*;
use super::save::*;

use crate::job::model::Job;
use crate::job::persistence::codec::load_path;
use crate::job::persistence::registry::validate_job_id;
use crate::job::persistence::types::{JobMutationEffect, JobRootField, SavedJob};
use crate::job::revision::config_revision;
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
    let created = save_job_in(&dir, "photos", &first, None, None, first_revision.clone()).unwrap();
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
