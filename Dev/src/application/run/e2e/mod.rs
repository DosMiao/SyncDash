//! Focused end-to-end checks for the scan → compare → apply boundary.

pub mod corpus;
pub mod tree;

mod archive_tier;
mod guards;
mod lanes;
mod unicode;

use std::sync::Arc;

use crate::fs::vfs::Vfs;
use crate::job::Job;
use crate::model::plan::{Action, Op};
use crate::obs::progress::{ApplyOutcome, RunCtl, RunCtx};

use corpus::Seed;

#[derive(Clone, Default)]
pub struct Transcript(Arc<std::sync::Mutex<Vec<String>>>);

impl Transcript {
    pub fn sink(&self) -> Arc<dyn crate::obs::progress::ProgressSink> {
        let messages = self.0.clone();
        Arc::new(move |event: crate::model::event::ProgressEvent| {
            use crate::model::event::{LogLevel, ProgressEvent};
            let line = match event {
                ProgressEvent::Error {
                    action, message, ..
                } => Some(format!("error[{action}] {message}")),
                ProgressEvent::Log {
                    level,
                    scope,
                    message,
                    ..
                } if level != LogLevel::Info => Some(format!("{level:?}[{scope}] {message}")),
                _ => None,
            };
            if let Some(line) = line {
                messages.lock().unwrap().push(line);
            }
        })
    }

    pub fn text(&self) -> String {
        let messages = self.0.lock().unwrap();
        if messages.is_empty() {
            "  (the run said nothing)".to_owned()
        } else {
            format!("  {}", messages.join("\n  "))
        }
    }
}

pub fn watched() -> (Transcript, RunCtx) {
    let transcript = Transcript::default();
    let context = RunCtx::new(RunCtl::new(), transcript.sink());
    (transcript, context)
}

pub fn bare_job() -> Job {
    Job {
        mode: "mirror".into(),
        rigor: "standard".into(),
        source: String::new(),
        targets: vec![String::new()],
        archive: None,
        include: Vec::new(),
        exclude: Vec::new(),
        require_marker: false,
        min_free_pct: 0.0,
        max_delete_ratio: 0.0,
        fsync: false,
        parallel: Some(1),
        ..Job::default()
    }
}

fn executable_operations(operations: &[Op]) -> Vec<Op> {
    operations
        .iter()
        .filter(|operation| operation.action.is_executable())
        .cloned()
        .collect()
}

pub fn try_cycle(
    job: &Job,
    source: &Arc<dyn Vfs>,
    target: &Arc<dyn Vfs>,
) -> (crate::model::plan::Plan, ApplyOutcome, String) {
    let (transcript, context) = watched();
    let comparison = super::local::compare_resolved(job, source, target, &context)
        .unwrap_or_else(|error| panic!("compare: {error}\n{}", transcript.text()));
    let outcome = super::local::apply_resolved(
        job,
        &comparison.plan,
        &executable_operations(&comparison.plan.ops),
        source,
        target,
        None,
        false,
        std::time::Instant::now(),
        &context,
    );
    (comparison.plan, outcome, transcript.text())
}

pub fn cycle(job: &Job, source: &Arc<dyn Vfs>, target: &Arc<dyn Vfs>) -> crate::model::plan::Plan {
    let (plan, outcome, transcript) = try_cycle(job, source, target);
    assert_eq!(outcome.errors, 0, "apply errored\n{transcript}");
    plan
}

pub fn run_pipeline_smoke(
    lane: &str,
    source: &Arc<dyn Vfs>,
    target: &Arc<dyn Vfs>,
    trash: Option<std::path::PathBuf>,
) {
    const STAMP: i64 = 1_767_225_600_000;

    corpus::write_seed(
        target,
        Seed {
            path: "update.txt",
            seed: 1,
            size: 512,
            mtime_ms: STAMP,
        },
    );
    corpus::write_seed(
        source,
        Seed {
            path: "update.txt",
            seed: 2,
            size: 768,
            mtime_ms: STAMP + 1_000,
        },
    );
    for root in [source, target] {
        corpus::write_seed(
            root,
            Seed {
                path: "mode.sh",
                seed: 3,
                size: 128,
                mtime_ms: STAMP,
            },
        );
    }
    corpus::write_seed(
        source,
        Seed {
            path: "new.txt",
            seed: 4,
            size: 256,
            mtime_ms: STAMP,
        },
    );
    corpus::write_seed(
        source,
        Seed {
            path: "new-parent/moved.txt",
            seed: 5,
            size: 1_024,
            mtime_ms: STAMP,
        },
    );
    corpus::write_seed(
        target,
        Seed {
            path: "old-parent/old-name.txt",
            seed: 5,
            size: 1_024,
            mtime_ms: STAMP,
        },
    );
    corpus::write_seed(
        target,
        Seed {
            path: "deleted.txt",
            seed: 6,
            size: 256,
            mtime_ms: STAMP,
        },
    );

    let mut job = bare_job();
    if source.caps().unix_mode.yes() && target.caps().unix_mode.yes() {
        source.set_mode("mode.sh", 0o755).unwrap();
        target.set_mode("mode.sh", 0o644).unwrap();
        job.sync_mode = true;
    }
    if source.caps().symlink.yes() && target.caps().symlink.yes() {
        source.make_symlink("link.txt", "new.txt").unwrap();
        job.symlinks = "direct".into();
    }

    let (transcript, context) = watched();
    let comparison = super::local::compare_resolved(&job, source, target, &context)
        .unwrap_or_else(|error| panic!("[{lane}] compare: {error}\n{}", transcript.text()));
    for (action, path) in [
        (Action::Copy, "new.txt"),
        (Action::Update, "update.txt"),
        (Action::Move, "new-parent/moved.txt"),
        (Action::Delete, "deleted.txt"),
        (Action::DeleteDir, "old-parent"),
    ] {
        assert!(
            comparison
                .plan
                .ops
                .iter()
                .any(|operation| operation.action == action && operation.path == path),
            "[{lane}] missing {action:?} for {path}: {:?}",
            comparison.plan.ops
        );
    }
    if job.sync_mode {
        assert!(comparison
            .plan
            .ops
            .iter()
            .any(|operation| operation.action == Action::Chmod && operation.path == "mode.sh"));
    }
    if job.symlinks == "direct" {
        assert!(comparison
            .plan
            .ops
            .iter()
            .any(|operation| operation.action == Action::Copy && operation.path == "link.txt"));
    }

    let outcome = super::local::apply_resolved(
        &job,
        &comparison.plan,
        &executable_operations(&comparison.plan.ops),
        source,
        target,
        trash.clone(),
        false,
        std::time::Instant::now(),
        &context,
    );
    assert_eq!(outcome.errors, 0, "[{lane}] apply\n{}", transcript.text());
    assert_eq!(
        outcome.bytes_copied, 1_024,
        "[{lane}] unexpected data transfer"
    );

    let preserved = preserved_of(target, trash.as_deref());
    assert!(preserved.iter().any(|path| path == "deleted.txt"));
    assert!(preserved.iter().any(|path| path == "update.txt"));
    assert!(!preserved
        .iter()
        .any(|path| path == "old-parent/old-name.txt"));
    let tolerance = tree::Tolerance::between(source, target);
    tree::assert_same(
        &tree::shape_of(source),
        &tree::shape_of(target),
        &tolerance,
        lane,
    );

    corpus::write_seed(
        target,
        Seed {
            path: "target-extra.txt",
            seed: 7,
            size: 64,
            mtime_ms: STAMP,
        },
    );
    let enrich = Job {
        mode: "enrich".into(),
        ..bare_job()
    };
    let comparison = super::local::compare_resolved(&enrich, source, target, &context).unwrap();
    assert!(!comparison
        .plan
        .ops
        .iter()
        .any(|operation| operation.action == Action::Delete));
    assert!(target.stat("target-extra.txt").unwrap().is_some());
}

fn preserved_of(target: &Arc<dyn Vfs>, trash: Option<&std::path::Path>) -> Vec<String> {
    let mut paths = Vec::new();
    let internal = format!("{}/trash", crate::foundation::names::APP_DIR);
    if let Ok(runs) = target.read_dir(&internal) {
        for run in runs {
            collect_vfs(target, &format!("{internal}/{}", run.name), "", &mut paths);
        }
    }
    if let Some(trash) = trash {
        collect_fs(&trash.join("target"), "", &mut paths);
    }
    paths.sort();
    paths
}

fn collect_fs(directory: &std::path::Path, relative: &str, paths: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let child = if relative.is_empty() {
            name
        } else {
            format!("{relative}/{name}")
        };
        if entry.path().is_dir() {
            collect_fs(&entry.path(), &child, paths);
        } else {
            paths.push(child);
        }
    }
}

fn collect_vfs(vfs: &Arc<dyn Vfs>, absolute: &str, relative: &str, paths: &mut Vec<String>) {
    let Ok(entries) = vfs.read_dir(absolute) else {
        return;
    };
    for entry in entries {
        let child_absolute = format!("{absolute}/{}", entry.name);
        let child_relative = if relative.is_empty() {
            entry.name.as_str().to_owned()
        } else {
            format!("{relative}/{}", entry.name)
        };
        if entry.meta.kind == crate::fs::vfs::VfsEntryKind::Directory {
            collect_vfs(vfs, &child_absolute, &child_relative, paths);
        } else {
            paths.push(child_relative);
        }
    }
}
