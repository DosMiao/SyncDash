//! CLI composition facade.
//!
//! `args` defines the public parser contract. `commands` dispatches parsed values through
//! capability-owned handlers, while each leaf renders the core result it receives.

pub mod args;

mod commands;
mod output;

pub(crate) use output::write_out;

use args::Cli;

pub fn run_cli(cli: Cli) -> std::io::Result<i32> {
    match cli.cmd {
        Some(command) => commands::dispatch(command),
        None => commands::system::launch_default_desktop(),
    }
}
