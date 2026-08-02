//! Identity-verified, no-replace file moves with recoverable rollback paths.

use crate::fs::vfs::error::VfsErrorKind;
use crate::model::plan::Op;
use crate::obs::progress::PhaseProgress;

use super::super::file_transaction::{
    claim_move_source, copy_claimed_move, rollback_claim, sync_local_parent,
    sync_local_rename_parents, verify_move_evidence,
};
use super::super::schedule::Shared;

pub(in crate::pipeline::apply::execute) fn execute(
    sh: &Shared<'_>,
    op: &Op,
    pp: &PhaseProgress<'_>,
) -> std::io::Result<()> {
    let (exec, _) = sh.exec_other(&op.side);
    let from = op.from.as_deref().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("move operation is missing its source path: {}", op.path),
        )
    })?;

    // Claim the exact source pathname before creating a destination directory or staging
    // bytes. Later cleanup addresses only this hold, never `from`, so a new writer at the
    // original name cannot be deleted by this operation.
    let hold = claim_move_source(sh, exec.as_ref(), from, sh.opt.fsync)?;
    let publish = (|| -> std::io::Result<bool> {
        let moving = verify_move_evidence(sh, exec.as_ref(), &hold, op, pp)?;
        if exec.stat(&op.path)?.is_some() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!(
                    "move destination appeared after compare; refusing to overwrite it: {}",
                    op.path
                ),
            ));
        }
        if let Some(parent) = crate::foundation::path::parent(&op.path) {
            sh.ensure_dir(&op.side, exec, parent)?;
        }

        sh.check_before_mutation()?;
        match exec.rename_noreplace(&hold, &op.path) {
            Ok(()) => Ok(false), // the hold itself became the destination
            Err(error) if error.kind == VfsErrorKind::CrossDevice => {
                if op.link.is_some() {
                    return Err(std::io::Error::new(
                                std::io::ErrorKind::Unsupported,
                                "cross-volume symlink move cannot be published atomically without a backend no-replace symlink primitive",
                            ));
                }
                copy_claimed_move(sh, exec.as_ref(), op, &hold, &moving, pp)?;
                Ok(true) // destination published; caller must delete only the hold
            }
            Err(error) => Err(error.into()),
        }
    })();

    match publish {
                Ok(false) => {
                    // An already-open writer follows the claimed inode across rename. Recheck at
                    // its published name and put it back if it changed in the verification-to-
                    // rename window; never bless unverified bytes merely because no copy occurred.
                    if let Err(error) = verify_move_evidence(sh, exec.as_ref(), &op.path, op, pp) {
                        if sh.lease_lost() {
                            return Err(std::io::Error::other(
                                format!(
                                    "{error}; root-lock ownership was lost, so the published move was left at recoverable path '{}' instead of racing a rollback",
                                    op.path
                                ),
                            ));
                        }
                        return Err(rollback_claim(
                            exec.as_ref(),
                            &op.path,
                            from,
                            error,
                            sh.opt.fsync,
                        ));
                    }
                    sync_local_rename_parents(
                        exec.as_ref(),
                        &hold,
                        &op.path,
                        sh.opt.fsync,
                    )
                    .map_err(|error| {
                        std::io::Error::new(
                            error.kind(),
                            format!(
                                "move destination '{}' was published, but its directory entry could not be synced ({error})",
                                op.path
                            ),
                        )
                    })
                }
                Ok(true) => {
                    sh.check_before_mutation()?;
                    match exec.remove_file(&hold) {
                        Ok(()) => sync_local_parent(exec.as_ref(), &hold, sh.opt.fsync).map_err(
                            |error| {
                                std::io::Error::new(
                                    error.kind(),
                                    format!(
                                        "move destination '{}' was published and the claimed source removed, but the source directory could not be synced ({error})",
                                        op.path
                                    ),
                                )
                            },
                        ),
                        Err(error) => Err(std::io::Error::other(
                            format!(
                                "move destination '{}' was published, but the claimed source could not be removed ({error}); recoverable duplicate retained at '{hold}'",
                                op.path
                            ),
                        )),
                    }
                }
                Err(error) if sh.lease_lost() => Err(std::io::Error::other(
                    format!(
                        "{error}; root-lock ownership was lost, so the claimed original was left at recoverable path '{hold}' instead of racing a rollback"
                    ),
                )),
                Err(error) => Err(rollback_claim(
                    exec.as_ref(),
                    &hold,
                    from,
                    error,
                    sh.opt.fsync,
                )),
            }
}
