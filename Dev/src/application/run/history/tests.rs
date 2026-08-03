use super::codec::{legacy_record_id, parse_current_record};
use super::migration::{
    ensure_current_schema_locked, migrate_current_schema_locked, migrate_legacy_record,
};
use super::model::{LegacyRunRecord, RUN_RECORD_SCHEMA};
use super::paths::{run_identifier, sanitize};
use super::recording::{create_run_dir, pending_record};
use super::repository::{
    artifact_lines_at, history_at, history_merged_at, with_validated_reveal_target_at,
};
use super::retention::{prune_at, sweep_orphans};
use super::*;
use std::collections::HashMap;

use crate::foundation::names::{
    RUNLOG_INDEX_FILE as INDEX_FILE, RUNLOG_LEGACY_INDEX_FILE, RUNLOG_LEGACY_SUMMARY_FILE,
    RUNLOG_RUN_FILE, RUNLOG_SCHEMA_FILE, RUNLOG_SUMMARY_FILE as SUMMARY_FILE,
};
use crate::foundation::path::EntryName;
use crate::fs::local_root::LocalRoot;

const RECORD_A: &str = "0123456789abcdef0123456789abcdef";
const RECORD_B: &str = "fedcba9876543210fedcba9876543210";

fn test_subject(job_name: &str) -> RunSubject {
    RunSubject {
        job_name: job_name.to_owned(),
        binding: RunJobBinding::LegacyUnbound,
        target_index: None,
    }
}

fn test_apply_record(record_id: &str, run_id: &str, finished: bool) -> RunRecord {
    RunRecord {
        schema: RUN_RECORD_SCHEMA,
        record_id: record_id.to_owned(),
        ts_ms: 1,
        subject: test_subject("job"),
        kind: RunKind::Apply,
        done: 0,
        skipped: 0,
        errors: 0,
        bytes: 0,
        elapsed_ms: 0,
        cancelled: false,
        artifacts: RunArtifacts::Directory {
            run_id: run_id.to_owned(),
        },
        warnings: 0,
        ops_found: None,
        finished,
    }
}

fn write_current_index(root_path: &std::path::Path, records: &[RunRecord]) {
    let index = records
        .iter()
        .map(|record| format!("{}\n", serde_json::to_string(record).unwrap()))
        .collect::<String>();
    std::fs::write(root_path.join(INDEX_FILE), index).unwrap();
    std::fs::write(
        root_path.join(RUNLOG_SCHEMA_FILE),
        format!("{RUN_RECORD_SCHEMA}\n"),
    )
    .unwrap();
}

#[test]
fn current_records_are_strict_and_legacy_records_require_migration() {
    let a = RunRecord {
        schema: RUN_RECORD_SCHEMA,
        record_id: RECORD_A.into(),
        ts_ms: 1,
        subject: test_subject("j"),
        kind: RunKind::Apply,
        done: 3,
        skipped: 0,
        errors: 1,
        bytes: 42,
        elapsed_ms: 100,
        cancelled: false,
        artifacts: RunArtifacts::Directory {
            run_id: "20260101-000000-j-apply".into(),
        },
        warnings: 2,
        ops_found: None,
        finished: true,
    };
    let s = serde_json::to_string(&a).unwrap();
    let b: RunRecord = serde_json::from_str(&s).unwrap();
    assert_eq!((b.done, b.errors, b.warnings), (3, 1, 2));
    let old = r#"{"ts_ms":9,"job":"j","kind":"apply","done":1,"skipped":0,"errors":0,
            "bytes":0,"elapsed_ms":5,"cancelled":false,"detail":"9-j.jsonl"}"#;
    assert!(serde_json::from_str::<RunRecord>(old).is_err());
    let legacy = serde_json::from_str::<LegacyRunRecord>(old).unwrap();
    let migrated =
        migrate_legacy_record(legacy, legacy_record_id(old, "0"), &HashMap::new()).unwrap();
    assert!(matches!(
        migrated.subject.binding,
        RunJobBinding::LegacyUnbound
    ));
    assert!(matches!(
        migrated.artifacts,
        RunArtifacts::LegacyFile { ref file_name } if file_name == "9-j.jsonl"
    ));
    assert!(migrated.finished);
}

#[test]
fn legacy_index_and_interrupted_summary_migrate_once_with_immutable_sources() {
    let root_path = std::env::temp_dir().join(format!(
        "syncdash-runlog-migration-{}-{}",
        std::process::id(),
        crate::foundation::time::now_ms()
    ));
    let _ = std::fs::remove_dir_all(&root_path);
    let run_id = "20260101-000000-job-apply";
    std::fs::create_dir_all(root_path.join(run_id)).unwrap();
    let legacy_index = format!(
            "{{\"ts_ms\":9,\"job\":\"job\",\"kind\":\"apply\",\"done\":1,\"skipped\":0,\"errors\":0,\"bytes\":0,\"elapsed_ms\":5,\"cancelled\":false,\"run_id\":\"{run_id}\",\"finished\":true}}\n"
        );
    let legacy_summary = format!(
            "{{\"ts_ms\":9,\"job\":\"job\",\"kind\":\"apply\",\"done\":0,\"skipped\":1,\"errors\":0,\"bytes\":0,\"elapsed_ms\":0,\"cancelled\":false,\"run_id\":\"{run_id}\",\"finished\":false}}"
        );
    std::fs::write(root_path.join(INDEX_FILE), &legacy_index).unwrap();
    std::fs::write(root_path.join(run_id).join(SUMMARY_FILE), &legacy_summary).unwrap();
    let root = LocalRoot::open(root_path.clone()).unwrap();

    migrate_current_schema_locked(&root, &HashMap::new()).unwrap();

    assert_eq!(
        std::fs::read_to_string(root_path.join(RUNLOG_LEGACY_INDEX_FILE)).unwrap(),
        legacy_index
    );
    assert_eq!(
        std::fs::read_to_string(root_path.join(run_id).join(RUNLOG_LEGACY_SUMMARY_FILE)).unwrap(),
        legacy_summary
    );
    assert_eq!(
        std::fs::read_to_string(root_path.join(RUNLOG_SCHEMA_FILE)).unwrap(),
        format!("{RUN_RECORD_SCHEMA}\n")
    );
    let index_record = parse_current_record(
        std::fs::read_to_string(root_path.join(INDEX_FILE))
            .unwrap()
            .trim(),
        "migrated test index",
    )
    .unwrap();
    let summary_record = parse_current_record(
        &std::fs::read_to_string(root_path.join(run_id).join(SUMMARY_FILE)).unwrap(),
        "migrated test summary",
    )
    .unwrap();
    assert_eq!(summary_record.record_id, index_record.record_id);
    assert!(index_record.finished);
    assert!(!summary_record.finished);
    assert!(matches!(
        index_record.subject.binding,
        RunJobBinding::LegacyUnbound
    ));

    ensure_current_schema_locked(&root).unwrap();
    let _ = std::fs::remove_dir_all(root_path);
}

#[test]
fn migration_refuses_a_conflicting_backup_without_rewriting_the_index() {
    let root_path = std::env::temp_dir().join(format!(
        "syncdash-runlog-migration-conflict-{}-{}",
        std::process::id(),
        crate::foundation::time::now_ms()
    ));
    let _ = std::fs::remove_dir_all(&root_path);
    std::fs::create_dir_all(&root_path).unwrap();
    let legacy_index = "{\"ts_ms\":9,\"job\":\"job\",\"kind\":\"compare\",\"done\":0,\"skipped\":0,\"errors\":0,\"bytes\":0,\"elapsed_ms\":5,\"cancelled\":false,\"ops_found\":1}\n";
    std::fs::write(root_path.join(INDEX_FILE), legacy_index).unwrap();
    std::fs::write(root_path.join(RUNLOG_LEGACY_INDEX_FILE), "different").unwrap();
    let root = LocalRoot::open(root_path.clone()).unwrap();

    let error = migrate_current_schema_locked(&root, &HashMap::new()).unwrap_err();

    assert!(error.to_string().contains("does not match its source"));
    assert_eq!(
        std::fs::read_to_string(root_path.join(INDEX_FILE)).unwrap(),
        legacy_index
    );
    assert!(!root_path.join(RUNLOG_SCHEMA_FILE).exists());
    let _ = std::fs::remove_dir_all(root_path);
}

#[test]
fn path_escapes_are_refused() {
    assert!(run_identifier("../../etc/passwd").is_err());
    assert!(run_identifier("a/b").is_err());
    assert!(run_identifier("a\\b").is_err());
    assert!(run_identifier("").is_err());
    assert!(run_identifier("20260101-000000-job-apply").is_ok());
    assert!(artifact_lines("../secrets", LogArtifactKind::Run, 10)
        .unwrap_err()
        .contains("record_id"));
}

#[test]
fn reveal_targets_are_validated_before_the_presentation_callback() {
    let root_path = std::env::temp_dir().join(format!(
        "syncdash-runlog-reveal-{}-{}",
        std::process::id(),
        crate::foundation::time::now_ms()
    ));
    let _ = std::fs::remove_dir_all(&root_path);
    let run_id = "20260101-000000-job-apply";
    std::fs::create_dir_all(root_path.join(run_id)).unwrap();
    write_current_index(&root_path, &[test_apply_record(RECORD_A, run_id, true)]);

    let root_target =
        with_validated_reveal_target_at(root_path.clone(), None, |path| Ok(path.to_path_buf()))
            .unwrap();
    assert_eq!(root_target, root_path);
    let run_target = with_validated_reveal_target_at(root_path.clone(), Some(RECORD_A), |path| {
        Ok(path.to_path_buf())
    })
    .unwrap();
    assert_eq!(run_target, root_path.join(run_id));

    for rejected in ["../outside", "unrelated", RECORD_B] {
        let called = std::cell::Cell::new(false);
        assert!(
            with_validated_reveal_target_at(root_path.clone(), Some(rejected), |_| {
                called.set(true);
                Ok(())
            },)
            .is_err()
        );
        assert!(!called.get());
    }

    let _ = std::fs::remove_dir_all(root_path);
}

#[cfg(unix)]
#[test]
fn reveal_refuses_a_symlinked_run_directory_before_the_callback() {
    use std::os::unix::fs::symlink;

    let root_path = std::env::temp_dir().join(format!(
        "syncdash-runlog-reveal-link-root-{}",
        std::process::id()
    ));
    let outside = std::env::temp_dir().join(format!(
        "syncdash-runlog-reveal-link-outside-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root_path);
    let _ = std::fs::remove_dir_all(&outside);
    std::fs::create_dir_all(&root_path).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    let run_id = "20260101-000000-job-apply";
    symlink(&outside, root_path.join(run_id)).unwrap();
    write_current_index(&root_path, &[test_apply_record(RECORD_A, run_id, true)]);
    let called = std::cell::Cell::new(false);

    assert!(
        with_validated_reveal_target_at(root_path.clone(), Some(RECORD_A), |_| {
            called.set(true);
            Ok(())
        })
        .is_err()
    );
    assert!(!called.get());

    let _ = std::fs::remove_dir_all(root_path);
    let _ = std::fs::remove_dir_all(outside);
}

#[test]
fn sanitize_strips_path_chars() {
    assert!(sanitize("a b/c\\d")
        .chars()
        .all(|c| c.is_alphanumeric() || c == '_' || c == '-'));
}

#[test]
fn run_directories_are_unique_inside_the_same_millisecond() {
    let root = std::env::temp_dir().join(format!(
        "syncdash-run-id-{}-{}",
        std::process::id(),
        crate::foundation::time::now_ms()
    ));
    let local_root = LocalRoot::create(root.clone()).unwrap();
    let first = create_run_dir(&local_root, 1_700_000_000_123, "job", RunKind::Apply).unwrap();
    let second = create_run_dir(&local_root, 1_700_000_000_123, "job", RunKind::Apply).unwrap();
    assert_ne!(first.0, second.0);
    let _ = std::fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn symlinked_run_directories_cannot_redirect_history_or_artifact_reads() {
    use std::os::unix::fs::symlink;

    let root_path = std::env::temp_dir().join(format!(
        "syncdash-runlog-confined-root-{}",
        std::process::id()
    ));
    let outside = std::env::temp_dir().join(format!(
        "syncdash-runlog-confined-outside-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root_path);
    let _ = std::fs::remove_dir_all(&outside);
    std::fs::create_dir_all(&root_path).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    let run_id = EntryName::try_from("20260101-000000-forged-apply").unwrap();
    let forged = pending_record(
        RECORD_A,
        &test_subject("forged"),
        RunKind::Apply,
        1,
        run_id.as_str(),
        0,
    );
    std::fs::write(
        outside.join(SUMMARY_FILE),
        serde_json::to_vec(&forged).unwrap(),
    )
    .unwrap();
    std::fs::write(outside.join(RUNLOG_RUN_FILE), b"outside-secret\n").unwrap();
    symlink(&outside, root_path.join(run_id.as_str())).unwrap();
    let root = LocalRoot::open(root_path.clone()).unwrap();

    assert!(artifact_lines_at(&root, &run_id, LogArtifactKind::Run, 10).is_err());
    assert!(history_merged_at(&root, None, 10).unwrap().is_empty());
    assert_eq!(
        std::fs::read(outside.join(RUNLOG_RUN_FILE)).unwrap(),
        b"outside-secret\n"
    );

    let _ = std::fs::remove_dir_all(root_path);
    let _ = std::fs::remove_dir_all(outside);
}

#[cfg(unix)]
#[test]
fn index_symlinks_surface_an_error_instead_of_reading_their_target() {
    use std::os::unix::fs::symlink;

    let root_path =
        std::env::temp_dir().join(format!("syncdash-runlog-index-root-{}", std::process::id()));
    let outside = std::env::temp_dir().join(format!(
        "syncdash-runlog-index-outside-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root_path);
    let _ = std::fs::remove_file(&outside);
    std::fs::create_dir_all(&root_path).unwrap();
    std::fs::write(&outside, b"{}\n").unwrap();
    symlink(&outside, root_path.join(INDEX_FILE)).unwrap();
    let root = LocalRoot::open(root_path.clone()).unwrap();

    assert!(history_at(&root, None, 10).is_err());
    assert_eq!(std::fs::read(&outside).unwrap(), b"{}\n");
    let _ = std::fs::remove_dir_all(root_path);
    let _ = std::fs::remove_file(outside);
}

#[test]
fn orphan_sweep_retains_unrelated_and_unverifiable_directories() {
    let root_path = std::env::temp_dir().join(format!(
        "syncdash-runlog-orphan-root-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root_path);
    std::fs::create_dir_all(root_path.join("unrelated-project")).unwrap();
    let malformed_id = "20260101-000000-000-job-apply-123-1";
    std::fs::create_dir_all(root_path.join(malformed_id)).unwrap();
    std::fs::write(root_path.join(malformed_id).join(SUMMARY_FILE), b"not-json").unwrap();
    let mismatched_id = "20260101-000000-000-job-apply-123-2";
    std::fs::create_dir_all(root_path.join(mismatched_id)).unwrap();
    let mismatch = pending_record(
        RECORD_A,
        &test_subject("job"),
        RunKind::Apply,
        1,
        malformed_id,
        0,
    );
    std::fs::write(
        root_path.join(mismatched_id).join(SUMMARY_FILE),
        serde_json::to_vec(&mismatch).unwrap(),
    )
    .unwrap();
    let root = LocalRoot::open(root_path.clone()).unwrap();
    let live = std::collections::HashSet::new();

    sweep_orphans(
        &root,
        &live,
        crate::foundation::time::now_ms() as i64 + 1_000,
    )
    .unwrap();

    assert!(root_path.join("unrelated-project").is_dir());
    assert!(root_path.join(malformed_id).is_dir());
    assert!(root_path.join(mismatched_id).is_dir());
    let _ = std::fs::remove_dir_all(root_path);
}

#[cfg(unix)]
#[test]
fn size_measurement_failure_aborts_before_any_retention_delete() {
    use std::os::unix::fs::symlink;

    let root_path =
        std::env::temp_dir().join(format!("syncdash-runlog-size-root-{}", std::process::id()));
    let outside = std::env::temp_dir().join(format!(
        "syncdash-runlog-size-outside-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root_path);
    let _ = std::fs::remove_dir_all(&outside);
    std::fs::create_dir_all(&root_path).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    let first_id = "20200101-000000-000-job-apply-123-1";
    let redirected_id = "20200101-000000-000-job-apply-123-2";
    std::fs::create_dir_all(root_path.join(first_id)).unwrap();
    std::fs::write(root_path.join(first_id).join(RUNLOG_RUN_FILE), b"keep").unwrap();
    std::fs::write(outside.join("sentinel"), b"outside").unwrap();
    symlink(&outside, root_path.join(redirected_id)).unwrap();
    let records = [
        pending_record(
            RECORD_A,
            &test_subject("job"),
            RunKind::Apply,
            1,
            first_id,
            0,
        ),
        pending_record(
            RECORD_B,
            &test_subject("job"),
            RunKind::Apply,
            1,
            redirected_id,
            0,
        ),
    ];
    let index = records
        .iter()
        .map(|record| format!("{}\n", serde_json::to_string(record).unwrap()))
        .collect::<String>();
    std::fs::write(root_path.join(INDEX_FILE), index).unwrap();
    let root = LocalRoot::open(root_path.clone()).unwrap();

    assert!(prune_at(&root, 1, 1).is_err());
    assert_eq!(
        std::fs::read(root_path.join(first_id).join(RUNLOG_RUN_FILE)).unwrap(),
        b"keep"
    );
    assert_eq!(std::fs::read(outside.join("sentinel")).unwrap(), b"outside");
    let _ = std::fs::remove_dir_all(root_path);
    let _ = std::fs::remove_dir_all(outside);
}
