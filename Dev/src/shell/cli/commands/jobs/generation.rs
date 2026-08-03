use crate::cli::args::Cmd;
use syncdash::job::{junk, territory};

pub(super) fn execute(command: Cmd) -> std::io::Result<i32> {
    match command {
        Cmd::GenJobs {
            root,
            target_root,
            mode,
            rigor,
            peer_host,
            peer_root_base,
            peer_exe,
            junk,
            force,
        } => {
            let peer_generation = peer_host.map(|h| territory::PeerJobGeneration {
                host: h,
                root_base: peer_root_base.unwrap_or_default(),
                exe: peer_exe,
            });
            let ids = match junk::parse_preset_ids(&junk) {
                Ok(ids) => ids,
                Err(message) => {
                    eprintln!("error: {message}");
                    return Ok(2);
                }
            };
            let n_pat = junk::expand_junk_presets(&ids).len();
            let opts = territory::GenOpts {
                mode,
                rigor,
                junk: ids.clone(),
                force,
                ..Default::default()
            };
            let outs = territory::gen_jobs(&root, &target_root, &opts, peer_generation.as_ref())?;
            for o in &outs {
                println!(
                    "{:<44} <- {}{}",
                    o.name,
                    o.territory,
                    if o.written {
                        ""
                    } else {
                        "   [kept — already exists]"
                    }
                );
            }
            let written = outs.iter().filter(|o| o.written).count();
            let kept = outs.len() - written;
            // State the seed rather than leaving it to be discovered: these lines are the job's entire filter
            println!(
                "{written} job(s) written to {} — each seeded with junk presets [{}] = {n_pat} exclude line(s), all listed in the file",
                syncdash::foundation::dirs::jobs_dir().display(),
                if ids.is_empty() { "none".into() } else { ids.join(", ") },
            );
            if kept > 0 {
                println!("{kept} existing job(s) left untouched (their exclude lists may have been edited) — pass --force to reseed them");
            }
            Ok(0)
        }
        _ => unreachable!("job-generation handler received another command"),
    }
}
