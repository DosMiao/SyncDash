//! Generation, schema, and checksum guards shared by index and artifact storage.

use serde::Serialize;

use super::super::model::result::CompareResultVersion;
use super::artifact_codec::write_result_body;
use super::error::invalid_data;
use super::index_validation::validate_index_state;
use super::schema::{IndexEnvelope, IndexState};
use super::{INDEX_CHECKSUM_DOMAIN, STORE_SCHEMA};

pub(super) fn require_generation(state: &IndexState, expected: u64) -> std::io::Result<()> {
    if state.generation != expected {
        return Err(std::io::Error::other(format!(
            "Compare-result repository changed in another process (expected generation {expected}, found {}) — restart SyncDash before changing retained results",
            state.generation
        )));
    }
    Ok(())
}

pub(super) fn require_schema(actual: u32, artifact: &str) -> std::io::Result<()> {
    if actual != STORE_SCHEMA {
        return Err(invalid_data(format!(
            "{artifact} uses schema {actual}; this build requires schema {STORE_SCHEMA}"
        )));
    }
    Ok(())
}

pub(super) fn index_envelope(state: IndexState) -> std::io::Result<IndexEnvelope> {
    validate_index_state(&state)?;
    Ok(IndexEnvelope {
        schema: STORE_SCHEMA,
        checksum: checksum(INDEX_CHECKSUM_DOMAIN, &state)?,
        state,
    })
}

pub(super) fn checksum<T: Serialize>(domain: &[u8], value: &T) -> std::io::Result<String> {
    let bytes = serde_json::to_vec(value).map_err(|error| {
        invalid_data(format!(
            "cannot encode Compare-result checksum input: {error}"
        ))
    })?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(domain);
    hasher.update(&bytes);
    Ok(hasher.finalize().to_hex().to_string())
}

pub(super) fn calculate_result_checksum(version: &CompareResultVersion) -> std::io::Result<String> {
    write_result_body(&mut std::io::sink(), version)
}
