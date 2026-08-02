mod generation;
mod junk;
mod list;
mod run;
mod territories;

use super::super::args::Cmd;

pub(super) fn execute(command: Cmd) -> std::io::Result<i32> {
    match command {
        command @ Cmd::Jobs => list::execute(command),
        command @ Cmd::Run { .. } => run::execute(command),
        command @ Cmd::Junk { .. } => junk::execute(command),
        command @ Cmd::Territories { .. } => territories::execute(command),
        command @ Cmd::GenJobs { .. } => generation::execute(command),
        _ => unreachable!("job dispatcher received a non-job command"),
    }
}
