//! Recovery delivery: version history, the local trash, and the mount-point marker.
//!
//! Recovering from the trash and recovering from a version both report the same three counts under
//! the same dry-run label, so that line is written once here.

mod marker;
mod restore;
mod trash;
mod versions;

use super::super::args::Cmd;

pub(super) fn execute(command: Cmd) -> std::io::Result<i32> {
    match command {
        command @ Cmd::Versions { .. } => versions::execute(command),
        command @ Cmd::Restore { .. } => restore::execute(command),
        command @ Cmd::Mark { .. } => marker::execute(command),
        command @ Cmd::Trash { .. } => trash::execute(command),
        _ => unreachable!("recovery dispatcher received a non-recovery command"),
    }
}

/// Both recovery commands are dry-run unless `--apply` is given, so a run that wrote nothing says
/// how to make it real rather than only reporting what it would have done.
pub(super) fn print_restore_summary(applied: bool, restored: u64, skipped: u64, errors: u64) {
    println!(
        "{}: {restored} restored, {skipped} skipped, {errors} error(s)",
        if applied {
            "restore"
        } else {
            syncdash::foundation::fmt::DRY_RUN_HINT
        }
    );
}
