use crate::foundation::path::RootRelativePath;
use crate::fs::local_root::LocalRoot;

pub(super) struct TemporaryPeerPackage {
    pub(super) root: LocalRoot,
    relative: RootRelativePath,
    file: std::fs::File,
}

impl TemporaryPeerPackage {
    pub(super) fn create() -> std::io::Result<Self> {
        let root = LocalRoot::open(std::env::temp_dir())?;
        for _ in 0..16 {
            let token = crate::foundation::token::random_token_128().map_err(|error| {
                std::io::Error::other(format!("random token generation failed: {error}"))
            })?;
            let relative = RootRelativePath::try_from(format!("syncdash-peer-{token}.tar"))
                .expect("generated peer package names satisfy the relative-path contract");
            match root.create_regular_file_new(&relative) {
                Ok(file) => {
                    return Ok(Self {
                        root,
                        relative,
                        file,
                    })
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "could not allocate an exclusive peer package file",
        ))
    }

    pub(super) fn output(&self) -> std::io::Result<std::fs::File> {
        self.file.try_clone()
    }

    pub(super) fn reader(&self) -> std::io::Result<std::fs::File> {
        use std::io::{Seek, SeekFrom};

        let mut reader = self.file.try_clone()?;
        reader.seek(SeekFrom::Start(0))?;
        Ok(reader)
    }

    pub(super) fn len(&self) -> std::io::Result<u64> {
        Ok(self.file.metadata()?.len())
    }

    pub(super) fn file_name(&self) -> &str {
        self.relative.as_str()
    }
}

impl Drop for TemporaryPeerPackage {
    fn drop(&mut self) {
        let _ = self.root.remove_open_file(&self.relative, &self.file);
    }
}
