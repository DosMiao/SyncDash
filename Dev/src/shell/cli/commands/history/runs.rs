use crate::cli::args::Cmd;

pub(super) fn execute(command: Cmd) -> std::io::Result<i32> {
    match command {
        Cmd::History {
            job,
            limit,
            prune_days,
        } => {
            if let Some(days) = prune_days {
                // 0 = don't stack the total-size gate: the meaning of `--prune-days N` is exactly "by days only"
                let n = syncdash::run::history::prune(days, 0)?;
                println!("pruned {n} run(s) older than {days} day(s)");
            }
            let rows = syncdash::run::history::history(job.as_deref(), limit)?;
            if rows.is_empty() {
                println!("{}", super::NO_RUNS_RECORDED);
                return Ok(0);
            }
            let now = syncdash::foundation::time::now_ms() as i64;
            for r in rows {
                let age = super::relative_age(now, r.ts_ms);
                println!(
                    "{:>9}  {:<20} {:<12} {:>5} done {:>4} skip {:>3} err  {:>10}  {:>7.1}s{}",
                    age,
                    r.subject.job_name,
                    r.kind,
                    r.done,
                    r.skipped,
                    r.errors,
                    syncdash::foundation::fmt::human_bytes(r.bytes),
                    r.elapsed_ms as f64 / 1000.0,
                    if r.cancelled { "  [cancelled]" } else { "" },
                );
            }
            Ok(0)
        }
        _ => unreachable!("run-history handler received another command"),
    }
}
