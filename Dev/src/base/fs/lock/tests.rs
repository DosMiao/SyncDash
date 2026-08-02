//! Lease behavior: contention, heartbeat loss, release, and refusal on an unreadable ledger.
//!
//! These reach private ledger internals on purpose — the invariants under test are about records
//! that must never be written, which no public surface can express.

use super::artifact::{claim_name, heartbeat_name, release_name, LockRelease};
use super::record_store::{publish_record, read_record};
use super::*;
use crate::foundation::names::LOCK_NAME;
use crate::fs::vfs::memory::MemVfs;
use crate::fs::vfs::VfsEntryKind;

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

    let first = RootLock::acquire_vfs(&vfs)
        .expect("the exact immutable claim proves ownership after an indeterminate publish result");
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
    replacement.owner_id = crate::fs::vfs::random_name_token().unwrap();
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
        owner_id: crate::fs::vfs::random_name_token().unwrap(),
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
        owner_id: crate::fs::vfs::random_name_token().unwrap(),
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
    let other_ledger = crate::fs::vfs::random_name_token().unwrap();
    let foreign = LockClaim {
        protocol: LOCK_PROTOCOL,
        ledger_id: other_ledger.clone(),
        generation: 0,
        owner_id: crate::fs::vfs::random_name_token().unwrap(),
        host: "foreign".to_owned(),
        pid: 1,
        started_ms: 1,
    };
    publish_record(&vfs, &claim_name(&other_ledger, 0), &foreign).unwrap();

    let error = RootLock::acquire_vfs(&vfs).err().unwrap();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    drop(guard);
}
