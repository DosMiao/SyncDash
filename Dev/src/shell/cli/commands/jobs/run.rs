use crate::cli::args::Cmd;
use syncdash::{job, run};

pub(super) fn execute(command: Cmd) -> std::io::Result<i32> {
    match command {
        Cmd::Run {
            job,
            all,
            prefix,
            apply: do_apply,
            verbose,
            watch,
            interval,
            auto_apply,
        } => {
            let list: Vec<(String, job::Job)> = if all || prefix.is_some() {
                job::load_all()?
                    .into_iter()
                    .filter(|(n, _)| prefix.as_deref().map(|p| n.starts_with(p)).unwrap_or(true))
                    .collect()
            } else if let Some(j) = job {
                vec![job::load(&j)?]
            } else {
                eprintln!("error: give a job name, or use --all / --prefix <p>");
                return Ok(2);
            };
            if list.is_empty() {
                eprintln!("no matching jobs");
                return Ok(2);
            }
            // Fast/balanced jobs let unchanged content reuse the hash cache each tick;
            // RootLock stops both ends acting at once.
            if watch {
                let iv = interval
                    .or_else(|| {
                        list.iter()
                            .filter_map(|(_, job)| job.autoscan_interval_secs)
                            .min()
                    })
                    .unwrap_or(30)
                    .max(1);
                eprintln!("watch: {} job(s), every {iv}s — Ctrl-C to stop", list.len());
                loop {
                    for (name, j) in &list {
                        let auto = auto_apply || j.autoscan_auto_apply;
                        let res = run::run_job(name, j, auto, verbose);
                        match res {
                            Ok((d, _s, e, c)) if d + e + c > 0 => {
                                eprintln!(
                                    "[{name}] watch: {d} done, {e} error(s), {c} conflict(s)"
                                );
                            }
                            Ok(_) => {}
                            Err(err) => eprintln!("[{name}] watch error: {err}"),
                        }
                    }
                    std::thread::sleep(std::time::Duration::from_secs(iv));
                }
            }
            let many = list.len() > 1;
            let mut tot = (0u64, 0u64, 0u64, 0u64);
            for (name, j) in &list {
                let res = run::run_job(name, j, do_apply, verbose);
                match res {
                    Ok((d, s, e, c)) => {
                        if do_apply {
                            println!("[{name}] applied: {d} done, {s} skipped, {e} error(s), {c} conflict(s)");
                        }
                        tot.0 += d;
                        tot.1 += s;
                        tot.2 += e;
                        tot.3 += c;
                    }
                    Err(err) => {
                        eprintln!("[{name}] FAILED: {err}");
                        tot.2 += 1;
                    }
                }
            }
            if many {
                println!(
                    "== total: {} job(s), {} done, {} skipped/pending, {} error(s), {} conflict(s)",
                    list.len(),
                    tot.0,
                    tot.1,
                    tot.2,
                    tot.3
                );
            }
            Ok(if tot.2 > 0 { 1 } else { 0 })
        }
        _ => unreachable!("job-run handler received another command"),
    }
}
