//! The lock ledger's on-disk format: its record bodies and its file-name grammar.
//!
//! This is a **user-visible format**, not an implementation detail. When a root is occupied the
//! error names these files and tells the user to remove them by hand, so the grammar is part of
//! the product's surface — renaming a component here breaks instructions people have already
//! followed. `protocol` is versioned for the same reason.
//!
//! Pure: no `Vfs`, no threads, no I/O. Everything here can be checked without acquiring anything.

use serde::{Deserialize, Serialize};

use crate::foundation::names::LOCK_NAME;

pub(super) const LOCK_PROTOCOL: u32 = 1;
pub(super) const TOKEN_HEX_LEN: usize = 32;
pub(super) const GENERATION_HEX_LEN: usize = 16;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct LockAnchor {
    pub(super) protocol: u32,
    pub(super) ledger_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct LockClaim {
    pub(super) protocol: u32,
    pub(super) ledger_id: String,
    pub(super) generation: u64,
    pub(super) owner_id: String,
    pub(super) host: String,
    pub(super) pid: u32,
    pub(super) started_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct LockHeartbeat {
    pub(super) protocol: u32,
    pub(super) ledger_id: String,
    pub(super) generation: u64,
    pub(super) owner_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct LockRelease {
    pub(super) protocol: u32,
    pub(super) ledger_id: String,
    pub(super) generation: u64,
    pub(super) owner_id: String,
    pub(super) released_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum LedgerArtifact {
    Claim { generation: u64 },
    Heartbeat { generation: u64, owner_id: String },
    Release { generation: u64, owner_id: String },
}

pub(super) fn invalid_lock(message: impl Into<String>) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message.into())
}

pub(super) fn is_lower_hex(value: &str, expected_len: usize) -> bool {
    value.len() == expected_len
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub(super) fn validate_anchor(anchor: &LockAnchor) -> std::io::Result<()> {
    if anchor.protocol != LOCK_PROTOCOL {
        return Err(invalid_lock(format!(
            "{LOCK_NAME} uses unsupported lock protocol {} (expected {LOCK_PROTOCOL})",
            anchor.protocol
        )));
    }
    if !is_lower_hex(&anchor.ledger_id, TOKEN_HEX_LEN) {
        return Err(invalid_lock(format!(
            "{LOCK_NAME} has an invalid ledger id"
        )));
    }
    Ok(())
}

pub(super) fn claim_name(ledger_id: &str, generation: u64) -> String {
    format!("{LOCK_NAME}.{ledger_id}.claim.{generation:016x}")
}

pub(super) fn heartbeat_name(ledger_id: &str, generation: u64, owner_id: &str) -> String {
    format!("{LOCK_NAME}.{ledger_id}.heartbeat.{generation:016x}.{owner_id}")
}

pub(super) fn release_name(ledger_id: &str, generation: u64, owner_id: &str) -> String {
    format!("{LOCK_NAME}.{ledger_id}.release.{generation:016x}.{owner_id}")
}

pub(super) fn parse_generation(value: &str) -> std::io::Result<u64> {
    if !is_lower_hex(value, GENERATION_HEX_LEN) {
        return Err(invalid_lock(format!(
            "invalid lock generation token {value:?}"
        )));
    }
    u64::from_str_radix(value, 16)
        .map_err(|error| invalid_lock(format!("invalid lock generation {value:?}: {error}")))
}

pub(super) fn parse_artifact(
    name: &str,
    ledger_id: &str,
) -> std::io::Result<Option<LedgerArtifact>> {
    let prefix = format!("{LOCK_NAME}.{ledger_id}.");
    let Some(rest) = name.strip_prefix(&prefix) else {
        return Ok(None);
    };
    let parts = rest.split('.').collect::<Vec<_>>();
    let artifact = match parts.as_slice() {
        ["claim", generation] => LedgerArtifact::Claim {
            generation: parse_generation(generation)?,
        },
        ["heartbeat", generation, owner_id] if is_lower_hex(owner_id, TOKEN_HEX_LEN) => {
            LedgerArtifact::Heartbeat {
                generation: parse_generation(generation)?,
                owner_id: (*owner_id).to_owned(),
            }
        }
        ["release", generation, owner_id] if is_lower_hex(owner_id, TOKEN_HEX_LEN) => {
            LedgerArtifact::Release {
                generation: parse_generation(generation)?,
                owner_id: (*owner_id).to_owned(),
            }
        }
        _ => {
            return Err(invalid_lock(format!(
                "malformed lock-ledger artifact {name:?}"
            )));
        }
    };
    Ok(Some(artifact))
}
