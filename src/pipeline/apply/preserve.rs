//! Where the previous content goes before it is overwritten or deleted.
//!
//! A root uses the central trash only when it can rename into that store. Mounted shares, protocol
//! roots, and external local volumes preserve by rename inside themselves. Versioning, when
//! enabled, layers on top of both.

use crate::foundation::path::join_native;
use std::path::{Path, PathBuf};

use crate::model::plan::{Op, Side};
use crate::obs::progress::PhaseProgress;
use super::schedule::Shared;

pub(super) fn default_trash() -> PathBuf {
    crate::store::trash::trash_root().join(crate::foundation::time::now_ms().to_string())
}

pub(super) fn move_to_trash(
    file: &Path,
    rel: &str,
    trash: &Path,
    fsync: bool,
    pp: &PhaseProgress,
) -> std::io::Result<()> {
    let dest = join_native(trash, rel);
    if let Some(p) = dest.parent() {
        std::fs::create_dir_all(p)?;
    }
    refuse_existing(&dest, rel)?;
    match crate::fs::rename_force(file, &dest) {
        Ok(_) => Ok(()),
        // Cross-volume: stage a cancellable copy beside the trash destination, then remove the
        // original only after the complete copy lands.
        Err(_) => copy_to_trash(file, &dest, rel, fsync, pp),
    }
}

fn refuse_existing(dest: &Path, rel: &str) -> std::io::Result<()> {
    match std::fs::symlink_metadata(dest) {
        Ok(_) => Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("refusing to overwrite an existing retained original: {rel}"),
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn copy_to_trash(
    file: &Path,
    dest: &Path,
    rel: &str,
    fsync: bool,
    pp: &PhaseProgress,
) -> std::io::Result<()> {
    refuse_existing(dest, rel)?;
    let metadata = std::fs::metadata(file)?;
    pp.add_total_bytes(metadata.len());
    pp.checkpoint()?;
    let mut staged = crate::fs::staged::Staged::create(dest)?;
    let copied = staged.copy_from(file, &mut |chunk| {
        pp.checkpoint()?;
        pp.add_bytes(chunk.len() as u64, rel);
        Ok(())
    })?;
    if copied != metadata.len() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("trash copy changed length while preserving '{rel}': expected {} bytes, copied {copied}", metadata.len()),
        ));
    }
    staged.seal(fsync)?;
    std::fs::set_permissions(staged.path(), metadata.permissions())?;
    refuse_existing(dest, rel)?;
    staged.commit()?;
    crate::fs::remove_file_force(file)
}

/// Archive the original before it is overwritten/deleted (trash or .version_syncDash).
///
/// Three routes, chosen by two independent questions:
///
/// - **Is there a real path?** (`local_of`) — the rdelta version store needs one. It writes into
///   `<root>/.version_syncDash/`, so it stays a move *within* the root and is safe anywhere a
///   path exists, share or not.
/// - **Can the configured trash path take it?** A local root and that actual path must be on the
///   same device. A move into it from a share or another local volume is cross-volume, and
///   `move_to_trash` answers a failed rename by copying every byte before removal. Those roots take
///   the in-root retention area instead, which is the same rename a genuinely remote root gets.
pub(super) fn preserve(
    sh: &Shared,
    op: &Op,
    exec: &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
    why: &str,
    newer: Option<&Path>,
    pp: &PhaseProgress,
) -> std::io::Result<()> {
    if let Some(root) = sh.local_of(&op.side) {
        let dst = join_native(root, &op.path);
        if sh.opt.versioning {
            let slot = if op.side == Side::Source { &sh.ver_source } else { &sh.ver_target };
            let mut w = slot.lock().unwrap();
            if w.is_none() {
                *w = Some(crate::store::version::VersionWriter::begin(root)?);
            }
            return w.as_mut().unwrap().preserve(&op.path, &dst, newer, why);
        }
        if sh.trash_reaches(&op.side) {
            let side = if op.side == Side::Source {
                "source"
            } else {
                "target"
            };
            let retained_rel = format!("{side}/{}", op.path);
            move_to_trash(&dst, &retained_rel, &sh.trash, sh.opt.fsync, pp)?;
            sh.note_central_preservation();
            return Ok(());
        }
    }
    // A root the central store cannot reach — external local volume, mounted share, or protocol
    // backend — keeps the original under itself, so preservation remains a same-root rename.
    let keep_rel = format!("{}/{}", sh.in_root_keep_rel, op.path);
    if exec.stat(&keep_rel)?.is_some() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("refusing to overwrite an existing retained original: {keep_rel}"),
        ));
    }
    if let Some(parent) = crate::foundation::path::parent(&keep_rel) {
        sh.ensure_dir(&op.side, exec, parent)?;
    }
    exec.rename(&op.path, &keep_rel)?;
    sh.note_in_root_preservation(&op.side);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::event::{Phase, ProgressEvent};
    use crate::obs::progress::{PhaseProgress, RunCtl, RunCtx};
    use std::sync::{Arc, Mutex};

    fn root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("syncdash-preserve-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn fallback_copy_reports_bytes_and_lands_atomically() {
        let dir = root("progress");
        let src = dir.join("old.bin");
        let dest = dir.join("trash/old.bin");
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        let content = vec![7u8; 2 * 1024 * 1024 + 17];
        std::fs::write(&src, &content).unwrap();
        let events: Arc<Mutex<Vec<ProgressEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let copy = events.clone();
        let ctx = RunCtx::new(RunCtl::new(), Arc::new(move |ev| copy.lock().unwrap().push(ev)));
        let pp = PhaseProgress::begin(&ctx, Phase::Apply, None, 1, 0);

        copy_to_trash(&src, &dest, "old.bin", false, &pp).unwrap();
        pp.item_done("old.bin");
        pp.finish().unwrap();

        assert!(!src.exists());
        assert_eq!(std::fs::read(&dest).unwrap(), content);
        let events = events.lock().unwrap();
        assert!(events.iter().any(|ev| matches!(ev, ProgressEvent::Totals {
            reset: false,
            bytes_total,
            ..
        } if *bytes_total == content.len() as u64)));
        assert!(matches!(events.last(), Some(ProgressEvent::PhaseEnd {
            status: crate::model::event::PhaseStatus::Completed,
            bytes_done,
            bytes_total,
            ..
        }) if *bytes_done == content.len() as u64 && *bytes_total == content.len() as u64));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cancelled_fallback_keeps_the_original_and_no_partial_trash() {
        let dir = root("cancel");
        let src = dir.join("old.bin");
        let dest = dir.join("trash/old.bin");
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::fs::write(&src, vec![9u8; 2 * 1024 * 1024]).unwrap();
        let ctl = RunCtl::new();
        ctl.request_cancel();
        let ctx = RunCtx::new(ctl, Arc::new(|_| {}));
        let pp = PhaseProgress::begin(&ctx, Phase::Apply, None, 1, 0);

        let err = copy_to_trash(&src, &dest, "old.bin", false, &pp).unwrap_err();
        assert!(crate::obs::progress::is_cancelled(&err));
        assert!(src.exists());
        assert!(!dest.exists());
        assert!(std::fs::read_dir(dest.parent().unwrap()).unwrap().next().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_existing_retained_original_is_never_replaced() {
        let dir = root("collision");
        let src = dir.join("old.bin");
        let trash = dir.join("trash");
        let dest = trash.join("target/old.bin");
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::fs::write(&src, b"second original").unwrap();
        std::fs::write(&dest, b"first original").unwrap();
        let ctx = RunCtx::new(RunCtl::new(), Arc::new(|_| {}));
        let pp = PhaseProgress::begin(&ctx, Phase::Apply, None, 1, 0);

        let error = move_to_trash(&src, "target/old.bin", &trash, false, &pp).unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&src).unwrap(), b"second original");
        assert_eq!(std::fs::read(&dest).unwrap(), b"first original");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
