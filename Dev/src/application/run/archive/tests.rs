//! The sync-mode archive: the record of what the two sides last agreed on.
//!
//! Refreshed after a successful apply, and never over paths that ended in conflict — an archive
//! that claims agreement where there was none turns the next run's conflict into a silent
//! overwrite.

use super::publish::*;
use super::target::ArchiveTarget;
use super::*;
use crate::fs::vfs::memory::MemVfs;
use crate::fs::vfs::Vfs;
use crate::job::Job;
use crate::model::digest::Blake3Digest;
use crate::model::event::ProgressEvent;
use crate::obs::progress::{RunCtl, RunCtx};
use std::io::{self};
use std::sync::{Arc, Mutex};

fn legacy_archive_bytes() -> Vec<u8> {
    let digest = Blake3Digest::hash_bytes(b"legacy payload");
    format!(
        "{{\"schema\":1,\"kind\":\"archive\",\"root\":\"/data\",\"host\":\"host\",\"os\":\"linux\",\"scanned_at_ms\":1,\"duration_ms\":2,\"entry_count\":1,\"hashed\":true}}\n{{\"path\":\"file.txt\",\"kind\":\"File\",\"size\":14,\"mtime_ms\":3,\"hash\":\"{digest}\"}}\n"
    )
    .into_bytes()
}

#[test]
fn legacy_archive_migration_keeps_an_immutable_backup_and_receipt() {
    let directory =
        std::env::temp_dir().join(format!("syncdash-archive-migration-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    let archive = directory.join("archive.jsonl");
    let legacy = legacy_archive_bytes();
    std::fs::write(&archive, &legacy).unwrap();

    let migrated = load_archive(&archive).unwrap().unwrap();
    assert_eq!(migrated.header.schema, crate::model::table::TABLE_SCHEMA);
    let names = std::fs::read_dir(&directory)
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let backup = names
        .iter()
        .find(|name| name.ends_with(".v1.backup"))
        .expect("immutable v1 backup");
    assert_eq!(std::fs::read(directory.join(backup)).unwrap(), legacy);
    assert!(names.iter().any(|name| name.ends_with(".prepared.json")));
    assert_ne!(std::fs::read(&archive).unwrap(), legacy);

    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn mismatched_prepared_receipt_blocks_migration_without_touching_the_archive() {
    let directory = std::env::temp_dir().join(format!(
        "syncdash-archive-migration-receipt-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    let archive = directory.join("archive.jsonl");
    let legacy = legacy_archive_bytes();
    std::fs::write(&archive, &legacy).unwrap();
    let target = ArchiveTarget::open_for_read(&archive).unwrap().unwrap();
    let receipt = target.migration_path("prepared.json").unwrap();
    std::fs::write(directory.join(receipt.as_str()), b"{}\n").unwrap();

    assert!(load_archive(&archive).is_err());
    assert_eq!(std::fs::read(&archive).unwrap(), legacy);

    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn archive_read_does_not_create_a_missing_parent() {
    let base = std::env::temp_dir().join(format!(
        "syncdash-archive-missing-parent-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let missing_parent = base.join("missing");

    assert!(load_archive(&missing_parent.join("archive.jsonl"))
        .unwrap()
        .is_none());
    assert!(!missing_parent.exists());

    let _ = std::fs::remove_dir_all(base);
}

#[cfg(unix)]
#[test]
fn archive_read_refuses_a_symlink() {
    use std::os::unix::fs::symlink;

    let base = std::env::temp_dir().join(format!(
        "syncdash-archive-read-symlink-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let outside = base.join("outside.jsonl");
    std::fs::write(&outside, b"outside").unwrap();
    let archive = base.join("archive.jsonl");
    symlink(&outside, &archive).unwrap();

    assert!(load_archive(&archive).is_err());
    assert_eq!(std::fs::read(&outside).unwrap(), b"outside");

    let _ = std::fs::remove_dir_all(base);
}

#[cfg(unix)]
#[test]
fn archive_read_stays_with_the_retained_parent_after_a_name_swap() {
    use std::os::unix::fs::symlink;

    let base = std::env::temp_dir().join(format!(
        "syncdash-archive-read-parent-swap-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    let selected = base.join("selected");
    let detached = base.join("detached");
    let outside = base.join("outside");
    std::fs::create_dir_all(&selected).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(
        selected.join("archive.jsonl"),
        b"{\"schema\":1,\"kind\":\"archive\",\"root\":\"retained\",\"host\":\"\",\"os\":\"\",\"scanned_at_ms\":0,\"duration_ms\":0,\"entry_count\":0,\"hashed\":false}\n",
    )
    .unwrap();
    std::fs::write(
        outside.join("archive.jsonl"),
        b"{\"schema\":1,\"kind\":\"archive\",\"root\":\"outside\",\"host\":\"\",\"os\":\"\",\"scanned_at_ms\":0,\"duration_ms\":0,\"entry_count\":0,\"hashed\":false}\n",
    )
    .unwrap();
    let target = ArchiveTarget::open_for_read(&selected.join("archive.jsonl"))
        .unwrap()
        .unwrap();

    std::fs::rename(&selected, &detached).unwrap();
    symlink(&outside, &selected).unwrap();
    let lock = target.acquire_lock().unwrap();
    let snapshot = target.load_or_migrate(&lock).unwrap().unwrap();

    assert_eq!(snapshot.header.root, "retained");
    let _ = std::fs::remove_dir_all(base);
}

#[test]
fn archive_replacement_keeps_the_previous_file_until_commit() {
    let dir = std::env::temp_dir().join(format!("syncdash-archive-atomic-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let archive = dir.join("archive.jsonl");
    std::fs::write(&archive, b"previous snapshot\n").unwrap();

    let failed = write_archive_atomic(
        &archive,
        |writer| {
            writer.write_all(b"partial replacement\n")?;
            Err(io::Error::other("simulated write failure"))
        },
        || Ok(()),
    );
    assert!(failed.is_err());
    assert_eq!(std::fs::read(&archive).unwrap(), b"previous snapshot\n");

    let cancelled = write_archive_atomic(
        &archive,
        |writer| writer.write_all(b"complete but cancelled replacement\n"),
        || Err(crate::obs::progress::cancelled_err()),
    );
    assert!(cancelled.is_err());
    assert_eq!(std::fs::read(&archive).unwrap(), b"previous snapshot\n");

    write_archive_atomic(
        &archive,
        |writer| writer.write_all(b"complete replacement\n"),
        || Ok(()),
    )
    .unwrap();
    assert_eq!(std::fs::read(&archive).unwrap(), b"complete replacement\n");
    let leftovers: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(crate::foundation::names::TEMP_PREFIX)
        })
        .collect();
    assert!(leftovers.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn archive_write_stays_with_the_retained_parent_after_a_name_swap() {
    use std::os::unix::fs::symlink;

    let base = std::env::temp_dir().join(format!(
        "syncdash-archive-parent-swap-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    let selected = base.join("selected");
    let detached = base.join("detached");
    let outside = base.join("outside");
    std::fs::create_dir_all(&selected).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(selected.join("archive.jsonl"), b"old").unwrap();
    let target = ArchiveTarget::open_for_write(&selected.join("archive.jsonl")).unwrap();

    std::fs::rename(&selected, &detached).unwrap();
    symlink(&outside, &selected).unwrap();
    let lock = target.acquire_lock().unwrap();
    write_archive_to(
        &target,
        &lock,
        |writer| writer.write_all(b"confined"),
        || Ok(()),
    )
    .unwrap();

    assert_eq!(
        std::fs::read(detached.join("archive.jsonl")).unwrap(),
        b"confined"
    );
    assert!(!outside.join("archive.jsonl").exists());
    let _ = std::fs::remove_dir_all(base);
}

#[test]
fn archive_write_failure_is_reported_to_the_run() {
    let source = Arc::new(MemVfs::new("archive-failure-source")) as Arc<dyn Vfs>;
    let target = Arc::new(MemVfs::new("archive-failure-target")) as Arc<dyn Vfs>;
    let mut job = Job::default();
    let dir = std::env::temp_dir().join(format!("syncdash-archive-failure-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    job.archive = Some(dir.clone()); // File::create on a directory must fail on every platform.
    let plan = crate::run::local::compare_resolved(&job, &source, &target, &RunCtx::null())
        .unwrap()
        .plan;
    let (ctx, events) = RunCtx::collecting();

    assert!(!refresh_archive_with(
        &job,
        &plan,
        &source,
        &crate::run::scan_opts(&job),
        &ctx
    ));
    assert!(events
        .lock()
        .unwrap()
        .iter()
        .any(|ev| matches!(ev, ProgressEvent::Error {
        action,
        ..
    } if action == "archive")));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cancelled_refresh_is_not_reported_as_an_archive_error() {
    let source = Arc::new(MemVfs::new("archive-cancel-source")) as Arc<dyn Vfs>;
    let target = Arc::new(MemVfs::new("archive-cancel-target")) as Arc<dyn Vfs>;
    let mut job = Job::default();
    let dir = std::env::temp_dir().join(format!("syncdash-archive-cancel-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    job.archive = Some(dir.join("archive.jsonl"));
    let plan = crate::run::local::compare_resolved(&job, &source, &target, &RunCtx::null())
        .unwrap()
        .plan;
    let ctl = RunCtl::new();
    ctl.request_cancel();
    let (ctx, events) = RunCtx::collecting_with(ctl);

    assert!(!refresh_archive_with(
        &job,
        &plan,
        &source,
        &crate::run::scan_opts(&job),
        &ctx
    ));
    let events = events.lock().unwrap();
    assert!(!events
        .iter()
        .any(|ev| matches!(ev, ProgressEvent::Error { .. })));
    assert!(matches!(
        events.last(),
        Some(ProgressEvent::PhaseEnd {
            status: crate::model::event::PhaseStatus::Cancelled,
            ..
        })
    ));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cancellation_at_the_archive_boundary_reaches_the_run_outcome() {
    let source = Arc::new(MemVfs::new("archive-boundary-source")) as Arc<dyn Vfs>;
    let target = Arc::new(MemVfs::new("archive-boundary-target")) as Arc<dyn Vfs>;
    let mut job = Job::default();
    let dir =
        std::env::temp_dir().join(format!("syncdash-archive-boundary-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let archive = dir.join("archive.jsonl");
    job.archive = Some(archive.clone());
    let plan = crate::run::local::compare_resolved(&job, &source, &target, &RunCtx::null())
        .unwrap()
        .plan;
    let events: Arc<Mutex<Vec<ProgressEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let copy = events.clone();
    let ctl = RunCtl::new();
    let cancel = ctl.clone();
    let ctx = RunCtx::new(
        ctl,
        Arc::new(move |ev| {
            if matches!(
                &ev,
                ProgressEvent::Progress {
                    phase: crate::model::event::Phase::Archive,
                    ..
                }
            ) {
                cancel.request_cancel();
            }
            copy.lock().unwrap().push(ev);
        }),
    );

    assert!(!refresh_archive_with(
        &job,
        &plan,
        &source,
        &crate::run::scan_opts(&job),
        &ctx
    ));
    assert!(
        archive.is_file(),
        "the cancellation arrived after the atomic archive commit"
    );
    let events = events.lock().unwrap();
    assert!(!events
        .iter()
        .any(|ev| matches!(ev, ProgressEvent::Error { .. })));
    assert!(matches!(
        events.last(),
        Some(ProgressEvent::PhaseEnd {
            phase: crate::model::event::Phase::Archive,
            status: crate::model::event::PhaseStatus::Cancelled,
            ..
        })
    ));
    let _ = std::fs::remove_dir_all(&dir);
}
