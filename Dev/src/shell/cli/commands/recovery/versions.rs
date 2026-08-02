use crate::cli::args::Cmd;

pub(super) fn execute(command: Cmd) -> std::io::Result<i32> {
    match command {
        Cmd::Versions { root, prune } => {
            if let Some(keep) = prune {
                let dropped = syncdash::store::version::prune(&root, keep)?;
                println!("pruned {} version(s), kept newest {keep}", dropped.len());
            }
            let list = syncdash::store::version::list(&root)?;
            if list.is_empty() {
                println!(
                    "no versions under {}",
                    root.join(syncdash::foundation::names::VERSION_STORE_DIR)
                        .display()
                );
            } else {
                for v in &list {
                    println!(
                        "{}  {}  ops={} preserved={} bytes={}",
                        v.id, v.host, v.ops, v.preserved, v.bytes
                    );
                }
            }
            Ok(0)
        }
        _ => unreachable!("versions handler received another command"),
    }
}
