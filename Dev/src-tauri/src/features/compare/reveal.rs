//! Turning a Compare row into a path the OS file manager may be pointed at.
//!
//! The two refusals on the resolved path are safety, not convenience. A relative path is parsed as a
//! `RootRelativePath`, so `../` cannot walk out of the root the result was compared against — a
//! reveal is a path handed to the shell, and a traversal would point it anywhere on the machine.
//! And a non-local root is refused outright: an `sftp://` or `smb://` phrase has no path this
//! computer can open, and joining it to something that looks like one would address a local file
//! that merely shares the remote's spelling.

use syncdash::pipeline::compare::evidence::side_paths;

use crate::contracts::compare::{CompareFileSideDto, CompareIdentity};
use crate::features::compare::evidence::repository::CompareResultRepository;
use crate::features::compare::export::presentation::presented_operation;

/// Resolve one reviewed row of a retained Compare result to a path on this computer. The row is
/// reconstructed from the retained plan in the direction the operator is looking at it, so the
/// caller never gets to name a path of its own.
pub(crate) fn compare_row_path(
    results: &CompareResultRepository,
    compare_identity: &CompareIdentity,
    index: usize,
    side: CompareFileSideDto,
    direction_reversed: bool,
) -> Result<std::path::PathBuf, String> {
    let retained = results
        .require_exact(compare_identity)
        .map_err(|error| error.to_string())?;
    let operation = presented_operation(
        retained.plan_operations(),
        retained.plan_metadata(),
        index,
        direction_reversed,
    )?;
    let (source_relative, target_relative) = side_paths(&operation);
    let (root, relative, side_name) = match side {
        CompareFileSideDto::Source => (
            retained.plan_header().source_root.as_str(),
            source_relative,
            "source",
        ),
        CompareFileSideDto::Target => (
            retained.plan_header().target_root.as_str(),
            target_relative,
            "target",
        ),
    };
    let relative = relative
        .ok_or_else(|| format!("Compare row {} has no {side_name}-side path", index + 1))?;
    local_compare_path(root, relative)
}

fn local_compare_path(root: &str, relative: &str) -> Result<std::path::PathBuf, String> {
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
