mod capabilities;
mod desktop;
mod probe;

use super::super::args::Cmd;

pub(super) fn execute(command: Cmd) -> std::io::Result<i32> {
    match command {
        command @ Cmd::Probe => probe::execute(command),
        command @ Cmd::Gui => desktop::execute(command),
        command @ Cmd::Caps { .. } => capabilities::execute(command),
        _ => unreachable!("system dispatcher received a non-system command"),
    }
}

pub(in crate::cli) fn launch_default_desktop() -> std::io::Result<i32> {
    desktop::launch_default()
}
