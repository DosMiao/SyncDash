//! Where the previous content goes before it is overwritten or deleted.
//!
//! Local roots use the trash store; a remote root cannot, so its preserve area is a rename inside
//! the root itself. Versioning, when enabled, layers on top of both.

use std::path::{Path, PathBuf};
use crate::foundation::path::join_native;

use crate::model::plan::{Op, Side};
use super::schedule::Shared;

pub(super) fn default_trash() -> PathBuf {
    crate::store::trash::trash_root().join(crate::foundation::time::now_ms().to_string())
}

pub(super) fn move_to_trash(file: &Path, rel: &str, trash: &Path) -> std::io::Result<()> {
    let dest = join_native(trash, rel);
    if let Some(p) = dest.parent() {
        std::fs::create_dir_all(p)?;
    }
    match crate::fs::rename_force(file, &dest) {
        Ok(_) => Ok(()),
        Err(_) => {
            // Cross-volume: fall back to copy+delete (force: git objects are read-only)
            std::fs::copy(file, &dest)?;
            crate::fs::remove_file_force(file)
        }
    }
}

/// Archive the original before it is overwritten/deleted (trash or .version_syncDash).
///
/// Three routes, chosen by two independent questions:
///
/// - **Is there a real path?** (`local_of`) — the rdelta version store needs one. It writes into
///   `<root>/.version_syncDash/`, so it stays a move *within* the root and is safe anywhere a
///   path exists, share or not.
/// - **Can the central trash store take it?** (`local_trash`) — that store lives on this machine.
///   A move into it from a network share is cross-volume, and `move_to_trash` answers a failed
///   rename by copying: every deleted file downloaded before it is removed. So a share takes the
///   in-root retention area instead, which is the same rename a genuinely remote root gets.
///
/// The two used to be one test, and the second question was never asked — `VfsCaps::local_trash`
/// was set correctly by the SMB backend and read by nobody.
pub(super) fn preserve(
    sh: &Shared,
    op: &Op,
    exec: &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
    why: &str,
    newer: Option<&Path>,
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
            return move_to_trash(&dst, &op.path, &sh.trash);
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
