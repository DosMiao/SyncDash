//! Exclusive lease for a sync root.
//!
//! `.syncdash.lock` is an immutable random ledger anchor. Each acquisition contends on one
//! generation-specific claim name with an atomic no-replace commit; release is a separate,
//! owner-specific immutable record. No guard updates or deletes a shared claim, so an old guard
//! cannot touch or release a later owner's lease.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::Arc;

use crate::foundation::names::LOCK_NAME;
use crate::fs::vfs::error::{VfsError, VfsErrorKind, VfsResult};
use crate::fs::vfs::VfsEntryKind;
use crate::fs::vfs::{Vfs, WriteHint};

#[cfg(not(test))]
const HEARTBEAT_MS: u64 = 4000;
#[cfg(test)]
const HEARTBEAT_MS: u64 = 40;
#[cfg(not(test))]
const HEARTBEAT_POLL_MS: u64 = 250;
#[cfg(test)]
const HEARTBEAT_POLL_MS: u64 = 10;
const LOCK_PROTOCOL: u32 = 1;
const TOKEN_HEX_LEN: usize = 32;
const GENERATION_HEX_LEN: usize = 16;
const MAX_LOCK_BYTES: u64 = 16 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct LockAnchor {
    protocol: u32,
    ledger_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct LockClaim {
    protocol: u32,
    ledger_id: String,
    generation: u64,
    owner_id: String,
    host: String,
    pid: u32,
    started_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct LockHeartbeat {
    protocol: u32,
    ledger_id: String,
    generation: u64,
    owner_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct LockRelease {
    protocol: u32,
    ledger_id: String,
    generation: u64,
    owner_id: String,
    released_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum LedgerArtifact {
    Claim { generation: u64 },
    Heartbeat { generation: u64, owner_id: String },
    Release { generation: u64, owner_id: String },
}

struct LedgerState {
    latest: Option<LockClaim>,
    latest_released: bool,
}

pub struct RootLock {
    vfs: Arc<dyn Vfs>,
    anchor: LockAnchor,
    claim: LockClaim,
    heartbeat_stop: Option<Sender<()>>,
    lease_lost: Arc<AtomicBool>,
    heartbeat: Option<std::thread::JoinHandle<()>>,
}

fn invalid_lock(message: impl Into<String>) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message.into())
}

fn is_lower_hex(value: &str, expected_len: usize) -> bool {
    value.len() == expected_len
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn validate_anchor(anchor: &LockAnchor) -> std::io::Result<()> {
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

fn claim_name(ledger_id: &str, generation: u64) -> String {
    format!("{LOCK_NAME}.{ledger_id}.claim.{generation:016x}")
}

fn heartbeat_name(ledger_id: &str, generation: u64, owner_id: &str) -> String {
    format!("{LOCK_NAME}.{ledger_id}.heartbeat.{generation:016x}.{owner_id}")
}

fn release_name(ledger_id: &str, generation: u64, owner_id: &str) -> String {
    format!("{LOCK_NAME}.{ledger_id}.release.{generation:016x}.{owner_id}")
}

fn parse_generation(value: &str) -> std::io::Result<u64> {
    if !is_lower_hex(value, GENERATION_HEX_LEN) {
        return Err(invalid_lock(format!(
            "invalid lock generation token {value:?}"
        )));
    }
    u64::from_str_radix(value, 16)
        .map_err(|error| invalid_lock(format!("invalid lock generation {value:?}: {error}")))
}

fn parse_artifact(name: &str, ledger_id: &str) -> std::io::Result<Option<LedgerArtifact>> {
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

fn read_record<T: DeserializeOwned>(vfs: &Arc<dyn Vfs>, path: &str) -> std::io::Result<Option<T>> {
    let reader = match vfs.open_read(path) {
        Ok(reader) => reader,
        Err(error) if error.kind == VfsErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let mut bytes = Vec::new();
    reader.take(MAX_LOCK_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_LOCK_BYTES {
        return Err(invalid_lock(format!(
            "{path} exceeds the {MAX_LOCK_BYTES}-byte lock-record limit"
        )));
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|error| invalid_lock(format!("cannot decode lock record {path:?}: {error}")))
}

fn publish_record<T: Serialize>(vfs: &Arc<dyn Vfs>, path: &str, value: &T) -> VfsResult<()> {
    let body = serde_json::to_vec(value).map_err(|error| {
        VfsError::new(
            VfsErrorKind::Io,
            format!("could not encode lock record {path:?}: {error}"),
        )
    })?;
    let mut staged = vfs.open_write(path, &WriteHint::default())?;
    staged.write(&body)?;
    staged.seal(false)?;
    staged.commit_noreplace()?;
    Ok(())
}

fn read_anchor(vfs: &Arc<dyn Vfs>) -> std::io::Result<Option<LockAnchor>> {
    let anchor = read_record::<LockAnchor>(vfs, LOCK_NAME)?;
    if let Some(anchor) = &anchor {
        validate_anchor(anchor)?;
    }
    Ok(anchor)
}

fn ensure_anchor(vfs: &Arc<dyn Vfs>) -> std::io::Result<LockAnchor> {
    if let Some(anchor) = read_anchor(vfs)? {
        return Ok(anchor);
    }
    let has_orphaned_ledger = vfs
        .read_dir_names("")
        .map_err(std::io::Error::from)?
        .into_iter()
        .any(|(name, _)| name.as_str().starts_with(&format!("{LOCK_NAME}.")));
    if has_orphaned_ledger {
        return Err(invalid_lock(format!(
            "{LOCK_NAME} is missing while lock-ledger artifacts remain; refuse to create a new ledger until every owner has stopped and the complete {LOCK_NAME}* namespace is removed"
        )));
    }
    if let Some(anchor) = read_anchor(vfs)? {
        return Ok(anchor);
    }
    let candidate = LockAnchor {
        protocol: LOCK_PROTOCOL,
        ledger_id: super::vfs::random_name_token().map_err(std::io::Error::from)?,
    };
    match publish_record(vfs, LOCK_NAME, &candidate) {
        Ok(()) => Ok(candidate),
        Err(publish_error) => match read_anchor(vfs)? {
            Some(winner) => Ok(winner),
            None => Err(publish_error.into()),
        },
    }
}

fn validate_claim(claim: &LockClaim, anchor: &LockAnchor, generation: u64) -> std::io::Result<()> {
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

fn validate_heartbeat(heartbeat: &LockHeartbeat, claim: &LockClaim) -> std::io::Result<()> {
    if heartbeat.protocol != LOCK_PROTOCOL
        || heartbeat.ledger_id != claim.ledger_id
        || heartbeat.generation != claim.generation
        || heartbeat.owner_id != claim.owner_id
    {
        return Err(invalid_lock("heartbeat body does not match its claim"));
    }
    Ok(())
}

fn validate_release(release: &LockRelease, claim: &LockClaim) -> std::io::Result<()> {
    if release.protocol != LOCK_PROTOCOL
        || release.ledger_id != claim.ledger_id
        || release.generation != claim.generation
        || release.owner_id != claim.owner_id
    {
        return Err(invalid_lock("release body does not match its claim"));
    }
    Ok(())
}

fn inspect_ledger(vfs: &Arc<dyn Vfs>, anchor: &LockAnchor) -> std::io::Result<LedgerState> {
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

fn claim_is_current(vfs: &Arc<dyn Vfs>, expected: &LockClaim) -> bool {
    let path = claim_name(&expected.ledger_id, expected.generation);
    matches!(
        read_record::<LockClaim>(vfs, &path),
        Ok(Some(actual)) if actual == *expected
    )
}

fn lease_identity_is_current(vfs: &Arc<dyn Vfs>, anchor: &LockAnchor, claim: &LockClaim) -> bool {
    matches!(read_anchor(vfs), Ok(Some(current)) if current == *anchor)
        && claim_is_current(vfs, claim)
}

fn release_claim(vfs: &Arc<dyn Vfs>, claim: &LockClaim) -> VfsResult<()> {
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

fn occupied_error(vfs: &Arc<dyn Vfs>, claim: &LockClaim) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::WouldBlock,
        format!(
            "{} is already locked by {} (pid {}, owner {}, generation {}); remove every {LOCK_NAME}* ledger artifact manually only after confirming all owners have stopped",
            vfs.display(), claim.host, claim.pid, claim.owner_id, claim.generation
        ),
    )
}

impl RootLock {
    pub fn acquire_vfs(vfs: &Arc<dyn Vfs>) -> std::io::Result<Self> {
        let anchor = ensure_anchor(vfs)?;
        let claim = loop {
            let state = inspect_ledger(vfs, &anchor)?;
            let generation = match state.latest {
                Some(ref latest) if !state.latest_released => {
                    return Err(occupied_error(vfs, latest));
                }
                Some(ref latest) => latest.generation.checked_add(1).ok_or_else(|| {
                    invalid_lock(
                        "lock generation exhausted; start a new ledger after all owners stop",
                    )
                })?,
                None => 0,
            };
            let candidate = LockClaim {
                protocol: LOCK_PROTOCOL,
                ledger_id: anchor.ledger_id.clone(),
                generation,
                owner_id: super::vfs::random_name_token().map_err(std::io::Error::from)?,
                host: crate::foundation::machine::host_name(),
                pid: std::process::id(),
                started_ms: crate::foundation::time::now_ms(),
            };
            let path = claim_name(&anchor.ledger_id, generation);
            match publish_record(vfs, &path, &candidate) {
                Ok(()) => break candidate,
                Err(publish_error) => {
                    let after = inspect_ledger(vfs, &anchor)?;
                    match after.latest {
                        // A backend may report a durability/transport error after the atomic
                        // namespace operation already succeeded. The immutable body identifies
                        // our own claim exactly, so continue as its owner instead of orphaning a
                        // live generation and reporting ourselves as the contender.
                        Some(ref winner) if winner == &candidate && !after.latest_released => {
                            break candidate;
                        }
                        Some(ref winner) if !after.latest_released => {
                            return Err(occupied_error(vfs, winner));
                        }
                        Some(ref winner) if winner.generation >= generation => continue,
                        _ => return Err(publish_error.into()),
                    }
                }
            }
        };

        let heartbeat_path = heartbeat_name(&claim.ledger_id, claim.generation, &claim.owner_id);
        let heartbeat_record = LockHeartbeat {
            protocol: LOCK_PROTOCOL,
            ledger_id: claim.ledger_id.clone(),
            generation: claim.generation,
            owner_id: claim.owner_id.clone(),
        };
        if let Err(error) = publish_record(vfs, &heartbeat_path, &heartbeat_record) {
            let _ = release_claim(vfs, &claim);
            return Err(error.into());
        }
        match read_anchor(vfs)? {
            Some(current) if current == anchor && claim_is_current(vfs, &claim) => {}
            _ => {
                let _ = release_claim(vfs, &claim);
                return Err(invalid_lock(
                    "root lock anchor or claim changed during acquisition; refusing to start apply",
                ));
            }
        }

        let (heartbeat_stop, heartbeat_wake) = mpsc::channel();
        let lease_lost = Arc::new(AtomicBool::new(false));
        let heartbeat_lost = lease_lost.clone();
        let heartbeat_vfs = vfs.clone();
        let heartbeat_anchor = anchor.clone();
        let heartbeat_claim = claim.clone();
        let heartbeat = std::thread::spawn(move || {
            let mut elapsed = 0u64;
            let mut touch_warning_emitted = false;
            loop {
                match heartbeat_wake
                    .recv_timeout(std::time::Duration::from_millis(HEARTBEAT_POLL_MS))
                {
                    Ok(()) | Err(RecvTimeoutError::Disconnected) => break,
                    Err(RecvTimeoutError::Timeout) => {}
                }
                if !lease_identity_is_current(&heartbeat_vfs, &heartbeat_anchor, &heartbeat_claim) {
                    heartbeat_lost.store(true, Ordering::Release);
                    break;
                }
                elapsed += HEARTBEAT_POLL_MS;
                if elapsed < HEARTBEAT_MS {
                    continue;
                }
                elapsed = 0;
                let path = heartbeat_name(
                    &heartbeat_claim.ledger_id,
                    heartbeat_claim.generation,
                    &heartbeat_claim.owner_id,
                );
                if let Err(error) =
                    heartbeat_vfs.set_mtime(&path, crate::foundation::time::now_ms() as i64)
                {
                    if error.kind != VfsErrorKind::Unsupported && !touch_warning_emitted {
                        crate::log_warn!(
                            "root-lock",
                            "root lock remains owned, but its observational heartbeat timestamp could not be refreshed on {}: {}",
                            heartbeat_vfs.display(),
                            error
                        );
                        touch_warning_emitted = true;
                    }
                }
            }
        });

        Ok(Self {
            vfs: vfs.clone(),
            anchor,
            claim,
            heartbeat_stop: Some(heartbeat_stop),
            lease_lost,
            heartbeat: Some(heartbeat),
        })
    }

    pub fn lease_lost(&self) -> bool {
        self.lease_lost.load(Ordering::Acquire)
    }

    pub(crate) fn lease_loss_signal(&self) -> Arc<AtomicBool> {
        self.lease_lost.clone()
    }

    pub(crate) fn verify_lease_identity(&self) -> bool {
        let current = lease_identity_is_current(&self.vfs, &self.anchor, &self.claim);
        if !current {
            self.lease_lost.store(true, Ordering::Release);
        }
        current
    }
}

impl Drop for RootLock {
    fn drop(&mut self) {
        if let Some(stop) = self.heartbeat_stop.take() {
            let _ = stop.send(());
        }
        if let Some(heartbeat) = self.heartbeat.take() {
            let _ = heartbeat.join();
        }
        if let Err(error) = release_claim(&self.vfs, &self.claim) {
            crate::log_warn!(
                "root-lock",
                "could not publish release for root lock generation {} on {}: {}",
                self.claim.generation,
                self.vfs.display(),
                error
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::vfs::memory::MemVfs;

    fn memory_root(id: &str) -> Arc<dyn Vfs> {
        Arc::new(MemVfs::new(id))
    }

    fn wait_until_lost(guard: &RootLock) {
        for _ in 0..50 {
            if guard.lease_lost() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("heartbeat did not observe ownership loss");
    }

    fn contend(vfs: &Arc<dyn Vfs>, contenders: usize) -> Vec<Option<u64>> {
        let start = Arc::new(std::sync::Barrier::new(contenders + 1));
        let release = Arc::new(std::sync::Barrier::new(contenders + 1));
        let (send, receive) = std::sync::mpsc::channel();
        let mut threads = Vec::new();
        for _ in 0..contenders {
            let vfs = vfs.clone();
            let start = start.clone();
            let release = release.clone();
            let send = send.clone();
            threads.push(std::thread::spawn(move || {
                start.wait();
                let guard = RootLock::acquire_vfs(&vfs).ok();
                send.send(guard.as_ref().map(|guard| guard.claim.generation))
                    .unwrap();
                release.wait();
                drop(guard);
            }));
        }
        drop(send);
        start.wait();
        let outcomes = receive.iter().take(contenders).collect::<Vec<_>>();
        release.wait();
        for thread in threads {
            thread.join().unwrap();
        }
        outcomes
    }

    #[test]
    fn concurrent_contenders_publish_exactly_one_initial_claim() {
        let vfs = memory_root("lock-race");
        let outcomes = contend(&vfs, 12);
        assert_eq!(outcomes.iter().filter(|result| result.is_some()).count(), 1);
        assert_eq!(outcomes.into_iter().flatten().next(), Some(0));
    }

    #[test]
    fn post_publication_error_does_not_orphan_our_exact_claim() {
        let memory = Arc::new(MemVfs::new("indeterminate-claim-publish"));
        memory.set_noreplace_post_publish_error(|path| {
            path.contains(".claim.").then_some(VfsErrorKind::Transient)
        });
        let vfs: Arc<dyn Vfs> = memory;

        let first = RootLock::acquire_vfs(&vfs).expect(
            "the exact immutable claim proves ownership after an indeterminate publish result",
        );
        assert_eq!(first.claim.generation, 0);
        drop(first);

        let second = RootLock::acquire_vfs(&vfs)
            .expect("the recovered claim must still release and permit the next generation");
        assert_eq!(second.claim.generation, 1);
        drop(second);
    }

    #[test]
    fn concurrent_contenders_publish_exactly_one_next_generation() {
        let vfs = memory_root("next-generation-race");
        let first = RootLock::acquire_vfs(&vfs).unwrap();
        assert_eq!(first.claim.generation, 0);
        drop(first);

        let outcomes = contend(&vfs, 12);
        assert_eq!(outcomes.iter().filter(|result| result.is_some()).count(), 1);
        assert_eq!(outcomes.into_iter().flatten().next(), Some(1));
    }

    #[test]
    fn an_unreleased_claim_is_never_treated_as_stale() {
        let vfs = memory_root("no-stale-takeover");
        let guard = RootLock::acquire_vfs(&vfs).unwrap();
        let error = match RootLock::acquire_vfs(&vfs) {
            Ok(_) => panic!("an unreleased claim must block a second owner"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
        assert!(vfs
            .stat(&claim_name(&guard.claim.ledger_id, guard.claim.generation))
            .unwrap()
            .is_some());
        drop(guard);
    }

    #[test]
    fn acquire_waiting_on_release_advances_exactly_one_generation() {
        let vfs = memory_root("release-acquire-race");
        let first = RootLock::acquire_vfs(&vfs).unwrap();
        let contender_vfs = vfs.clone();
        let contender = std::thread::spawn(move || loop {
            match RootLock::acquire_vfs(&contender_vfs) {
                Ok(guard) => break guard,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::yield_now();
                }
                Err(error) => panic!("unexpected acquisition failure: {error}"),
            }
        });
        std::thread::yield_now();
        drop(first);
        let second = contender.join().unwrap();
        assert_eq!(second.claim.generation, 1);
        drop(second);
    }

    #[test]
    fn old_guard_cannot_release_an_adversarial_replacement_claim() {
        let vfs = memory_root("owner-safe-release");
        let guard = RootLock::acquire_vfs(&vfs).unwrap();
        let claim_path = claim_name(&guard.claim.ledger_id, guard.claim.generation);
        let old_heartbeat = heartbeat_name(
            &guard.claim.ledger_id,
            guard.claim.generation,
            &guard.claim.owner_id,
        );
        vfs.remove_file(&old_heartbeat).unwrap();
        vfs.remove_file(&claim_path).unwrap();

        let mut replacement = guard.claim.clone();
        replacement.owner_id = super::super::vfs::random_name_token().unwrap();
        publish_record(&vfs, &claim_path, &replacement).unwrap();
        let replacement_heartbeat = LockHeartbeat {
            protocol: LOCK_PROTOCOL,
            ledger_id: replacement.ledger_id.clone(),
            generation: replacement.generation,
            owner_id: replacement.owner_id.clone(),
        };
        publish_record(
            &vfs,
            &heartbeat_name(
                &replacement.ledger_id,
                replacement.generation,
                &replacement.owner_id,
            ),
            &replacement_heartbeat,
        )
        .unwrap();

        wait_until_lost(&guard);
        drop(guard);

        let replacement_release = release_name(
            &replacement.ledger_id,
            replacement.generation,
            &replacement.owner_id,
        );
        assert!(read_record::<LockRelease>(&vfs, &replacement_release)
            .unwrap()
            .is_none());
        assert_eq!(
            read_record::<LockClaim>(&vfs, &claim_path)
                .unwrap()
                .unwrap(),
            replacement
        );
        assert!(RootLock::acquire_vfs(&vfs).is_err());
    }

    #[test]
    fn missing_anchor_with_live_ledger_fails_closed_until_complete_cleanup() {
        let vfs = memory_root("anchor-reset");
        let old = RootLock::acquire_vfs(&vfs).unwrap();
        let old_ledger = old.claim.ledger_id.clone();
        vfs.remove_file(LOCK_NAME).unwrap();

        let error = RootLock::acquire_vfs(&vfs).err().unwrap();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        drop(old);

        let artifacts = vfs.read_dir_names("").unwrap();
        for (name, kind) in artifacts {
            if kind == VfsEntryKind::File && name.as_str().starts_with(LOCK_NAME) {
                vfs.remove_file(name.as_str()).unwrap();
            }
        }
        let current = RootLock::acquire_vfs(&vfs).unwrap();
        assert_ne!(current.claim.ledger_id, old_ledger);
        drop(current);
    }

    #[test]
    fn unsupported_observational_mtime_does_not_lose_the_lease() {
        let vfs: Arc<dyn Vfs> = Arc::new(MemVfs::new("no-heartbeat-mtime").without(|caps| {
            caps.set_mtime = crate::fs::vfs::Support::No;
        }));
        let guard = RootLock::acquire_vfs(&vfs).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(HEARTBEAT_MS * 3));
        assert!(!guard.lease_lost());
        let error = RootLock::acquire_vfs(&vfs).err().unwrap();
        assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
        drop(guard);
    }

    #[test]
    fn transient_observational_mtime_failure_does_not_lose_a_current_lease() {
        let vfs: Arc<dyn Vfs> = Arc::new(
            MemVfs::new("transient-heartbeat-mtime").failing_set_mtime(VfsErrorKind::Transient),
        );
        let guard = RootLock::acquire_vfs(&vfs).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(HEARTBEAT_MS * 3));
        assert!(!guard.lease_lost());
        let error = RootLock::acquire_vfs(&vfs).err().unwrap();
        assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
        drop(guard);
    }

    #[test]
    fn malformed_future_artifact_fails_closed() {
        let vfs = memory_root("future-artifact");
        let guard = RootLock::acquire_vfs(&vfs).unwrap();
        let bogus = LockRelease {
            protocol: LOCK_PROTOCOL,
            ledger_id: guard.claim.ledger_id.clone(),
            generation: 9,
            owner_id: super::super::vfs::random_name_token().unwrap(),
            released_ms: 1,
        };
        let path = release_name(&bogus.ledger_id, bogus.generation, &bogus.owner_id);
        publish_record(&vfs, &path, &bogus).unwrap();

        let error = RootLock::acquire_vfs(&vfs).err().unwrap();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        drop(guard);
    }

    #[test]
    fn generation_gap_fails_closed() {
        let vfs = memory_root("generation-gap");
        let first = RootLock::acquire_vfs(&vfs).unwrap();
        let anchor = first.anchor.clone();
        drop(first);
        let gap = LockClaim {
            protocol: LOCK_PROTOCOL,
            ledger_id: anchor.ledger_id.clone(),
            generation: 2,
            owner_id: super::super::vfs::random_name_token().unwrap(),
            host: "gap".to_owned(),
            pid: 1,
            started_ms: 1,
        };
        publish_record(&vfs, &claim_name(&gap.ledger_id, gap.generation), &gap).unwrap();

        let error = RootLock::acquire_vfs(&vfs).err().unwrap();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn artifact_from_another_ledger_fails_closed() {
        let vfs = memory_root("foreign-ledger");
        let guard = RootLock::acquire_vfs(&vfs).unwrap();
        let other_ledger = super::super::vfs::random_name_token().unwrap();
        let foreign = LockClaim {
            protocol: LOCK_PROTOCOL,
            ledger_id: other_ledger.clone(),
            generation: 0,
            owner_id: super::super::vfs::random_name_token().unwrap(),
            host: "foreign".to_owned(),
            pid: 1,
            started_ms: 1,
        };
        publish_record(&vfs, &claim_name(&other_ledger, 0), &foreign).unwrap();

        let error = RootLock::acquire_vfs(&vfs).err().unwrap();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        drop(guard);
    }
}
