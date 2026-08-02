use crate::cli::args::Cmd;
use syncdash::job::junk;

pub(super) fn execute(command: Cmd) -> std::io::Result<i32> {
    match command {
        Cmd::Junk { patterns } => {
            match patterns {
                Some(ids) => {
                    let ids: Vec<&str> = ids
                        .iter()
                        .map(|s| s.trim())
                        .filter(|s| !s.is_empty())
                        .collect();
                    if let Some(bad) = ids.iter().find(|id| junk::junk_preset(id).is_none()) {
                        eprintln!(
                            "error: unknown junk preset '{bad}' — run `syncdash junk` for the list"
                        );
                        return Ok(2);
                    }
                    for p in junk::expand_junk_presets(&ids) {
                        println!("{p}");
                    }
                }
                None => {
                    println!("Junk presets — each one is a macro over a job's `exclude` list, nothing more:\n");
                    for p in junk::JUNK_PRESETS {
                        println!(
                            "{}{}  ({})",
                            p.id,
                            if p.default_on {
                                " [on for new jobs]"
                            } else {
                                ""
                            },
                            p.label
                        );
                        println!("  {}", p.hint);
                        println!("  {}\n", p.patterns.join("  "));
                    }
                    println!("Apply ad hoc:  syncdash scan <root> --junk windows,macos,dev");
                    println!("Paste into a job: syncdash junk --patterns dev");
                }
            }
            Ok(0)
        }
        _ => unreachable!("junk handler received another command"),
    }
}
