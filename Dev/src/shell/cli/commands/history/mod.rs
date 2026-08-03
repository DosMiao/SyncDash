//! Run-history delivery: the recorded runs and the artifacts each run left behind.
//!
//! Both listings speak of a run's age and of an empty history in the same words, so that wording
//! lives here rather than in whichever command happens to print it first.

mod logs;
mod runs;

use super::super::args::Cmd;

pub(super) fn execute(command: Cmd) -> std::io::Result<i32> {
    match command {
        Cmd::Logs { cmd } => logs::run_logs(cmd),
        command @ Cmd::History { .. } => runs::execute(command),
        _ => unreachable!("history dispatcher received a non-history command"),
    }
}

pub(super) const NO_RUNS_RECORDED: &str =
    "no runs recorded yet (runs are logged when a job actually applies)";

/// A run's age in the coarsest unit that still reads as recent: minutes for the first hour, hours
/// up to two days, days beyond that. A clock that moved backwards reads as `0m ago` rather than
/// as a negative age.
pub(super) fn relative_age(now_ms: i64, ts_ms: i64) -> String {
    let age_min = (now_ms - ts_ms).max(0) / 60_000;
    if age_min < 60 {
        format!("{age_min}m ago")
    } else if age_min < 48 * 60 {
        format!("{}h ago", age_min / 60)
    } else {
        format!("{}d ago", age_min / 60 / 24)
    }
}
