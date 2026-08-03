//! Surviving a process restart.
//!
//! After a restart an immutable result is still viewable but its execution authority is expired,
//! because the filesystem evidence it was proven against is no longer known to be current. These
//! also pin that the random 128-bit result_id prevents a run-number collision across restarts, and
//! that a corrupt artifact stops startup rather than being skipped.

use crate::contracts::compare::{
    CompareExecutionExpiryReasonDto, CompareScopeExecutionStatusDto, CompareWorkspaceLookupDto,
};

use super::super::super::model::error::*;
use super::super::super::state::HOT_RESULT_CACHE_CAPACITY;
use super::super::*;
use super::fixtures::*;

struct TestDirectory(std::path::PathBuf);

impl TestDirectory {
    fn new(label: &str) -> Self {
        let token = crate::secure_random::random_hex::<8>("test directory").unwrap();
        let path = std::env::temp_dir().join(format!(
            "syncdash-compare-results-{label}-{}-{token}",
            std::process::id()
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn durable_result_restores_after_restart_but_execution_is_expired() {
    let directory = TestDirectory::new("restart");
    let result = identity("job-a", 0, "revision-a", 1);
    {
        let repository = CompareResultRepository::open_at(directory.0.clone()).unwrap();
        publish(&repository, version("job-a", "A", 0, "revision-a", 1));
    }

    let repository = CompareResultRepository::open_at(directory.0.clone()).unwrap();
    assert!(repository.store.lock().unwrap().versions_by_id.is_empty());
    let restored = repository
        .restore_workspace("job-a", 0, "revision-a")
        .unwrap();
    assert!(matches!(
        restored,
        CompareWorkspaceLookupDto::Found { workspace }
            if workspace.plan.owner.identity == result
                && matches!(
                    workspace.execution_status,
                    CompareScopeExecutionStatusDto::Expired {
                        reason: CompareExecutionExpiryReasonDto::ApplicationRestarted,
                        ..
                    }
                )
    ));
    assert!(repository.get_fresh_exact(&result).is_err());
}

#[test]
fn stable_result_id_prevents_run_number_collision_after_restart() {
    let directory = TestDirectory::new("run-id-collision");
    let older = identity("job-a", 0, "revision-a", 1);
    {
        let repository = CompareResultRepository::open_at(directory.0.clone()).unwrap();
        publish(&repository, version("job-a", "A", 0, "revision-a", 1));
    }

    let repository = CompareResultRepository::open_at(directory.0.clone()).unwrap();
    let mut newer_version = version("job-a", "A", 0, "revision-a", 1);
    newer_version.owner.identity.result_id = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee".into();
    let newer = newer_version.owner.identity.clone();
    publish(&repository, newer_version);

    assert_eq!(older.compare_run_id, newer.compare_run_id);
    assert_ne!(older.result_id, newer.result_id);
    assert!(repository.get_exact(&older).unwrap().is_some());
    assert_eq!(
        repository
            .latest_for("job-a", 0, "revision-a")
            .unwrap()
            .unwrap()
            .identity(),
        &newer
    );
}

#[test]
fn hot_cache_eviction_never_changes_durable_retention() {
    let directory = TestDirectory::new("hot-cache");
    let repository = CompareResultRepository::open_at(directory.0.clone()).unwrap();
    for run_id in 1..=6 {
        publish(
            &repository,
            version(
                &format!("job-{run_id}"),
                &format!("Job {run_id}"),
                0,
                &format!("revision-{run_id}"),
                run_id,
            ),
        );
    }
    {
        let store = repository.store.lock().unwrap();
        assert_eq!(store.retained_identities_by_id.len(), 6);
        assert_eq!(store.versions_by_id.len(), HOT_RESULT_CACHE_CAPACITY);
    }
    assert!(repository
        .get_exact(&identity("job-1", 0, "revision-1", 1))
        .unwrap()
        .is_some());
    assert_eq!(
        repository
            .store
            .lock()
            .unwrap()
            .retained_identities_by_id
            .len(),
        6
    );
}

#[test]
fn durable_forget_survives_restart_without_resurrecting_an_older_latest() {
    let directory = TestDirectory::new("forget");
    let older = identity("job-a", 0, "revision-a", 1);
    let latest = identity("job-a", 0, "revision-a", 2);
    {
        let repository = CompareResultRepository::open_at(directory.0.clone()).unwrap();
        publish(&repository, version("job-a", "A", 0, "revision-a", 1));
        publish(&repository, version("job-a", "A", 0, "revision-a", 2));
        assert!(matches!(
            repository.forget(&latest).unwrap(),
            CompareResultForgetOutcome::Forgotten {
                cleanup_warning: None
            }
        ));
    }

    let repository = CompareResultRepository::open_at(directory.0.clone()).unwrap();
    assert!(repository.get_exact(&older).unwrap().is_some());
    assert!(repository.get_exact(&latest).unwrap().is_none());
    assert!(matches!(
        repository
            .restore_workspace("job-a", 0, "revision-a")
            .unwrap(),
        CompareWorkspaceLookupDto::Missing { .. }
    ));
}

#[test]
fn corrupt_artifact_blocks_repository_startup() {
    use std::io::Write as _;

    let directory = TestDirectory::new("corrupt");
    let result = identity("job-a", 0, "revision-a", 1);
    {
        let repository = CompareResultRepository::open_at(directory.0.clone()).unwrap();
        publish(&repository, version("job-a", "A", 0, "revision-a", 1));
    }
    let artifact = directory
        .0
        .join("results")
        .join(format!("{}.jsonl", result.result_id));
    std::fs::OpenOptions::new()
        .append(true)
        .open(&artifact)
        .unwrap()
        .write_all(b"{}\n")
        .unwrap();

    assert!(matches!(
        CompareResultRepository::open_at(directory.0.clone()),
        Err(CompareResultRepositoryError::Storage(_))
    ));
}

/// The equality window a comparison used is part of its evidence, not a constant the window can
/// look up. `run` widens it for a backend whose timestamps are coarser than the policy floor — an
/// FTP LIST root reports whole minutes — and every surface that draws a "newer" or "drifted" cue
/// has to be told that number, or it will contradict the verdict the same result carries. It must
/// therefore survive the artifact round trip as faithfully as the operations do.
#[test]
fn the_effective_mtime_window_is_published_and_survives_a_restart() {
    let directory = TestDirectory::new("mtime-window");
    let widened = syncdash::pipeline::compare::CompareOptions {
        mtime_window_ms: 60_000,
        ..Default::default()
    };
    {
        let repository = CompareResultRepository::open_at(directory.0.clone()).unwrap();
        publish(
            &repository,
            version_with_compare_options("job-a", "A", 0, "revision-a", 1, widened),
        );
        let retained = repository
            .get_exact(&identity("job-a", 0, "revision-a", 1))
            .unwrap()
            .unwrap();
        assert_eq!(
            retained.plan().mtime_window_ms,
            60_000,
            "a live result publishes the window its comparison applied"
        );
    }

    let repository = CompareResultRepository::open_at(directory.0.clone()).unwrap();
    let restored = repository
        .restore_workspace("job-a", 0, "revision-a")
        .unwrap();
    let CompareWorkspaceLookupDto::Found { workspace } = restored else {
        panic!("the published result must restore");
    };
    assert_eq!(
        workspace.plan.mtime_window_ms, 60_000,
        "the widened window must not decay to the policy floor across a restart"
    );
}
