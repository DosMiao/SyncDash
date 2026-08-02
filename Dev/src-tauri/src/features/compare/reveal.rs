//! Turning a Compare row into a path the OS file manager may be pointed at.
//!
//! Two refusals live here and both are safety, not convenience. A relative path is parsed as a
//! `RootRelativePath`, so `../` cannot walk out of the root the result was compared against — a
//! reveal is a path handed to the shell, and a traversal would point it anywhere on the machine.
//! And a non-local root is refused outright: an `sftp://` or `smb://` phrase has no path this
//! computer can open, and joining it to something that looks like one would address a local file
//! that merely shares the remote's spelling.

pub(crate) fn local_compare_path(root: &str, relative: &str) -> Result<std::path::PathBuf, String> {
    let relative = syncdash::foundation::path::RootRelativePath::new(relative)
        .map_err(|error| error.to_string())?;
    let syncdash::fs::vfs::spec::RootSpec::Local(root) = syncdash::fs::vfs::spec::parse(root)
    else {
        return Err("File Manager reveal is only available for roots on this computer".into());
    };
    Ok(syncdash::foundation::path::join_native(
        &root,
        relative.as_str(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reveal_paths_accept_only_safe_entries_under_local_roots() {
        let path = local_compare_path("/root", "folder/file.txt").unwrap();
        assert_eq!(
            path,
            syncdash::foundation::path::join_native(
                std::path::Path::new("/root"),
                "folder/file.txt"
            )
        );
        assert!(local_compare_path("/root", "../outside").is_err());
        assert!(local_compare_path("sftp://host/root", "file.txt").is_err());
    }
}
