use crate::cli::{
    args::{Cmd, Mode},
    write_out,
};
use syncdash::model::table;
use syncdash::pipeline::compare;

pub(super) fn execute(command: Cmd) -> std::io::Result<i32> {
    match command {
        Cmd::Compare {
            source,
            target,
            mode,
            archive,
            resolve_newer,
            case_sensitive,
            out,
        } => {
            let s = table::TableArtifact::load_snapshot(&source)?;
            let t = table::TableArtifact::load_snapshot(&target)?;
            let a = match &archive {
                Some(path) => {
                    Some(syncdash::run::archive::load_archive(path)?.ok_or_else(|| {
                        std::io::Error::new(
                            std::io::ErrorKind::NotFound,
                            format!("archive does not exist: {}", path.display()),
                        )
                    })?)
                }
                None => None,
            };
            let mode_str = match mode {
                Mode::Mirror => "mirror",
                Mode::Sync => "sync",
                Mode::Enrich => "enrich",
            };
            let plan = compare::compare(
                &s,
                &t,
                mode_str,
                a.as_ref(),
                resolve_newer,
                &compare::CompareOptions {
                    case_insensitive: !case_sensitive,
                    ..Default::default()
                },
            );
            eprintln!(
                "plan: {} op(s), {} conflict(s)  [{} -> {}]",
                plan.header.op_count,
                plan.header.conflict_count,
                plan.header.source_root,
                plan.header.target_root
            );
            write_out(&out, |w| plan.write_to(w))?;
            Ok(if plan.header.conflict_count > 0 { 1 } else { 0 })
        }
        _ => unreachable!("compare handler received another command"),
    }
}
