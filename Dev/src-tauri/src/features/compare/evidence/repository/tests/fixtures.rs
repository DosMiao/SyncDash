//! Shared builders for the repository suites: identities, owners, versions, and publication.

use crate::contracts::compare::{CompareIdentity, CompareOwner, PlanDto};

use super::super::super::model::result::*;
use super::super::super::model::scope::*;
use super::super::*;
use syncdash::model::plan::PlanHeader;
use syncdash::model::table::{TableArtifact, TableEvidence, TableHeader, TableKind, TABLE_SCHEMA};
pub(super) fn identity(
    job_id: &str,
    target_index: usize,
    revision: &str,
    compare_run_id: u64,
) -> CompareIdentity {
    let result_digest =
        blake3::hash(format!("{job_id}\0{target_index}\0{revision}\0{compare_run_id}").as_bytes())
            .to_hex()
            .to_string();
    CompareIdentity {
        result_id: result_digest[..32].to_string(),
        compare_run_id,
        job_id: job_id.into(),
        target_index,
        config_revision: revision.into(),
    }
}

pub(super) fn owner(
    job_id: &str,
    job_name: &str,
    target_index: usize,
    revision: &str,
    compare_run_id: u64,
) -> CompareOwner {
    CompareOwner {
        identity: identity(job_id, target_index, revision, compare_run_id),
        job_name: job_name.into(),
    }
}

pub(super) fn version(
    job_id: &str,
    job_name: &str,
    target_index: usize,
    revision: &str,
    compare_run_id: u64,
) -> SuccessfulCompareResult {
    version_with_compare_options(
        job_id,
        job_name,
        target_index,
        revision,
        compare_run_id,
        syncdash::pipeline::compare::CompareOptions::default(),
    )
}

/// The comparison policy a run ended up with is retained evidence: `run` widens the mtime equality
/// window for coarse backends, and the suites that assert what a restored result publishes need to
/// build a result whose window is not the policy default.
pub(super) fn version_with_compare_options(
    job_id: &str,
    job_name: &str,
    target_index: usize,
    revision: &str,
    compare_run_id: u64,
    compare_options: syncdash::pipeline::compare::CompareOptions,
) -> SuccessfulCompareResult {
    let owner = owner(job_id, job_name, target_index, revision, compare_run_id);
    let plan_header = PlanHeader {
        schema: syncdash::model::plan::PLAN_SCHEMA,
        kind: "plan".into(),
        mode: "mirror".into(),
        generated_at_ms: 0,
        source_root: "/source".into(),
        source_host: "host".into(),
        target_root: "/target".into(),
        target_host: "host".into(),
        op_count: 0,
        conflict_count: 0,
        source_entries: 0,
        target_entries: 0,
        source_excluded: 0,
        target_excluded: 0,
        source_walk_errors: 0,
        target_walk_errors: 0,
        source_walk_err_samples: Vec::new(),
        target_walk_err_samples: Vec::new(),
        source_icloud_stubs: 0,
        target_icloud_stubs: 0,
        source_icloud_stub_samples: Vec::new(),
        target_icloud_stub_samples: Vec::new(),
        source_unread_paths: Vec::new(),
        target_unread_paths: Vec::new(),
        source_unread_entries: 0,
        target_unread_entries: 0,
    };
    let plan_digest = syncdash::model::plan::Plan::digest_parts(&plan_header, &[]);
    let snapshot = |root: &str| TableArtifact {
        header: TableHeader {
            schema: TABLE_SCHEMA,
            kind: TableKind::Snapshot,
            root: root.into(),
            host: "host".into(),
            os: "test".into(),
            scanned_at_ms: 0,
            duration_ms: 0,
            entry_count: 0,
            evidence: TableEvidence::None,
            excluded_dirs: 0,
            excluded_files: 0,
            walk_errors: 0,
            walk_err_samples: Vec::new(),
            unread_paths: Vec::new(),
            icloud_stubs: 0,
            icloud_stub_samples: Vec::new(),
            skipped_symlinks: 0,
            dataless_files: 0,
            vfs: None,
        },
        entries: Vec::new(),
    };
    SuccessfulCompareResult::from_plan(
        plan_digest,
        PlanDto {
            header: plan_header,
            ops: Vec::new(),
            metas: Vec::new(),
            identical_count: 0,
            identical_bytes: 0,
            mtime_window_ms: compare_options.mtime_window_ms,
            owner,
        },
        snapshot("/source"),
        snapshot("/target"),
        compare_options,
    )
}

pub(super) fn publish(repository: &CompareResultRepository, version: SuccessfulCompareResult) {
    let scope = CompareScope::from_identity(&version.owner.identity);
    let compare_run_id = version.owner.identity.compare_run_id;
    let verification = repository
        .begin_verification(scope, Some(compare_run_id))
        .unwrap();
    repository
        .publish_successful_version(&verification, version)
        .unwrap();
}
