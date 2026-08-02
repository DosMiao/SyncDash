use crate::cli::args::Cmd;

pub(super) fn execute(command: Cmd) -> std::io::Result<i32> {
    match command {
        Cmd::Chunks { root, files } => {
            let root = syncdash::fs::local_root::LocalRoot::open(root)?;
            let files: Vec<syncdash::foundation::path::RootRelativePath> = files
                .into_iter()
                .map(|rel| {
                    syncdash::foundation::path::RootRelativePath::try_from(rel).map_err(|e| {
                        std::io::Error::new(std::io::ErrorKind::InvalidInput, e.to_string())
                    })
                })
                .collect::<Result<_, _>>()?;
            let stdout = std::io::stdout();
            let mut w = std::io::BufWriter::new(stdout.lock());
            for rel in &files {
                let fc = syncdash::fs::chunk::chunk_file(&root, rel)?;
                use std::io::Write;
                writeln!(w, "{}", serde_json::to_string(&fc)?)?;
            }
            Ok(0)
        }
        _ => unreachable!("chunks handler received another command"),
    }
}
