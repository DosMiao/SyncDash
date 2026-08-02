//! Structural and checksum validation for the authoritative index.

use std::cmp::Ordering;
use std::collections::HashSet;

use super::artifact_validation::{validate_digest, validate_identity, validate_result_id};
use super::error::invalid_data;
use super::index_order::compare_latest_scope;
use super::integrity::{checksum, require_schema};
use super::schema::{IndexEnvelope, IndexState, LatestResult};
use super::INDEX_CHECKSUM_DOMAIN;

pub(super) fn validate_index_envelope(envelope: &IndexEnvelope) -> std::io::Result<()> {
    require_schema(envelope.schema, "Compare-result index")?;
    validate_digest(&envelope.checksum, "Compare-result index checksum")?;
    let expected = checksum(INDEX_CHECKSUM_DOMAIN, &envelope.state)?;
    if envelope.checksum != expected {
        return Err(invalid_data("the Compare-result index checksum is invalid"));
    }
    validate_index_state(&envelope.state)
}

pub(super) fn validate_index_state(state: &IndexState) -> std::io::Result<()> {
    if state.generation < state.last_publication_sequence {
        return Err(invalid_data(
            "Compare-result index generation precedes its publication sequence",
        ));
    }
    let mut publication_sequences = HashSet::with_capacity(state.results.len());
    let mut maximum_sequence = 0_u64;
    let mut referenced_jobs = HashSet::new();
    for (result_id, indexed) in &state.results {
        validate_result_id(result_id)?;
        validate_identity(&indexed.identity)?;
        if indexed.identity.result_id != *result_id {
            return Err(invalid_data(format!(
                "Compare index key '{result_id}' does not match its immutable identity"
            )));
        }
        if indexed.publication_sequence == 0
            || !publication_sequences.insert(indexed.publication_sequence)
        {
            return Err(invalid_data(
                "Compare-result publication sequences must be unique and non-zero",
            ));
        }
        maximum_sequence = maximum_sequence.max(indexed.publication_sequence);
        validate_digest(
            &indexed.artifact_checksum,
            "Compare-result artifact checksum",
        )?;
        referenced_jobs.insert(indexed.identity.job_id.clone());
    }
    if maximum_sequence > state.last_publication_sequence {
        return Err(invalid_data(
            "Compare-result index publication sequence precedes a retained record",
        ));
    }
    let presented_jobs = state.job_names.keys().cloned().collect::<HashSet<_>>();
    if referenced_jobs != presented_jobs
        || state.job_names.values().any(|name| name.trim().is_empty())
    {
        return Err(invalid_data(
            "Compare-result presentation names do not exactly cover retained job identities",
        ));
    }
    let mut previous: Option<&LatestResult> = None;
    let mut scopes = HashSet::with_capacity(state.latest_by_scope.len());
    for latest in &state.latest_by_scope {
        validate_result_id(&latest.result_id)?;
        if previous.is_some_and(|prior| compare_latest_scope(prior, latest) != Ordering::Less) {
            return Err(invalid_data(
                "Compare-result latest pointers are not uniquely ordered by scope",
            ));
        }
        previous = Some(latest);
        let scope = latest.scope();
        if !scopes.insert(scope.clone()) {
            return Err(invalid_data("duplicate Compare-result latest scope"));
        }
        let indexed = state.results.get(&latest.result_id).ok_or_else(|| {
            invalid_data(format!(
                "Compare latest pointer '{}' has no indexed artifact",
                latest.result_id
            ))
        })?;
        if !scope.contains(&indexed.identity) {
            return Err(invalid_data(format!(
                "Compare latest pointer '{}' crosses result scopes",
                latest.result_id
            )));
        }
        let newest_sequence = state
            .results
            .values()
            .filter(|candidate| scope.contains(&candidate.identity))
            .map(|candidate| candidate.publication_sequence)
            .max()
            .expect("a latest pointer references at least one result in its scope");
        if newest_sequence != indexed.publication_sequence {
            return Err(invalid_data(format!(
                "Compare latest pointer '{}' is not the newest retained publication for its scope",
                latest.result_id
            )));
        }
    }
    Ok(())
}
