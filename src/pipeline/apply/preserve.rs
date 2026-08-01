//! Where the previous content goes before it is overwritten or deleted.
//!
//! Local roots use the trash store; a remote root cannot, so its preserve area is a rename inside
//! the root itself. Versioning, when enabled, layers on top of both.

use std::path::{Path, PathBuf};
use crate::foundation::path::join_native;

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
    match crate::fs::rename_force(file, &dest) {
        Ok(_) => Ok(()),
        // Cross-volume: stage a cancellable copy beside the trash destination, then remove the
        // original only after the complete copy lands.
        Err(_) => copy_to_trash(file, &dest, rel, fsync, pp),
    }
}

fn copy_to_trash(
    file: &Path,
    dest: &Path,
    rel: &str,
    fsync: bool,
    pp: &PhaseProgress,
) -> std::io::Result<()> {
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
/// - **Can the central trash store take it?** (`local_trash`) — that store lives on this machine.
///   A move into it from a share or another local disk is cross-volume, and `move_to_trash`
///   answers a failed rename by copying every byte before removal. Those roots take the in-root
///   retention area instead, which is the same rename a genuinely remote root gets.
///
/// The two used to be one test, and the second question was never asked — `VfsCaps::local_trash`
/// was set correctly by the SMB backend and read by nobody.
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
            return move_to_trash(&dst, &op.path, &sh.trash, sh.opt.fsync, pp);
        }
    }
    // Off-machine root — mounted share or protocol backend alike: rename into
    // <root>/.syncdash/trash/<run_ms>/<rel>, on the far side, nothing transferred.
    let keep_rel = format!("{}/{}", sh.remote_keep_rel, op.path);
    if let Some(parent) = crate::foundation::path::parent(&keep_rel) {
        sh.ensure_dir(&op.side, exec, parent)?;
    }
    exec.rename(&op.path, &keep_rel)?;
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
}
