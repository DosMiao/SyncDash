//! Capability reporting behavior.

use super::report::*;
use super::write::*;
use crate::model::plan::{Action, Op, Side};

fn copy_to_target() -> Op {
    Op {
        side: Side::Target,
        action: Action::Copy,
        path: "file.txt".into(),
        from: None,
        size: Some(1),
        mtime_ms: Some(1),
        hash: None,
        link: None,
        mode: None,
        reason: "test".into(),
    }
}

fn write_report_with_target_caps(target: crate::fs::vfs::VfsCaps) -> CapReport {
    use crate::fs::vfs::{memory::MemVfs, Vfs};

    let source = MemVfs::new("caps-source").caps();
    cap_report_write(
        &WriteCapsQuery {
            fsync: false,
            verify: false,
            versioning: false,
            delta: false,
            src_local: false,
            tgt_local: false,
        },
        &[copy_to_target()],
        &source,
        &target,
    )
}

/// A plan that only adds files displaces nothing, so it has no preservation story to tell.
/// Counting `Copy` as destructive described a trash or version-store effect for runs that
/// never reach one — the report has to match the plan it was given.
#[test]
fn a_copy_only_plan_reports_no_preservation_effect() {
    use crate::fs::vfs::{memory::MemVfs, Vfs};

    let source = MemVfs::new("caps-source").caps();
    let mut target = MemVfs::new("caps-target").caps();
    target.local_trash = false;
    let query = WriteCapsQuery {
        fsync: false,
        verify: false,
        versioning: false,
        delta: false,
        src_local: false,
        tgt_local: false,
    };

    let copy_only = cap_report_write(&query, &[copy_to_target()], &source, &target);
    assert!(copy_only
        .items
        .iter()
        .all(|item| item.feature != "trash" && item.feature != "versioning"));

    let mut overwrite = copy_to_target();
    overwrite.action = Action::Update;
    let displacing = cap_report_write(&query, &[overwrite], &source, &target);
    assert!(displacing
        .items
        .iter()
        .any(|item| item.feature == "trash" && item.side == "target"));
}

#[test]
fn root_lock_requires_exclusive_staged_publish_not_set_mtime() {
    use crate::fs::vfs::{memory::MemVfs, Support, Vfs};

    let mut target = MemVfs::new("caps-target").caps();
    target.set_mtime = Support::No;
    target.exclusive_staged_file_publish = Support::Yes;
    let report = write_report_with_target_caps(target);
    assert!(report.items.iter().all(|item| item.feature != "root lock"));
}

#[test]
fn root_lock_is_reported_when_exclusive_staged_publish_is_not_established() {
    use crate::fs::vfs::{memory::MemVfs, Support, Vfs};

    for (support, expected_actual) in [
        (
            Support::No,
            "backend cannot atomically publish a staged file onto an absent name",
        ),
        (
            Support::Unknown,
            "exclusive staged-file publication is not established for this backend",
        ),
    ] {
        let mut target = MemVfs::new("caps-target").caps();
        target.set_mtime = Support::Yes;
        target.exclusive_staged_file_publish = support;
        let report = write_report_with_target_caps(target);
        let reported = report
            .unavailable()
            .into_iter()
            .find(|item| item.feature == "root lock")
            .expect("missing exclusive staged-file publication must be reported");
        assert_eq!(reported.actual, expected_actual);
    }
}

#[test]
fn root_lock_checks_both_roots_even_when_only_target_has_operations() {
    use crate::fs::vfs::{memory::MemVfs, Support, Vfs};

    let mut source = MemVfs::new("caps-source").caps();
    source.exclusive_staged_file_publish = Support::No;
    let target = MemVfs::new("caps-target").caps();
    let report = cap_report_write(
        &WriteCapsQuery {
            fsync: false,
            verify: false,
            versioning: false,
            delta: false,
            src_local: false,
            tgt_local: false,
        },
        &[copy_to_target()],
        &source,
        &target,
    );
    assert!(report
        .unavailable()
        .into_iter()
        .any(|item| item.feature == "root lock" && item.side == "source"));
}

#[test]
fn existing_entry_rename_is_reported_independently_from_staged_publication() {
    use crate::fs::vfs::{memory::MemVfs, Support, Vfs};

    let mut target = MemVfs::new("caps-target").caps();
    target.exclusive_staged_file_publish = Support::Yes;
    target.exclusive_entry_rename = Support::Unknown;
    let report = write_report_with_target_caps(target);
    assert!(report
        .unavailable()
        .into_iter()
        .any(|item| item.feature == "entry rename" && item.side == "target"));
}

#[test]
fn symlink_publication_has_its_own_exclusive_primitive() {
    use crate::fs::vfs::{memory::MemVfs, Support, Vfs};

    let mut target = MemVfs::new("caps-target").caps();
    target.symlink = Support::Yes;
    target.exclusive_symlink_publish = Support::No;
    let mut operation = copy_to_target();
    operation.link = Some("destination".into());
    let source = MemVfs::new("caps-source").caps();
    let report = cap_report_write(
        &WriteCapsQuery {
            fsync: false,
            verify: false,
            versioning: false,
            delta: false,
            src_local: false,
            tgt_local: false,
        },
        &[operation],
        &source,
        &target,
    );
    assert!(report
        .unavailable()
        .into_iter()
        .any(|item| item.feature == "symlink publication"));
}

#[test]
fn file_flush_does_not_stand_in_for_namespace_durability() {
    use crate::fs::vfs::{memory::MemVfs, Support, Vfs};

    let source = MemVfs::new("caps-source").caps();
    let mut target = MemVfs::new("caps-target").caps();
    target.fsync = Support::Yes;
    target.durable_namespace = Support::Unknown;
    let report = cap_report_write(
        &WriteCapsQuery {
            fsync: true,
            verify: false,
            versioning: false,
            delta: false,
            src_local: false,
            tgt_local: false,
        },
        &[copy_to_target()],
        &source,
        &target,
    );
    assert!(report.items.iter().any(|item| {
        item.feature == "fsync namespace"
            && item.side == "target"
            && item.severity == CapSeverity::Degraded
    }));
}
