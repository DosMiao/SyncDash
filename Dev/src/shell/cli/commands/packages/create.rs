use std::path::PathBuf;

use crate::cli::args::Cmd;
use syncdash::transfer::pack;

pub(super) fn execute(command: Cmd) -> std::io::Result<i32> {
    match command {
        Cmd::Pack {
            plan,
            out,
            source_root,
        } => {
            let p = syncdash::model::plan::Plan::load(&plan)?;
            let sr = source_root.unwrap_or_else(|| PathBuf::from(&p.header.source_root));
            let s = pack::pack(&p, &sr, &out, None)?;
            println!(
                "packed: {} op(s), {} payload file(s), {} bytes -> {}",
                s.ops,
                s.files,
                s.bytes,
                out.display()
            );
            Ok(0)
        }
        _ => unreachable!("pack handler received another command"),
    }
}
