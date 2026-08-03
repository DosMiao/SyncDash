//! Transactional preservation of content before overwrite or deletion.

use std::io::{Read, Seek};
use std::path::PathBuf;

use crate::foundation::path::{RootRelativeDir, RootRelativePath};
use crate::fs::local_root::LocalRoot;
use crate::model::plan::{Op, Side};
use crate::obs::progress::PhaseProgress;

use super::schedule::Shared;

pub(in crate::pipeline::apply) fn default_trash() -> PathBuf {
    crate::store::trash::trash_root().join(crate::foundation::time::now_ms().to_string())
}

pub(super) fn move_to_trash<F>(
    source_root: &LocalRoot,
    source: &RootRelativePath,
    retained_relative: &str,
    trash_root: &LocalRoot,
    fsync: bool,
    progress: &PhaseProgress,
    mut checkpoint: F,
) -> std::io::Result<()>
where
    F: FnMut() -> std::io::Result<()>,
{
    let destination = relative_path(retained_relative)?;
    trash_root.create_directory_all(&parent_directory(&destination))?;
    refuse_existing(trash_root, &destination)?;
    checkpoint()?;
    match source_root.rename_to_noreplace(source, trash_root, &destination) {
        Ok(()) => {
            if fsync {
                trash_root.sync_parent(&destination)?;
                source_root.sync_parent(source)?;
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::CrossesDevices => copy_to_trash(
            source_root,
            source,
            trash_root,
            &destination,
            fsync,
            progress,
            checkpoint,
        ),
        Err(error) => Err(error),
    }
}

fn refuse_existing(root: &LocalRoot, destination: &RootRelativePath) -> std::io::Result<()> {
    match root.metadata_path(destination) {
        Ok(_) => Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!(
                "refusing to overwrite an existing retained original: {}",
                destination.as_str()
            ),
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn copy_to_trash<F>(
    source_root: &LocalRoot,
    source: &RootRelativePath,
    trash_root: &LocalRoot,
    destination: &RootRelativePath,
    fsync: bool,
    progress: &PhaseProgress,
    mut checkpoint: F,
) -> std::io::Result<()>
where
    F: FnMut() -> std::io::Result<()>,
{
    let destination_label = destination.as_str();
    trash_root.create_directory_all(&parent_directory(destination))?;
    refuse_existing(trash_root, destination)?;
    let mut source_file = source_root.open_read(source)?;
    let metadata = source_file.metadata()?;
    progress.add_total_bytes(metadata.len());
    checkpoint()?;
    let mut staged = trash_root.create_staged(destination)?;
    let mut buffer = vec![0u8; 1024 * 1024];
    let mut copied = 0u64;
    let mut copied_hash = blake3::Hasher::new();
    loop {
        checkpoint()?;
        let count = source_file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        std::io::Write::write_all(&mut staged, &buffer[..count])?;
        copied = copied.checked_add(count as u64).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("trash copy byte count overflowed for '{destination_label}'"),
            )
        })?;
        copied_hash.update(&buffer[..count]);
        progress.add_bytes(count as u64, destination_label);
    }
    if copied != metadata.len() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "trash copy changed length while preserving '{destination_label}': expected {} bytes, copied {copied}",
                metadata.len()
            ),
        ));
    }
    source_file.seek(std::io::SeekFrom::Start(0))?;
    let mut current_hash = blake3::Hasher::new();
    let mut verified = 0u64;
    loop {
        checkpoint()?;
        let count = source_file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        verified = verified.checked_add(count as u64).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("trash verification byte count overflowed for '{destination_label}'"),
            )
        })?;
        current_hash.update(&buffer[..count]);
    }
    if verified != copied || current_hash.finalize() != copied_hash.finalize() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("source changed while preserving '{destination_label}'"),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        staged.set_mode(metadata.mode() & 0o7777)?;
    }
    staged.set_mtime(crate::foundation::time::systime_ms(metadata.modified()?))?;
    staged.seal(fsync)?;
    refuse_existing(trash_root, destination)?;
    checkpoint()?;
    staged.commit_noreplace()?;
    checkpoint()?;
    source_root.remove_open_file(source, &source_file)?;
    if fsync {
        source_root.sync_parent(source)?;
    }
    Ok(())
}

pub(super) fn preserve(
    shared: &Shared,
    operation: &Op,
    destination_vfs: &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
    reason: &str,
    newer: Option<(&LocalRoot, &RootRelativePath)>,
    progress: &PhaseProgress,
) -> std::io::Result<()> {
    if let Some(root) = shared.local_root_of(&operation.side) {
        let relative = relative_path(&operation.path)?;
        if shared.opt.versioning {
            shared.check_before_mutation()?;
            let slot = if operation.side == Side::Source {
                &shared.ver_source
            } else {
                &shared.ver_target
            };
            let mut writer = slot.lock().unwrap();
            if writer.is_none() {
                *writer = Some(crate::store::version::VersionWriter::begin(root, || {
                    shared.checkpoint(progress)
                })?);
            }
            return writer.as_mut().unwrap().preserve(
                &relative,
                newer,
                reason,
                shared.opt.fsync,
                || shared.checkpoint(progress),
            );
        }
        if shared.trash_reaches(&operation.side) {
            let side = operation.side.as_str();
            let retained_relative = format!("{side}/{}", operation.path);
            let trash_root = shared
                .central_trash_root
                .as_ref()
                .expect("central trash reachability opens a central trash capability");
            move_to_trash(
                root,
                &relative,
                &retained_relative,
                trash_root,
                shared.opt.fsync,
                progress,
                || shared.checkpoint(progress),
            )?;
            shared.note_central_preservation();
            return Ok(());
        }
    }

    let retained_relative = format!("{}/{}", shared.in_root_keep_rel, operation.path);
    if let Some(parent) = crate::foundation::path::parent(&retained_relative) {
        shared.ensure_dir(&operation.side, destination_vfs, parent)?;
    }
    shared.check_before_mutation()?;
    destination_vfs.rename_noreplace(&operation.path, &retained_relative)?;
    shared.note_in_root_preservation(&operation.side);
    Ok(())
}

fn relative_path(value: &str) -> std::io::Result<RootRelativePath> {
    RootRelativePath::try_from(value)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string()))
}

fn parent_directory(path: &RootRelativePath) -> RootRelativeDir {
    RootRelativeDir::try_from(crate::foundation::path::parent(path.as_str()).unwrap_or(""))
        .expect("a validated relative path has a valid parent")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::event::{Phase, ProgressEvent};
    use crate::obs::progress::{PhaseProgress, RunCtl, RunCtx};
    use std::sync::{Arc, Mutex};

    fn roots(tag: &str) -> (PathBuf, LocalRoot, LocalRoot) {
        let base =
            std::env::temp_dir().join(format!("syncdash-preserve-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let source_path = base.join("source");
        let trash_path = base.join("trash");
        let source = LocalRoot::create(source_path).unwrap();
        let trash = LocalRoot::create(trash_path).unwrap();
        (base, source, trash)
    }

    #[test]
    fn preservation_reports_bytes_and_lands_atomically() {
        let (base, source, trash) = roots("progress");
        let relative = RootRelativePath::try_from("old.bin").unwrap();
        let content = vec![7u8; 2 * 1024 * 1024 + 17];
        let mut staged = source.create_staged(&relative).unwrap();
        std::io::Write::write_all(&mut staged, &content).unwrap();
        staged.commit().unwrap();
        let events: Arc<Mutex<Vec<ProgressEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let copy = events.clone();
        let context = RunCtx::new(
            RunCtl::new(),
            Arc::new(move |event| copy.lock().unwrap().push(event)),
        );
        let progress = PhaseProgress::begin(&context, Phase::Apply, None, 1, 0);

        copy_to_trash(
            &source,
            &relative,
            &trash,
            &RootRelativePath::try_from("target/old.bin").unwrap(),
            false,
            &progress,
            || progress.checkpoint(),
        )
        .unwrap();
        progress.item_done("old.bin");
        progress.finish().unwrap();

        assert!(source.metadata_path(&relative).is_err());
        assert_eq!(
            trash
                .read(&RootRelativePath::try_from("target/old.bin").unwrap())
                .unwrap(),
            content
        );
        let events = events.lock().unwrap();
        assert!(events
            .iter()
            .any(|event| matches!(event, ProgressEvent::Totals {
            reset: false,
            bytes_total,
            ..
        } if *bytes_total == content.len() as u64)));
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn cancellation_keeps_the_original_and_no_partial_trash() {
        let (base, source, trash) = roots("cancel");
        let relative = RootRelativePath::try_from("old.bin").unwrap();
        let mut staged = source.create_staged(&relative).unwrap();
        std::io::Write::write_all(&mut staged, &vec![9u8; 2 * 1024 * 1024]).unwrap();
        staged.commit().unwrap();
        let control = RunCtl::new();
        control.request_cancel();
        let context = RunCtx::new(control, Arc::new(|_| {}));
        let progress = PhaseProgress::begin(&context, Phase::Apply, None, 1, 0);
        let destination = RootRelativePath::try_from("target/old.bin").unwrap();

        let error = copy_to_trash(
            &source,
            &relative,
            &trash,
            &destination,
            false,
            &progress,
            || progress.checkpoint(),
        )
        .unwrap_err();
        assert!(crate::obs::progress::is_cancelled(&error));
        assert!(source.metadata_path(&relative).is_ok());
        assert!(trash.metadata_path(&destination).is_err());
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn a_source_change_during_cross_volume_preservation_is_not_removed() {
        let (base, source, trash) = roots("source-change");
        let relative = RootRelativePath::try_from("old.bin").unwrap();
        let destination = RootRelativePath::try_from("target/old.bin").unwrap();
        std::fs::write(source.display_path().join("old.bin"), b"original").unwrap();
        let context = RunCtx::new(RunCtl::new(), Arc::new(|_| {}));
        let progress = PhaseProgress::begin(&context, Phase::Apply, None, 1, 0);
        let source_path = source.display_path().join("old.bin");
        let mut checkpoints = 0;

        let error = copy_to_trash(
            &source,
            &relative,
            &trash,
            &destination,
            false,
            &progress,
            || {
                checkpoints += 1;
                if checkpoints == 4 {
                    std::fs::write(&source_path, b"modified").unwrap();
                }
                Ok(())
            },
        )
        .unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(source.read(&relative).unwrap(), b"modified");
        assert!(trash.metadata_path(&destination).is_err());
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn an_existing_retained_original_is_never_replaced() {
        let (base, source, trash) = roots("collision");
        let relative = RootRelativePath::try_from("old.bin").unwrap();
        let destination = RootRelativePath::try_from("target/old.bin").unwrap();
        trash
            .create_directory_all(&RootRelativeDir::try_from("target").unwrap())
            .unwrap();
        for (root, path, bytes) in [
            (&source, &relative, b"second original".as_slice()),
            (&trash, &destination, b"first original".as_slice()),
        ] {
            let mut staged = root.create_staged(path).unwrap();
            std::io::Write::write_all(&mut staged, bytes).unwrap();
            staged.commit().unwrap();
        }
        let context = RunCtx::new(RunCtl::new(), Arc::new(|_| {}));
        let progress = PhaseProgress::begin(&context, Phase::Apply, None, 1, 0);

        let error = move_to_trash(
            &source,
            &relative,
            "target/old.bin",
            &trash,
            false,
            &progress,
            || progress.checkpoint(),
        )
        .unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(source.read(&relative).unwrap(), b"second original");
        assert_eq!(trash.read(&destination).unwrap(), b"first original");
        let _ = std::fs::remove_dir_all(base);
    }
}
