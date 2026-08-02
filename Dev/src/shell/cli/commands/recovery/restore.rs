use crate::cli::args::Cmd;

pub(super) fn execute(command: Cmd) -> std::io::Result<i32> {
    match command {
        Cmd::Restore {
            root,
            version,
            files,
            apply: do_apply,
        } => {
            let (restored, skipped, errors) =
                syncdash::store::version::restore(&root, &version, &files, !do_apply)?;
            println!(
                "{}: {restored} restored, {skipped} skipped, {errors} error(s)",
                if do_apply {
                    "restore"
                } else {
                    "dry-run (rerun with --apply)"
                }
            );
            Ok(if errors > 0 { 1 } else { 0 })
        }
        _ => unreachable!("version-restore handler received another command"),
    }
}
