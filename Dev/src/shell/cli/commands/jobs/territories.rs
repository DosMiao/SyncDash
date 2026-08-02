use crate::cli::args::Cmd;
use syncdash::job::territory;

pub(super) fn execute(command: Cmd) -> std::io::Result<i32> {
    match command {
        Cmd::Territories { root } => {
            let ts = territory::find_territories(&root);
            if ts.is_empty() {
                println!("no .ffs-sync territories under {}", root.display());
            } else {
                for t in &ts {
                    println!("{t}");
                }
                eprintln!("{} territor(ies)", ts.len());
            }
            Ok(0)
        }
        _ => unreachable!("territory handler received another command"),
    }
}
