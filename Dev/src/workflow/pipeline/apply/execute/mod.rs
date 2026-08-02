//! What a single op does, dispatched by action.
//!
//! Copy and Update share a lane — the difference between "the target has nothing" and "the target
//! has the wrong thing" matters to the plan, not to the write. Both stage, verify if asked, and
//! land by rename.

mod delta;
mod directory;
mod file_transaction;
mod ledger;
mod operations;
pub(super) mod preservation;
pub(super) mod schedule;

use self::directory::{try_delete_dir_vfs, DirOutcome};
use self::preservation::preserve;
use self::schedule::Shared;
use crate::model::plan::{Action, Op};
use crate::obs::progress::PhaseProgress;

/// Above this size, skip the in-memory delta path instead of retaining several GiB in memory.
const DELTA_MEM_CAP: u64 = 1024 * 1024 * 1024;

/// Execute a single op through the VFS pair. Cancel/pause are honored at chunk boundaries via
/// `Shared::checkpoint`; staged-write Drop contracts keep interrupted work unpublished.
pub(super) fn exec_op(sh: &Shared, op: &Op, pp: &PhaseProgress) -> std::io::Result<()> {
    sh.checkpoint(pp)?;
    let (exec, _) = sh.exec_other(&op.side);
    match op.action {
        Action::Copy | Action::Update => operations::copy_file::execute(sh, op, pp),
        Action::Chmod => {
            let mode = op.mode.ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "chmod op without mode")
            })?;
            if exec.caps().unix_mode != crate::fs::vfs::Support::Yes {
                return Ok(());
            }
            sh.check_before_mutation()?;
            Ok(exec.set_mode(&op.path, mode)?)
        }
        Action::Move => operations::move_file::execute(sh, op, pp),
        Action::Delete => {
            if exec.stat(&op.path)?.is_some() {
                preserve(sh, op, exec, "deleted", None, pp)?;
            }
            Ok(())
        }
        Action::DeleteDir => {
            match try_delete_dir_vfs(exec, &op.path, sh.opt.filter.as_ref(), || {
                sh.check_before_mutation()
            }) {
                DirOutcome::Removed | DirOutcome::Absent => Ok(()),
                DirOutcome::NotEmpty { sample } => Err(std::io::Error::new(
                    std::io::ErrorKind::DirectoryNotEmpty,
                    format!(
                        "directory not empty, kept: {} (protected by filters or unknown to the plan). \
                         Add them to `deletable` in the job to have them removed with the directory.",
                        sample.join(", ")
                    ),
                )),
                DirOutcome::Failed(error) => Err(error),
            }
        }
        Action::Conflict | Action::Note => Ok(()),
    }
}
