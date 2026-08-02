use crate::cli::args::Cmd;

pub(super) fn execute(command: Cmd) -> std::io::Result<i32> {
    match command {
        Cmd::Recv { path } => {
            if let Some(p) = path.parent() {
                std::fs::create_dir_all(p)?;
            }
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)?;
            let n = std::io::copy(&mut std::io::stdin().lock(), &mut f)?;
            eprintln!("received {n} bytes -> {}", path.display());
            Ok(0)
        }
        _ => unreachable!("receive handler received another command"),
    }
}
