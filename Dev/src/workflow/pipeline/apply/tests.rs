use super::*;
use crate::fs::vfs::memory::MemVfs;
use crate::fs::vfs::{NameRules, Vfs};
use crate::model::event::Phase;
use crate::model::plan::{Action, Side};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use super::validation::validate_operation_paths;

fn copy_op(path: &str, size: usize) -> Op {
    Op {
        side: Side::Target,
        action: Action::Copy,
        path: path.to_owned(),
        from: None,
        size: Some(size as u64),
        mtime_ms: Some(1),
        hash: None,
        link: None,
        mode: None,
        reason: "lease test".to_owned(),
    }
}

fn remove_current_claim(root: &MemVfs) {
    let claims = root
        .read_dir_names("")
        .unwrap()
        .into_iter()
        .filter(|(name, kind)| {
            *kind == crate::fs::vfs::VfsEntryKind::File
                && name
                    .as_str()
                    .starts_with(crate::foundation::names::LOCK_NAME)
                && name.as_str().contains(".claim.")
        })
        .map(|(name, _)| name)
        .collect::<Vec<_>>();
    assert_eq!(claims.len(), 1);
    root.remove_file(claims[0].as_str()).unwrap();
}

fn options() -> ApplyOptions {
    ApplyOptions {
        dry_run: false,
        parallel: 1,
        fsync: false,
        ..ApplyOptions::default()
    }
}

#[test]
fn lease_loss_during_copy_never_publishes_the_staged_destination() {
    let source = Arc::new(MemVfs::new("lease-loss-copy-source"));
    let target = Arc::new(MemVfs::new("lease-loss-copy-target"));
    let content = vec![7u8; 512 * 1024];
    source.seed_bytes("large.bin", &content, 1);

    let hook_source = source.clone();
    let fired = Arc::new(AtomicBool::new(false));
    let hook_fired = fired.clone();
    source.set_read_hook(move |path, _| {
        if path == "large.bin" && !hook_fired.swap(true, Ordering::AcqRel) {
            remove_current_claim(&hook_source);
            std::thread::sleep(std::time::Duration::from_millis(80));
        }
    });

    let source_vfs: Arc<dyn Vfs> = source;
    let target_vfs: Arc<dyn Vfs> = target.clone();
    let events = Arc::new(Mutex::new(Vec::new()));
    let event_store = events.clone();
    let ctx = RunCtx::new(
        crate::obs::progress::RunCtl::new(),
        Arc::new(move |event| event_store.lock().unwrap().push(event)),
    );
    let out = apply_vfs(
        &[copy_op("large.bin", content.len())],
        &source_vfs,
        &target_vfs,
        &options(),
        &ctx,
    );

    assert!(fired.load(Ordering::Acquire));
    assert_eq!(out.done, 0);
    assert_eq!(out.errors, 1);
    assert!(
        !out.cancelled,
        "lease loss is a safety failure, not user cancel"
    );
    assert!(target.stat("large.bin").unwrap().is_none());
    assert!(events.lock().unwrap().iter().any(|event| matches!(
        event,
        crate::model::event::ProgressEvent::PhaseEnd {
            phase: Phase::Apply,
            status: crate::model::event::PhaseStatus::Failed,
            ..
        }
    )));
}

#[test]
fn lease_loss_after_one_commit_blocks_the_next_operation() {
    let source = Arc::new(MemVfs::new("lease-loss-next-source"));
    let target = Arc::new(MemVfs::new("lease-loss-next-target"));
    source.seed_bytes("first.bin", b"first", 1);
    source.seed_bytes("second.bin", b"second", 1);

    let hook_source = source.clone();
    let fired = Arc::new(AtomicBool::new(false));
    let hook_fired = fired.clone();
    target.set_commit_hook(move |path| {
        if path == "first.bin" && !hook_fired.swap(true, Ordering::AcqRel) {
            remove_current_claim(&hook_source);
            std::thread::sleep(std::time::Duration::from_millis(80));
        }
    });

    let source_vfs: Arc<dyn Vfs> = source;
    let target_vfs: Arc<dyn Vfs> = target.clone();
    let out = apply_vfs(
        &[copy_op("first.bin", 5), copy_op("second.bin", 6)],
        &source_vfs,
        &target_vfs,
        &options(),
        &RunCtx::null(),
    );

    assert!(fired.load(Ordering::Acquire));
    assert_eq!(out.done, 1);
    assert_eq!(out.errors, 1);
    assert!(
        !out.cancelled,
        "lease loss is a safety failure, not user cancel"
    );
    assert!(target.stat("first.bin").unwrap().is_some());
    assert!(target.stat("second.bin").unwrap().is_none());
}

#[test]
fn destination_appearing_at_publication_is_never_replaced() {
    let source = Arc::new(MemVfs::new("publish-race-source"));
    let target = Arc::new(MemVfs::new("publish-race-target"));
    source.seed_bytes("file.bin", b"planned content", 1);

    let hook_target = target.clone();
    let fired = Arc::new(AtomicBool::new(false));
    let hook_fired = fired.clone();
    target.set_noreplace_pre_publish_hook(move |path| {
        if path == "file.bin" && !hook_fired.swap(true, Ordering::AcqRel) {
            hook_target.seed_bytes(path, b"external writer", 2);
        }
    });

    let source_vfs: Arc<dyn Vfs> = source;
    let target_vfs: Arc<dyn Vfs> = target.clone();
    let out = apply_vfs(
        &[copy_op("file.bin", b"planned content".len())],
        &source_vfs,
        &target_vfs,
        &options(),
        &RunCtx::null(),
    );

    assert!(fired.load(Ordering::Acquire));
    assert_eq!((out.done, out.errors), (0, 1));
    let mut reader = target.open_read("file.bin").unwrap();
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut reader, &mut bytes).unwrap();
    assert_eq!(bytes, b"external writer");
}

#[test]
fn published_file_with_wrong_permissions_is_not_reported_as_success() {
    let source = Arc::new(MemVfs::new("mode-failure-source"));
    let target = Arc::new(
        MemVfs::new("mode-failure-target")
            .failing_commit_mode(crate::fs::vfs::error::VfsErrorKind::PermissionDenied),
    );
    source.seed_bytes("file.bin", b"content", 1);
    let mut operation = copy_op("file.bin", b"content".len());
    operation.mode = Some(0o600);

    let source_vfs: Arc<dyn Vfs> = source;
    let target_vfs: Arc<dyn Vfs> = target.clone();
    let out = apply_vfs(
        &[operation],
        &source_vfs,
        &target_vfs,
        &options(),
        &RunCtx::null(),
    );

    assert_eq!((out.done, out.errors), (0, 1));
    assert!(target.stat("file.bin").unwrap().is_some());
}

#[test]
fn plans_cannot_mutate_internal_artifact_paths() {
    let source = Arc::new(MemVfs::new("reserved-path-source"));
    let target = Arc::new(MemVfs::new("reserved-path-target"));
    source.seed_bytes("nested/.syncdash/cache.bin", b"content", 1);
    let operation = copy_op("nested/.syncdash/cache.bin", b"content".len());

    let source_vfs: Arc<dyn Vfs> = source.clone();
    let target_vfs: Arc<dyn Vfs> = target.clone();
    let out = apply_vfs(
        &[operation],
        &source_vfs,
        &target_vfs,
        &options(),
        &RunCtx::null(),
    );

    assert_eq!((out.done, out.skipped, out.errors), (0, 1, 1));
    assert!(target.stat("nested/.syncdash/cache.bin").unwrap().is_none());
    assert!(source
        .stat(crate::foundation::names::LOCK_NAME)
        .unwrap()
        .is_none());
    assert!(target
        .stat(crate::foundation::names::LOCK_NAME)
        .unwrap()
        .is_none());

    let mut move_operation = copy_op("safe.bin", 1);
    move_operation.action = Action::Move;
    move_operation.from = Some("nested/.syncdash.tmp.claim".to_owned());
    assert!(validate_operation_paths(&[move_operation])
        .unwrap_err()
        .contains("reserved for SyncDash metadata"));
}

#[test]
fn move_hash_evidence_requires_a_canonical_digest() {
    let mut operation = copy_op("moved.bin", 1);
    operation.action = Action::Move;
    operation.from = Some("original.bin".to_owned());
    operation.hash = Some("A".repeat(64));

    assert!(validate_operation_paths(&[operation])
        .unwrap_err()
        .contains("invalid content digest"));
}

#[test]
fn imported_plan_cannot_bypass_windows_name_addressing_rules() {
    let source = Arc::new(MemVfs::new("windows-name-source"));
    let target = Arc::new(MemVfs::new("windows-name-target").without(|capabilities| {
        capabilities.name_rules = NameRules::Windows;
    }));
    source.seed_bytes("report:2024.pdf", b"content", 1);

    let source_vfs: Arc<dyn Vfs> = source;
    let target_vfs: Arc<dyn Vfs> = target.clone();
    let outcome = apply_vfs(
        &[copy_op("report:2024.pdf", b"content".len())],
        &source_vfs,
        &target_vfs,
        &options(),
        &RunCtx::null(),
    );

    assert_eq!((outcome.done, outcome.skipped, outcome.errors), (0, 1, 1));
    assert!(target.stat("report:2024.pdf").unwrap().is_none());
}
