use crate::cli::args::Cmd;
use syncdash::transfer::pack;

pub(super) fn execute(command: Cmd) -> std::io::Result<i32> {
    match command {
        Cmd::ApplyPack {
            pkg,
            target_root,
            apply: do_apply,
            remove_pkg,
            versioning,
            verbose,
        } => {
            let (done, skipped, errors) =
                pack::apply_pack(&pkg, target_root.as_deref(), do_apply, verbose, versioning)?;
            println!(
                "{}: {done} done, {skipped} skipped, {errors} error(s)",
                if do_apply { "applied" } else { "dry-run" }
            );
            if remove_pkg && errors == 0 && do_apply {
                let _ = std::fs::remove_file(&pkg);
            }
            Ok(if errors > 0 { 1 } else { 0 })
        }
        _ => unreachable!("apply-pack handler received another command"),
    }
}
