mod runs;

use super::super::args::Cmd;

pub(super) fn execute(command: Cmd) -> std::io::Result<i32> {
    match command {
        Cmd::Logs { cmd } => crate::cli::logs::run_logs(cmd),
        command @ Cmd::History { .. } => runs::execute(command),
        _ => unreachable!("history dispatcher received a non-history command"),
    }
}
