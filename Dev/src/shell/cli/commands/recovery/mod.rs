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
