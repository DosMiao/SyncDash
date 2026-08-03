//! Semantic validation for immutable Compare plans and snapshots.

use syncdash::model::plan::{Action, Plan, PLAN_SCHEMA};
use syncdash::model::table::{TableArtifact, TableKind, TABLE_SCHEMA};

use crate::contracts::compare::CompareIdentity;

use super::super::model::result::CompareResultVersion;
use super::error::invalid_data;
use super::{DIGEST_HEX_LENGTH, RESULT_ID_HEX_LENGTH};

pub(super) fn validate_compare_result(version: &CompareResultVersion) -> std::io::Result<()> {
    validate_identity(&version.identity)?;
    validate_digest(&version.plan_digest, "Compare plan digest")?;
    let plan_header = &version.plan.header;
    let operations = &version.plan.operations;
    if Plan::digest_parts(plan_header, operations) != version.plan_digest {
        return Err(invalid_data(format!(
            "Compare artifact '{}' plan digest is invalid",
            version.identity.result_id
        )));
    }
    if plan_header.schema != PLAN_SCHEMA || plan_header.kind != "plan" {
        return Err(invalid_data(
            "retained Compare plan has an unsupported schema or kind",
        ));
    }
    if plan_header.op_count != operations.len() as u64
        || plan_header.conflict_count
            != operations
                .iter()
                .filter(|operation| matches!(operation.action, Action::Conflict))
                .count() as u64
        || version.plan.metadata.len() != operations.len()
    {
        return Err(invalid_data(
            "retained Compare plan counts do not match its operations and metadata",
        ));
    }
    validate_snapshot("source", &version.source)?;
    validate_snapshot("target", &version.target)?;
    if plan_header.source_root != version.source.header.root
        || plan_header.source_host != version.source.header.host
        || plan_header.target_root != version.target.header.root
        || plan_header.target_host != version.target.header.host
        || plan_header.source_entries != version.source.entries.len() as u64
        || plan_header.target_entries != version.target.entries.len() as u64
        || plan_header.source_excluded
            != version.source.header.excluded_dirs + version.source.header.excluded_files
        || plan_header.target_excluded
            != version.target.header.excluded_dirs + version.target.header.excluded_files
        || plan_header.source_walk_errors != version.source.header.walk_errors
        || plan_header.target_walk_errors != version.target.header.walk_errors
        || plan_header.source_walk_err_samples != version.source.header.walk_err_samples
        || plan_header.target_walk_err_samples != version.target.header.walk_err_samples
        || plan_header.source_icloud_stubs != version.source.header.icloud_stubs
        || plan_header.target_icloud_stubs != version.target.header.icloud_stubs
        || plan_header.source_icloud_stub_samples != version.source.header.icloud_stub_samples
        || plan_header.target_icloud_stub_samples != version.target.header.icloud_stub_samples
    {
        return Err(invalid_data(
            "retained Compare plan header does not attest to its exact snapshots",
        ));
    }
    if version.compare_options.max_conflicts < -1 || version.compare_options.mtime_window_ms < 0 {
        return Err(invalid_data(
            "retained Compare options are outside their valid range",
        ));
    }
    let evidence = syncdash::pipeline::compare::evidence::evidence_for_operations(
        &version.source,
        &version.target,
        operations,
        &version.compare_options,
    );
    if evidence.identical_count != version.plan.identical_count
        || evidence.identical_bytes != version.plan.identical_bytes
    {
        return Err(invalid_data(
            "retained Compare identical-item totals do not match its snapshots",
        ));
    }
    for ((retained, derived), operation) in version
        .plan
        .metadata
        .iter()
        .zip(evidence.metas.iter())
        .zip(operations.iter())
    {
        match retained {
            Some(retained) if retained == derived => {}
            // An elided entry is only valid where the row rebuilds it exactly, which is the same
            // rule publication elided it by — one function, so the two cannot drift apart.
            None if syncdash::pipeline::compare::evidence::implied_row_meta(operation).as_ref()
                == Some(derived) => {}
            _ => {
                return Err(invalid_data(
                    "retained Compare row metadata does not match its snapshots",
                ))
            }
        }
    }
    Ok(())
}

fn validate_snapshot(side: &str, snapshot: &TableArtifact) -> std::io::Result<()> {
    if snapshot.header.schema != TABLE_SCHEMA || snapshot.header.kind != TableKind::Snapshot {
        return Err(invalid_data(format!(
            "retained {side} snapshot has an unsupported schema or kind"
        )));
    }
    if snapshot.header.entry_count != snapshot.entries.len() as u64 {
        return Err(invalid_data(format!(
            "retained {side} snapshot entry count is invalid"
        )));
    }
    snapshot
        .validate()
        .map_err(|error| invalid_data(format!("retained {side} snapshot is invalid: {error}")))
}

pub(super) fn validate_identity(identity: &CompareIdentity) -> std::io::Result<()> {
    validate_result_id(&identity.result_id)?;
    if identity.job_id.trim().is_empty() || identity.config_revision.trim().is_empty() {
        return Err(invalid_data(
            "Compare identity has an empty job ID or configuration revision",
        ));
    }
    Ok(())
}

pub(super) fn validate_result_id(result_id: &str) -> std::io::Result<()> {
    if result_id.len() != RESULT_ID_HEX_LENGTH
        || !result_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(invalid_data(format!(
            "Compare result ID '{result_id}' is not {RESULT_ID_HEX_LENGTH} lowercase hexadecimal characters"
        )));
    }
    Ok(())
}

pub(super) fn validate_digest(digest: &str, label: &str) -> std::io::Result<()> {
    if !syncdash::model::digest::is_blake3_hex(digest) {
        return Err(invalid_data(format!(
            "{label} is not {DIGEST_HEX_LENGTH} lowercase hexadecimal characters"
        )));
    }
    Ok(())
}
