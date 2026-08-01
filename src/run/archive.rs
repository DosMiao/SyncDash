//! The sync-mode archive: the record of what the two sides last agreed on.
//!
//! Refreshed after a successful apply, and never over paths that ended in conflict — an archive
//! that claims agreement where there was none turns the next run's conflict into a silent
//! overwrite.

use crate::job::Job;
use crate::model::plan::{Action, Plan};
use crate::model::table::Snapshot;
use crate::pipeline::scan;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::foundation::names::TEMP_PREFIX;

static ARCHIVE_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct ArchiveTemp {
    path: PathBuf,
    committed: bool,
}

impl ArchiveTemp {
    fn create(dst: &Path) -> io::Result<(Self, std::fs::File)> {
        let dir = archive_parent(dst);
        let base = dst
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "archive path has no UTF-8 file name",
                )
            })?;
        for _ in 0..16 {
            let sequence = ARCHIVE_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = dir.join(format!(
                "{TEMP_PREFIX}{base}.archive.{}.{}",
                std::process::id(),
                sequence
            ));
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(file) => {
                    return Ok((
                        ArchiveTemp {
                            path,
                            committed: false,
                        },
                        file,
                    ))
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate a unique archive staging file",
        ))
    }
}

impl Drop for ArchiveTemp {
    fn drop(&mut self) {
        if !self.committed {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

fn archive_parent(dst: &Path) -> &Path {
    dst.parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

#[cfg(windows)]
fn archive_backup_path(dst: &Path) -> io::Result<PathBuf> {
    let base = dst
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "archive path has no UTF-8 file name",
            )
        })?;
    Ok(archive_parent(dst).join(format!("{TEMP_PREFIX}{base}.archive-backup")))
}

#[cfg(windows)]
fn recover_archive_target(dst: &Path) -> io::Result<()> {
    let backup = archive_backup_path(dst)?;
    match (dst.exists(), backup.exists()) {
        (false, true) => std::fs::rename(backup, dst),
        (true, true) => std::fs::remove_file(backup),
        _ => Ok(()),
    }
}

#[cfg(not(windows))]
fn recover_archive_target(_dst: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(windows)]
fn replace_archive(temp: &Path, dst: &Path) -> io::Result<()> {
    if !dst.exists() {
        return std::fs::rename(temp, dst);
    }
    let backup = archive_backup_path(dst)?;
    if backup.exists() {
        std::fs::remove_file(&backup)?;
    }
    std::fs::rename(dst, &backup)?;
    match std::fs::rename(temp, dst) {
        Ok(()) => {
            let _ = std::fs::remove_file(backup);
            Ok(())
        }
        Err(commit_error) => match std::fs::rename(&backup, dst) {
            Ok(()) => Err(commit_error),
            Err(restore_error) => Err(io::Error::new(
                commit_error.kind(),
                format!(
                    "archive replacement failed: {commit_error}; restoring the previous archive also failed: {restore_error}"
                ),
            )),
        },
    }
}

#[cfg(not(windows))]
fn replace_archive(temp: &Path, dst: &Path) -> io::Result<()> {
    std::fs::rename(temp, dst)
}

fn write_archive_atomic(
    dst: &Path,
    write_snapshot: impl FnOnce(&mut dyn Write) -> io::Result<()>,
    before_commit: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    let dir = archive_parent(dst);
    std::fs::create_dir_all(dir)?;
    recover_archive_target(dst)?;
    if dst.exists() && !dst.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("archive path is not a file: {}", dst.display()),
        ));
    }

    let (mut temp, file) = ArchiveTemp::create(dst)?;
    let mut writer = io::BufWriter::new(file);
    write_snapshot(&mut writer)?;
    writer.flush()?;
    let file = writer.into_inner().map_err(|e| e.into_error())?;
    file.sync_all()?;
    drop(file);
    before_commit()?;
    replace_archive(&temp.path, dst)?;
    temp.committed = true;
    Ok(())
}

/// Refresh the archive after a successful sync: rescan source, drop conflicted paths (a conflict keeps being reported, never silently arbitrated).
/// v0.9 M1: make the Refresh phase visible — the archive rescan is a long phase that is completely invisible today, so wire it to the event stream and cancellation.
/// Being cancelled only means conflicts get re-reported next round — safe.
///
/// Takes the **already-open** source root rather than re-opening `job.source`. Re-resolving here
/// paid for a second full handshake on every sync run to an sftp or smb root, for no reason beyond
/// the handle having been dropped across a call boundary.
///
/// `opt` is the caller's, and must be the options the comparison actually ran at — not
/// `scan_opts(job)`. The archive exists to be compared against those digests, so it has to be
/// written in the same evidence tier; when this recomputed the tier from the job instead, an
/// asymmetric pair of roots (a source that can do ranged reads, a target that cannot) wrote a
/// sampled archive that the next full-tier comparison could never match.
pub fn refresh_archive_with(
    job: &Job,
    plan: &Plan,
    sv: &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
    opt: &scan::ScanOptions,
    ctx: &crate::obs::progress::RunCtx,
) -> bool {
    let Some(arch_path) = &job.archive else {
        ctx.log(
            crate::model::event::LogLevel::Warn,
            "run",
            "hint: sync job without `archive` — add one so deletions/moves can be attributed next time",
        );
        return true;
    };
    let conflicted: std::collections::HashSet<&str> = plan
        .ops
        .iter()
        .filter(|o| o.action == Action::Conflict)
        .map(|o| o.path.as_str())
        .collect();
    let mut snap = match scan::scan_root(sv, opt, ctx, crate::model::event::Phase::Refresh) {
        Ok(snap) => snap,
        Err(e) => {
            if crate::obs::progress::is_cancelled(&e) && ctx.ctl.cancelled() {
                return false;
            }
            ctx.sink.emit(crate::model::event::ProgressEvent::Error {
                phase: crate::model::event::Phase::Refresh,
                ts_ms: crate::foundation::time::now_ms(),
                path: arch_path.display().to_string(),
                action: "archive-scan".into(),
                side: "source".into(),
                message: e.to_string(),
            });
            return false;
        }
    };
    let saved = (|| -> std::io::Result<()> {
        let pp = crate::obs::progress::PhaseProgress::begin(
            ctx,
            crate::model::event::Phase::Archive,
            Some(arch_path.display().to_string()),
            1,
            0,
        );
        pp.checkpoint()?;
        // The previous-generation archive: every row of the new table pushes the old hash onto the prev chain, so that
        // "one generation behind" can be told apart from "concurrent modification" (P1-3, see compare::generation_of)
        recover_archive_target(arch_path)?;
        let previous = if arch_path.is_file() {
            Some(Snapshot::load(arch_path)?)
        } else {
            None
        };
        snap.header.kind = "archive".into();
        snap.entries
            .retain(|e| !conflicted.contains(e.path.as_str()));
        if let Some(prev) = &previous {
            crate::model::table::roll_generations(&mut snap.entries, &prev.entries);
        }
        write_archive_atomic(
            arch_path,
            |writer| snap.write_to(writer),
            || pp.checkpoint(),
        )?;
        ctx.log(
            crate::model::event::LogLevel::Info,
            "run",
            format!("archive refreshed: {}", arch_path.display()),
        );
        pp.item_done(&arch_path.display().to_string());
        pp.finish()?;
        Ok(())
    })();
    if let Err(e) = saved {
        if crate::obs::progress::is_cancelled(&e) && ctx.ctl.cancelled() {
            return false;
        }
        ctx.sink.emit(crate::model::event::ProgressEvent::Error {
            phase: crate::model::event::Phase::Archive,
            ts_ms: crate::foundation::time::now_ms(),
            path: arch_path.display().to_string(),
            action: "archive".into(),
            side: "source".into(),
            message: e.to_string(),
        });
        return false;
    }
    !ctx.ctl.cancelled()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::vfs::memory::MemVfs;
    use crate::fs::vfs::Vfs;
    use crate::model::event::ProgressEvent;
    use crate::obs::progress::{RunCtl, RunCtx};
    use std::sync::{Arc, Mutex};

    #[test]
    fn archive_replacement_keeps_the_previous_file_until_commit() {
        let dir =
            std::env::temp_dir().join(format!("syncdash-archive-atomic-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let archive = dir.join("archive.jsonl");
        std::fs::write(&archive, b"previous snapshot\n").unwrap();

        let failed = write_archive_atomic(
            &archive,
            |writer| {
                writer.write_all(b"partial replacement\n")?;
                Err(io::Error::new(
                    io::ErrorKind::Other,
                    "simulated write failure",
                ))
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

    #[test]
    fn archive_write_failure_is_reported_to_the_run() {
        let source = Arc::new(MemVfs::new("archive-failure-source")) as Arc<dyn Vfs>;
        let target = Arc::new(MemVfs::new("archive-failure-target")) as Arc<dyn Vfs>;
        let mut job = Job::default();
        let dir =
            std::env::temp_dir().join(format!("syncdash-archive-failure-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        job.archive = Some(dir.clone()); // File::create on a directory must fail on every platform.
        let plan =
            super::super::local::compare_resolved(&job, &source, &target, &RunCtx::null(), false)
                .unwrap()
                .plan;
        let events: Arc<Mutex<Vec<ProgressEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let copy = events.clone();
        let ctx = RunCtx::new(
            RunCtl::new(),
            Arc::new(move |ev| copy.lock().unwrap().push(ev)),
        );

        assert!(!refresh_archive_with(
            &job,
            &plan,
            &source,
            &super::super::scan_opts(&job),
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
        let dir =
            std::env::temp_dir().join(format!("syncdash-archive-cancel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        job.archive = Some(dir.join("archive.jsonl"));
        let plan =
            super::super::local::compare_resolved(&job, &source, &target, &RunCtx::null(), false)
                .unwrap()
                .plan;
        let events: Arc<Mutex<Vec<ProgressEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let copy = events.clone();
        let ctl = RunCtl::new();
        ctl.request_cancel();
        let ctx = RunCtx::new(ctl, Arc::new(move |ev| copy.lock().unwrap().push(ev)));

        assert!(!refresh_archive_with(
            &job,
            &plan,
            &source,
            &super::super::scan_opts(&job),
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
        let plan =
            super::super::local::compare_resolved(&job, &source, &target, &RunCtx::null(), false)
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
            &super::super::scan_opts(&job),
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
}
