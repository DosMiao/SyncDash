//! The fail-closed reading of a ledger: who, if anyone, currently holds the lease.
//!
//! Every rule here refuses on doubt. Generations must be contiguous, a generation carries at most
//! one claim, a heartbeat or release counts only from the owner named in that generation's claim,
//! and a claim over an unreleased earlier generation is a malformed ledger rather than a takeover.
//! Anything unparseable stops the read instead of being skipped — a ledger this process cannot
//! fully understand is one whose lease it cannot prove is free.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use crate::foundation::names::LOCK_NAME;
use crate::fs::vfs::error::VfsResult;
use crate::fs::vfs::{Vfs, VfsEntryKind};

use super::artifact::{
    claim_name, invalid_lock, is_lower_hex, parse_artifact, release_name, LedgerArtifact,
    LockAnchor, LockClaim, LockHeartbeat, LockRelease, LOCK_PROTOCOL, TOKEN_HEX_LEN,
};
use super::record_store::{publish_record, read_anchor, read_record};

pub(super) struct LedgerState {
    pub(super) latest: Option<LockClaim>,
    pub(super) latest_released: bool,
}

pub(super) fn validate_claim(
    claim: &LockClaim,
    anchor: &LockAnchor,
    generation: u64,
) -> std::io::Result<()> {
    if claim.protocol != LOCK_PROTOCOL
        || claim.ledger_id != anchor.ledger_id
        || claim.generation != generation
        || !is_lower_hex(&claim.owner_id, TOKEN_HEX_LEN)
    {
        return Err(invalid_lock(format!(
            "claim {} does not match its ledger identity",
            claim_name(&anchor.ledger_id, generation)
        )));
    }
    Ok(())
}

pub(super) fn validate_heartbeat(
    heartbeat: &LockHeartbeat,
    claim: &LockClaim,
) -> std::io::Result<()> {
    if heartbeat.protocol != LOCK_PROTOCOL
        || heartbeat.ledger_id != claim.ledger_id
        || heartbeat.generation != claim.generation
        || heartbeat.owner_id != claim.owner_id
    {
        return Err(invalid_lock("heartbeat body does not match its claim"));
    }
    Ok(())
}

pub(super) fn validate_release(release: &LockRelease, claim: &LockClaim) -> std::io::Result<()> {
    if release.protocol != LOCK_PROTOCOL
        || release.ledger_id != claim.ledger_id
        || release.generation != claim.generation
        || release.owner_id != claim.owner_id
    {
        return Err(invalid_lock("release body does not match its claim"));
    }
    Ok(())
}

pub(super) fn inspect_ledger(
    vfs: &Arc<dyn Vfs>,
    anchor: &LockAnchor,
) -> std::io::Result<LedgerState> {
    let mut listed_names = HashSet::new();
    let mut claim_paths = BTreeMap::new();
    let mut heartbeats = Vec::new();
    let mut releases = Vec::new();

    for (name, kind) in vfs.read_dir_names("").map_err(std::io::Error::from)? {
        let artifact = parse_artifact(name.as_str(), &anchor.ledger_id)?;
        if artifact.is_none() && name.as_str().starts_with(&format!("{LOCK_NAME}.")) {
            return Err(invalid_lock(format!(
                "reserved lock artifact {:?} does not belong to active ledger {}",
                name.as_str(),
                anchor.ledger_id
            )));
        }
        let Some(artifact) = artifact else {
            continue;
        };
        if kind != VfsEntryKind::File {
            return Err(invalid_lock(format!(
                "lock-ledger artifact {:?} is not a regular file",
                name.as_str()
            )));
        }
        if !listed_names.insert(name.as_str().to_owned()) {
            return Err(invalid_lock(format!(
                "duplicate lock-ledger directory entry {:?}",
                name.as_str()
            )));
        }
        match artifact {
            LedgerArtifact::Claim { generation } => {
                if claim_paths
                    .insert(generation, name.as_str().to_owned())
                    .is_some()
                {
                    return Err(invalid_lock(format!(
                        "duplicate claim for lock generation {generation}"
                    )));
                }
            }
            LedgerArtifact::Heartbeat {
                generation,
                owner_id,
            } => heartbeats.push((generation, owner_id, name.as_str().to_owned())),
            LedgerArtifact::Release {
                generation,
                owner_id,
            } => releases.push((generation, owner_id, name.as_str().to_owned())),
        }
    }

    let mut claims = BTreeMap::new();
    for (&generation, path) in &claim_paths {
        let claim = read_record::<LockClaim>(vfs, path)?.ok_or_else(|| {
            invalid_lock(format!(
                "listed claim {path:?} disappeared during inspection"
            ))
        })?;
        validate_claim(&claim, anchor, generation)?;
        claims.insert(generation, claim);
    }
    for (expected, generation) in (0u64..).zip(claims.keys().copied()) {
        if generation != expected {
            return Err(invalid_lock(format!(
                "lock-ledger generation gap: expected {expected}, found {generation}"
            )));
        }
    }

    let mut heartbeat_generations = HashSet::new();
    for (generation, owner_id, path) in heartbeats {
        let claim = claims.get(&generation).ok_or_else(|| {
            invalid_lock(format!(
                "heartbeat {path:?} refers to a missing or future claim"
            ))
        })?;
        if owner_id != claim.owner_id || !heartbeat_generations.insert(generation) {
            return Err(invalid_lock(format!(
                "heartbeat {path:?} conflicts with its generation's owner"
            )));
        }
        let heartbeat = read_record::<LockHeartbeat>(vfs, &path)?.ok_or_else(|| {
            invalid_lock(format!(
                "listed heartbeat {path:?} disappeared during inspection"
            ))
        })?;
        validate_heartbeat(&heartbeat, claim)?;
    }

    let mut released_generations = HashSet::new();
    for (generation, owner_id, path) in releases {
        let claim = claims.get(&generation).ok_or_else(|| {
            invalid_lock(format!(
                "release {path:?} refers to a missing or future claim"
            ))
        })?;
        if owner_id != claim.owner_id || !released_generations.insert(generation) {
            return Err(invalid_lock(format!(
                "release {path:?} conflicts with its generation's owner"
            )));
        }
        let release = read_record::<LockRelease>(vfs, &path)?.ok_or_else(|| {
            invalid_lock(format!(
                "listed release {path:?} disappeared during inspection"
            ))
        })?;
        validate_release(&release, claim)?;
    }

    for generation in claims.keys().copied().rev().skip(1) {
        if !released_generations.contains(&generation) {
            return Err(invalid_lock(format!(
                "future lock claim follows unreleased generation {generation}"
            )));
        }
    }

    let latest = claims.last_key_value().map(|(_, claim)| claim.clone());
    let latest_released = latest
        .as_ref()
        .is_some_and(|claim| released_generations.contains(&claim.generation));
    Ok(LedgerState {
        latest,
        latest_released,
    })
}

pub(super) fn claim_is_current(vfs: &Arc<dyn Vfs>, expected: &LockClaim) -> bool {
    let path = claim_name(&expected.ledger_id, expected.generation);
    matches!(
        read_record::<LockClaim>(vfs, &path),
        Ok(Some(actual)) if actual == *expected
    )
}

pub(super) fn lease_identity_is_current(
    vfs: &Arc<dyn Vfs>,
    anchor: &LockAnchor,
    claim: &LockClaim,
) -> bool {
    matches!(read_anchor(vfs), Ok(Some(current)) if current == *anchor)
        && claim_is_current(vfs, claim)
}

pub(super) fn release_claim(vfs: &Arc<dyn Vfs>, claim: &LockClaim) -> VfsResult<()> {
    let path = release_name(&claim.ledger_id, claim.generation, &claim.owner_id);
    let release = LockRelease {
        protocol: LOCK_PROTOCOL,
        ledger_id: claim.ledger_id.clone(),
        generation: claim.generation,
        owner_id: claim.owner_id.clone(),
        released_ms: crate::foundation::time::now_ms(),
    };
    match publish_record(vfs, &path, &release) {
        Ok(()) => Ok(()),
        Err(publish_error) => match read_record::<LockRelease>(vfs, &path) {
            Ok(Some(existing)) if existing == release => Ok(()),
            _ => Err(publish_error),
        },
    }
}

pub(super) fn occupied_error(vfs: &Arc<dyn Vfs>, claim: &LockClaim) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::WouldBlock,
        format!(
            "{} is already locked by {} (pid {}, owner {}, generation {}); remove every {LOCK_NAME}* ledger artifact manually only after confirming all owners have stopped",
            vfs.display(), claim.host, claim.pid, claim.owner_id, claim.generation
        ),
    )
}
