use std::path::PathBuf;

pub(crate) fn write_out<F: Fn(&mut dyn std::io::Write) -> std::io::Result<()>>(
    out: &Option<PathBuf>,
    write: F,
) -> std::io::Result<()> {
    match out {
        Some(path) => {
            let file = std::fs::File::create(path)?;
            let mut writer = std::io::BufWriter::new(file);
            write(&mut writer)
        }
        None => {
            let stdout = std::io::stdout();
            let mut writer = std::io::BufWriter::new(stdout.lock());
            write(&mut writer)
        }
    }
}
