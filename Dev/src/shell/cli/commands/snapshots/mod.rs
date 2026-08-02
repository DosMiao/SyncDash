mod chunks;
mod compare;
mod scan;

use super::super::args::Cmd;

pub(super) fn execute(command: Cmd) -> std::io::Result<i32> {
    match command {
        command @ Cmd::Scan { .. } => scan::execute(command),
        command @ Cmd::Compare { .. } => compare::execute(command),
        command @ Cmd::Chunks { .. } => chunks::execute(command),
        _ => unreachable!("snapshot dispatcher received a non-snapshot command"),
    }
}
