//! The sync-mode archive: the record of what the two sides last agreed on.
//!
//! Refreshed after a successful apply, and never over paths that ended in conflict — an archive
//! that claims agreement where there was none turns the next run's conflict into a silent
//! overwrite.

use crate::job::Job;
use crate::model::digest::Blake3Digest;
use crate::model::plan::{Action, Plan};
use crate::model::table::TableArtifact;
use crate::pipeline::scan;
use std::io::{self, Write};
use std::path::Path;

use crate::foundation::path::RootRelativePath;
use crate::fs::local_root::LocalRoot;

struct ArchiveTarget {
    parent: LocalRoot,
    relative: RootRelativePath,
}

struct ArchiveLock {
    _file: std::fs::File,
}

#[derive(serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ArchiveMigrationReceipt {
    schema: String,
    version: u32,
    archive: RootRelativePath,
    backup: RootRelativePath,
    source_blake3: Blake3Digest,
    target_blake3: Blake3Digest,
}

impl ArchiveTarget {
    fn open_for_read(destination: &Path) -> io::Result<Option<Self>> {
        let (parent_path, relative) = archive_location(destination)?;
        let parent = match LocalRoot::open(parent_path.to_path_buf()) {
            Ok(parent) => parent,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let target = Self { parent, relative };
        if target.validate_existing(destination)? {
            Ok(Some(target))
        } else {
            Ok(None)
        }
    }

    fn open_for_write(destination: &Path) -> io::Result<Self> {
        let (parent_path, relative) = archive_location(destination)?;
        std::fs::create_dir_all(parent_path)?;
        let target = Self {
            parent: LocalRoot::open(parent_path.to_path_buf())?,
            relative,
        };
        target.validate_existing(destination)?;
        Ok(target)
    }

    fn validate_existing(&self, destination: &Path) -> io::Result<bool> {
        match self.parent.metadata_path(&self.relative) {
            Ok(metadata) if metadata.is_file() => Ok(true),
            Ok(_) => Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!(
                    "archive path is not a regular file: {}",
                    destination.display()
                ),
            )),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }

    fn acquire_lock(&self) -> io::Result<ArchiveLock> {
        let file = self.parent.open_lock_file(&self.migration_path("lock")?)?;
        file.lock()?;
        Ok(ArchiveLock { _file: file })
    }

    fn migration_path(&self, suffix: &str) -> io::Result<RootRelativePath> {
        let digest = Blake3Digest::hash_bytes(self.relative.as_str().as_bytes());
        RootRelativePath::try_from(format!(
            ".syncdash.archive-migration.{}.{}",
            &digest.as_str()[..16],
            suffix
        ))
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error.to_string()))
    }

    fn load_current(&self) -> io::Result<Option<TableArtifact>> {
        match self.parent.open_read(&self.relative) {
            Ok(file) => TableArtifact::read_archive(io::BufReader::new(file)).map(Some),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn load_or_migrate(&self, _lock: &ArchiveLock) -> io::Result<Option<TableArtifact>> {
        let format = match self.parent.open_read(&self.relative) {
            Ok(file) => crate::model::table::migrate::classify_archive(io::BufReader::new(file))?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        if format == crate::model::table::migrate::ArchiveFormat::Current {
            return self.load_current();
        }

        let source_blake3 = hash_file(&self.parent, &self.relative)?;
        let migrated = {
            let file = self.parent.open_read(&self.relative)?;
            crate::model::table::migrate::migrate_v1_archive(io::BufReader::new(file))?
        };
        let mut target_bytes = Vec::new();
        migrated.write_to(&mut target_bytes)?;
        let target_blake3 = Blake3Digest::hash_bytes(&target_bytes);
        let backup = self.migration_path("v1.backup")?;
        publish_immutable_copy(&self.parent, &self.relative, &backup, &source_blake3)?;
        let receipt_path = self.migration_path("prepared.json")?;
        let receipt = ArchiveMigrationReceipt {
            schema: "syncdash.archive-migration".into(),
            version: 1,
            archive: self.relative.clone(),
            backup,
            source_blake3,
            target_blake3: target_blake3.clone(),
        };
        publish_receipt(&self.parent, &receipt_path, &receipt)?;
        if hash_file(&self.parent, &self.relative)? != receipt.source_blake3 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "archive changed after its migration was prepared; refusing replacement",
            ));
        }
        let mut staged = self.parent.create_staged(&self.relative)?;
        staged.write_all(&target_bytes)?;
        staged.seal(true)?;
        if let Err(error) = staged.commit() {
            match hash_file(&self.parent, &self.relative) {
                Ok(actual) if actual == receipt.target_blake3 => {}
                _ => return Err(error),
            }
        }
        self.load_current()
    }
}

pub fn load_archive(destination: &Path) -> io::Result<Option<TableArtifact>> {
    match ArchiveTarget::open_for_read(destination)? {
        Some(target) => {
            let lock = target.acquire_lock()?;
            target.load_or_migrate(&lock)
        }
        None => Ok(None),
    }
}

fn hash_file(root: &LocalRoot, relative: &RootRelativePath) -> io::Result<Blake3Digest> {
    use std::io::Read as _;
    let mut file = root.open_read(relative)?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(Blake3Digest::from_hash(hasher.finalize()))
}

fn publish_immutable_copy(
    root: &LocalRoot,
    source: &RootRelativePath,
    destination: &RootRelativePath,
    expected: &Blake3Digest,
) -> io::Result<()> {
    let mut source_file = root.open_read(source)?;
    let mut staged = root.create_staged(destination)?;
    staged.write_all_from(&mut source_file)?;
    staged.seal(true)?;
    match staged.commit_noreplace() {
        Ok(()) => {}
        Err(_error) if hash_file(root, destination).ok().as_ref() == Some(expected) => {}
        Err(error) => return Err(error),
    }
    let actual = hash_file(root, destination)?;
    if &actual != expected {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "immutable archive migration backup does not match its source",
        ));
    }
    Ok(())
}

fn publish_receipt(
    root: &LocalRoot,
    path: &RootRelativePath,
    expected: &ArchiveMigrationReceipt,
) -> io::Result<()> {
    let mut bytes = serde_json::to_vec(expected).map_err(io::Error::other)?;
    bytes.push(b'\n');
    let expected_hash = Blake3Digest::hash_bytes(&bytes);
    let mut staged = root.create_staged(path)?;
    staged.write_all(&bytes)?;
    staged.seal(true)?;
    match staged.commit_noreplace() {
        Ok(()) => {}
        Err(_error) if hash_file(root, path).ok().as_ref() == Some(&expected_hash) => {}
        Err(error) => return Err(error),
    }
    let stored: ArchiveMigrationReceipt =
        serde_json::from_slice(&root.read(path)?).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid archive migration receipt: {error}"),
            )
        })?;
    if &stored != expected {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "archive migration receipt does not match the prepared migration",
        ));
    }
    Ok(())
}

#[cfg(test)]
fn write_archive_atomic(
    dst: &Path,
    write_snapshot: impl FnOnce(&mut dyn Write) -> io::Result<()>,
    before_commit: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    let target = ArchiveTarget::open_for_write(dst)?;
    let lock = target.acquire_lock()?;
    write_archive_to(&target, &lock, write_snapshot, before_commit)
}

fn write_archive_to(
    target: &ArchiveTarget,
    _lock: &ArchiveLock,
    write_snapshot: impl FnOnce(&mut dyn Write) -> io::Result<()>,
    before_commit: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    let mut staged = target.parent.create_staged(&target.relative)?;
    {
        let mut writer = io::BufWriter::new(&mut staged);
        write_snapshot(&mut writer)?;
        writer.flush()?;
    }
    staged.seal(true)?;
    before_commit()?;
    staged.commit()
}

fn archive_location(destination: &Path) -> io::Result<(&Path, RootRelativePath)> {
    let parent = destination
        .parent()
        .filter(|directory| !directory.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "archive path has no UTF-8 file name",
            )
        })?;
    let relative = RootRelativePath::try_from(name)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error.to_string()))?;
    Ok((parent, relative))
}

/// Refreshes the archive from the already-open source after a successful sync.
///
/// Conflicted paths remain absent so the next run reports them again. `opt` must be the effective
/// comparison options because archive digests are only comparable within the same evidence tier.
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
        // The generation chain distinguishes one-generation lag from concurrent modification.
        let archive = ArchiveTarget::open_for_write(arch_path)?;
        let lock = archive.acquire_lock()?;
        let previous = archive.load_or_migrate(&lock)?;
        snap.header.kind = crate::model::table::TableKind::Archive;
        snap.entries
            .retain(|entry| !conflicted.contains(entry.path().as_str()));
        snap.header.entry_count = snap.entries.len() as u64;
        if let Some(prev) = &previous {
            crate::model::table::roll_generations(&mut snap.entries, &prev.entries);
        }
        write_archive_to(
            &archive,
            &lock,
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
