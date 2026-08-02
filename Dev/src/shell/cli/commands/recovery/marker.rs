use crate::cli::args::Cmd;

pub(super) fn execute(command: Cmd) -> std::io::Result<i32> {
    match command {
        Cmd::Mark { root, job, note } => {
            let (path, created) =
                syncdash::pipeline::guard::marker::write_marker(&root, &job, &note)?;
            if created {
                println!("marked: {}", path.display());
            } else {
                let m = syncdash::pipeline::guard::marker::read_marker(&root);
                println!(
                    "already marked: {}{}",
                    path.display(),
                    m.map(|m| format!("  (job '{}', by {} )", m.job, m.host))
                        .unwrap_or_default()
                );
            }
            println!(
                "set `require_marker = true` in the job to have syncdash refuse to run without it"
            );
            Ok(0)
        }
        _ => unreachable!("marker handler received another command"),
    }
}
