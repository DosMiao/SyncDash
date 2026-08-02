//! Atomic staged publication for best-effort scanner state.

use std::io::Write;
use std::path::Path;

pub(crate) fn rewrite(
    destination: &Path,
    write: impl FnOnce(&mut dyn Write) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let directory = destination.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "cache destination has no parent",
        )
    })?;
    std::fs::create_dir_all(directory)?;
    let mut staged = crate::fs::staged::Staged::create(destination)?;
    {
        let mut buffered = std::io::BufWriter::new(&mut staged);
        write(&mut buffered)?;
        buffered.flush()?;
    }
    staged.seal(true)?;
    staged.commit()
}
