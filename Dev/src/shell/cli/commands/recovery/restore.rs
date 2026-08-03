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
            super::print_restore_summary(do_apply, restored, skipped, errors);
            Ok(if errors > 0 { 1 } else { 0 })
        }
        _ => unreachable!("version-restore handler received another command"),
    }
}
