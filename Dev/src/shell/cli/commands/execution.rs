use std::path::PathBuf;

use crate::cli::args::Cmd;
use syncdash::pipeline::apply;

pub(super) fn execute(command: Cmd) -> std::io::Result<i32> {
    match command {
        Cmd::Apply {
            plan,
            apply: do_apply,
            source_root,
            target_root,
            trash,
            verify,
            versioning,
            delta,
            no_fsync,
            verbose,
        } => {
            let p = syncdash::model::plan::Plan::load(&plan)?;
            let sr = source_root.unwrap_or_else(|| PathBuf::from(&p.header.source_root));
            let tr = target_root.unwrap_or_else(|| PathBuf::from(&p.header.target_root));
            for (name, r) in [("source", &sr), ("target", &tr)] {
                if !r.is_dir() {
                    eprintln!("error: {name} root is not locally accessible: {} (use `syncdash run` for VFS or peer roots)", r.display());
                    return Ok(2);
                }
            }
            let (done, skipped, errors) = apply::apply(
                &p.ops,
                &sr,
                &tr,
                &apply::ApplyOptions {
                    dry_run: !do_apply,
                    trash,
                    verbose,
                    verify,
                    versioning,
                    delta,
                    fsync: !no_fsync,
                    ..Default::default()
                },
            );
            println!(
                "{}: {done} done, {skipped} {}, {errors} error(s)",
                if do_apply { "applied" } else { "dry-run" },
                if do_apply {
                    "skipped"
                } else {
                    "pending (rerun with --apply)"
                },
            );
            Ok(if errors > 0 { 1 } else { 0 })
        }
        _ => unreachable!("apply handler received another command"),
    }
}
