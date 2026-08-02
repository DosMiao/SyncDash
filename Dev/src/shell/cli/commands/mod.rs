//! Capability-level command routing.

mod credentials;
mod execution;
mod history;
mod jobs;
mod packages;
mod recovery;
mod snapshots;
mod system;

use super::args::Cmd;

pub(super) fn dispatch(command: Cmd) -> std::io::Result<i32> {
    match command {
        command @ (Cmd::Probe | Cmd::Gui | Cmd::Caps { .. }) => system::execute(command),
        command @ (Cmd::Jobs
        | Cmd::Run { .. }
        | Cmd::Junk { .. }
        | Cmd::Territories { .. }
        | Cmd::GenJobs { .. }) => jobs::execute(command),
        command @ (Cmd::Scan { .. } | Cmd::Compare { .. } | Cmd::Chunks { .. }) => {
            snapshots::execute(command)
        }
        command @ (Cmd::Pack { .. } | Cmd::Recv { .. } | Cmd::ApplyPack { .. }) => {
            packages::execute(command)
        }
        command @ (Cmd::Versions { .. }
        | Cmd::Restore { .. }
        | Cmd::Mark { .. }
        | Cmd::Trash { .. }) => recovery::execute(command),
        command @ Cmd::Cred { .. } => credentials::execute(command),
        command @ (Cmd::Logs { .. } | Cmd::History { .. }) => history::execute(command),
        command @ Cmd::Apply { .. } => execution::execute(command),
    }
}

pub(super) fn launch_default_desktop() -> std::io::Result<i32> {
    system::launch_default_desktop()
}
