//! What each op did: one record to the event stream and the run log.

use crate::model::event::ItemOutcome;
use crate::model::plan::{Op, Side};
use super::schedule::{Counters, Shared};
use crate::obs::progress::PhaseProgress;
use std::sync::atomic::Ordering;

/// Book the result of a single op: an error never aborts the whole run (FFS accumulating semantics),
/// each one emits an Error event through the sink (the first time the windowed desktop build can really see errors) plus keeps the eprintln for the CLI.
pub(super) fn record(sh: &Shared, op: &Op, res: std::io::Result<()>, pp: &PhaseProgress, acc: &Counters, ms: u64) {
    let label = format!(
        "[{}] {:?} {}",
        if op.side == Side::Target { "target" } else { "source" },
        op.action,
        op.path
    );
    let side = if op.side == Side::Target { "target" } else { "source" };
    // Every op's outcome emits one ItemResult — this is the only place in the codebase that knows "did this one actually succeed";
    // outside this function all that remains are three aggregate counters. The execution ledger (items.jsonl) rests entirely on it.
    let ledger = |outcome: ItemOutcome| {
        sh.ctx.sink.emit(crate::model::event::ProgressEvent::ItemResult {
            ts_ms: crate::foundation::time::now_ms(),
            path: op.path.clone(),
            action: format!("{:?}", op.action),
            side: side.to_string(),
            outcome,
            // The item's own size (a delta update actually writes fewer bytes — that is a link metric, not a ledger metric)
            bytes: op.size.unwrap_or(0),
            ms,
        });
    };
    match res {
        Ok(_) => {
            acc.done.fetch_add(1, Ordering::Relaxed);
            if sh.opt.verbose {
                println!("OK   {label}");
            }
            ledger(ItemOutcome::Ok);
            pp.item_done(&op.path);
        }
        Err(e) if e.kind() == std::io::ErrorKind::DirectoryNotEmpty => {
            // Keeping the directory is not an error (protecting a filtered file is correct), but it must be visible
            acc.skipped.fetch_add(1, Ordering::Relaxed);
            println!("KEPT      {} ({e})", op.path);
            ledger(ItemOutcome::Kept);
            pp.item_done(&op.path);
        }
        Err(e) if crate::obs::progress::is_cancelled(&e) && sh.ctx.ctl.cancelled() => {
            // User cancelled: this op not completing is not an error — cancelled=true in the summary says it all.
            // But it must leave a trace in the ledger, or "why wasn't this one done" has no answer after the fact.
            ledger(ItemOutcome::Cancelled);
        }
        Err(e) => {
            acc.errors.fetch_add(1, Ordering::Relaxed);
            // Plain stderr for the CLI. Deliberately NOT the log_error! macro: with the
            // desktop's sink installed, the macro line arrives as a Log{Error} event on
            // top of the structured Error event below — the panel then counts every
            // failure twice (observed live: 2075 real errors listed as 4150).
            eprintln!("ERR  {label}: {e}");
            pp.error(&op.path, &format!("{:?}", op.action), side, &e.to_string());
            ledger(ItemOutcome::Failed);
            pp.item_done(&op.path);
        }
    }
}
