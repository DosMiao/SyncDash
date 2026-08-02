use crate::cli::args::Cmd;
use syncdash::job;

pub(super) fn execute(command: Cmd) -> std::io::Result<i32> {
    match command {
        Cmd::Jobs => {
            let jobs = job::load_all()?;
            if jobs.is_empty() {
                println!(
                    "no jobs in {}\n\nsample job file:\n{}",
                    syncdash::foundation::dirs::jobs_dir().display(),
                    job::SAMPLE
                );
            } else {
                for (name, j) in jobs {
                    println!(
                        "{:<24} {:<7} {}  ->  {}",
                        name,
                        j.mode,
                        j.source,
                        j.targets.join(", ")
                    );
                }
            }
            Ok(0)
        }
        _ => unreachable!("job-list handler received another command"),
    }
}
