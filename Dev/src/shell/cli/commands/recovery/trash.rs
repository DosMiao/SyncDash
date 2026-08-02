use crate::cli::args::{Cmd, TrashCmd};

pub(super) fn execute(command: Cmd) -> std::io::Result<i32> {
    match command {
        Cmd::Trash { cmd } => {
            use syncdash::foundation::fmt::human_bytes;
            match cmd {
                TrashCmd::Runs => {
                    let runs = syncdash::store::trash::list_runs();
                    if runs.is_empty() {
                        println!(
                            "no trash runs under {}",
                            syncdash::store::trash::trash_root().display()
                        );
                    }
                    let mut total = 0u64;
                    for r in &runs {
                        println!(
                            "{:<16} {:>7} files  {:>10}",
                            r.id,
                            r.files,
                            human_bytes(r.bytes)
                        );
                        total += r.bytes;
                    }
                    if !runs.is_empty() {
                        println!("== {} run(s), {} total", runs.len(), human_bytes(total));
                    }
                    Ok(0)
                }
                TrashCmd::Find { pattern } => {
                    let hits = syncdash::store::trash::find(&pattern);
                    for h in &hits {
                        println!("{:<16} {:>10}  {}", h.run_id, human_bytes(h.size), h.rel);
                    }
                    println!("{} version(s)", hits.len());
                    Ok(0)
                }
                TrashCmd::Restore {
                    pattern,
                    into,
                    run,
                    apply: do_apply,
                } => {
                    let (r, s, e) = syncdash::store::trash::restore(
                        &pattern,
                        run.as_deref(),
                        &into,
                        !do_apply,
                    )?;
                    println!(
                        "{}: {r} restored, {s} skipped, {e} error(s)",
                        if do_apply {
                            "restore"
                        } else {
                            "dry-run (rerun with --apply)"
                        }
                    );
                    Ok(if e > 0 { 1 } else { 0 })
                }
                TrashCmd::Prune {
                    keep_days,
                    max_gib,
                    no_staggered,
                    apply: do_apply,
                } => {
                    let ret = syncdash::store::trash::Retention {
                        keep_days,
                        max_bytes: max_gib * 1024 * 1024 * 1024,
                        staggered: !no_staggered,
                    };
                    let (n, freed) = syncdash::store::trash::prune(&ret, !do_apply)?;
                    println!(
                        "{}: {n} run(s), {} freed",
                        if do_apply {
                            "pruned"
                        } else {
                            "dry-run (rerun with --apply)"
                        },
                        human_bytes(freed)
                    );
                    Ok(0)
                }
            }
        }
        _ => unreachable!("trash handler received another command"),
    }
}
