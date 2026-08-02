//! Exclusive lease for a sync root.
//!
//! `.syncdash.lock` is an immutable random ledger anchor. Each acquisition contends on one
//! generation-specific claim name with an atomic no-replace commit; release is a separate,
//! owner-specific immutable record. No guard updates or deletes a shared claim, so an old guard
//! cannot touch or release a later owner's lease.

mod artifact;
mod ledger;
mod record_store;

#[cfg(test)]
mod tests;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::Arc;

use crate::fs::vfs::error::VfsErrorKind;
use crate::fs::vfs::Vfs;

use self::artifact::{
    claim_name, heartbeat_name, invalid_lock, LockAnchor, LockClaim, LockHeartbeat, LOCK_PROTOCOL,
};
use self::ledger::{
    claim_is_current, inspect_ledger, lease_identity_is_current, occupied_error, release_claim,
};
use self::record_store::{ensure_anchor, publish_record, read_anchor};

#[cfg(not(test))]
const HEARTBEAT_MS: u64 = 4000;
#[cfg(test)]
const HEARTBEAT_MS: u64 = 40;
#[cfg(not(test))]
const HEARTBEAT_POLL_MS: u64 = 250;
#[cfg(test)]
const HEARTBEAT_POLL_MS: u64 = 10;

pub struct RootLock {
    vfs: Arc<dyn Vfs>,
    anchor: LockAnchor,
    claim: LockClaim,
    heartbeat_stop: Option<Sender<()>>,
    lease_lost: Arc<AtomicBool>,
    heartbeat: Option<std::thread::JoinHandle<()>>,
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
