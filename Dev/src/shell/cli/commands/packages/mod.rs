mod apply;
mod create;
mod receive;

use super::super::args::Cmd;

pub(super) fn execute(command: Cmd) -> std::io::Result<i32> {
    match command {
        command @ Cmd::Pack { .. } => create::execute(command),
        command @ Cmd::Recv { .. } => receive::execute(command),
        command @ Cmd::ApplyPack { .. } => apply::execute(command),
        _ => unreachable!("package dispatcher received a non-package command"),
    }
}
