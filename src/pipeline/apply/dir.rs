//! Deleting a directory, and saying why it survived if it did.
//!
//! A directory that will not go empty is not an error — it usually means something inside was
//! excluded by the filter, or a file appeared after the scan. The engine walks it to find out
//! which, so the report names the reason instead of just "failed".



/// Classified outcome of a failed directory deletion (P0-4).
/// This used to be `Err(_) => Ok(())`: safe behavior (no recursive delete) but **completely silent** —
/// the user sees "0 errors" while the directory is still there, the next comparison emits the same DeleteDir again, and it never converges.
pub(super) enum DirOutcome {
    Removed,
    /// It was never there in the first place
    Absent,
    /// Non-empty; `sample` names a few of the leftovers so the error can say what is still in there.
    /// Deletability was already decided before this point — a directory whose leftovers all match
    /// `deletable` is removed outright and reports `Removed`, so reaching here means it is staying.
    NotEmpty { sample: Vec<String> },
    Failed(std::io::Error),
}

pub(super) fn try_delete_dir_vfs(
    exec: &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
    rel: &str,
    filter: Option<&crate::pipeline::filter::PathFilter>,
) -> DirOutcome {
    use crate::fs::vfs::error::VfsErrorKind;
    use crate::model::table::EntryKind;
    match exec.remove_dir(rel) {
        Ok(_) => return DirOutcome::Removed,
        Err(e) if e.kind == VfsErrorKind::NotFound => return DirOutcome::Absent,
        Err(e) if e.kind != VfsErrorKind::NotEmpty => return DirOutcome::Failed(e.into()),
        Err(_) => {}
    }
    // Non-empty: walk what is left (engine-side DFS, one read_dir per directory).
    //
    // Symlinks count exactly as files do. Letting directories "go along with the tree" is right —
    // an empty directory carries nothing — but a symlink is content: it is the only record of where
    // it pointed, and it cannot be reconstructed from the other side once unlinked. Excluding it
    // from `count` meant a directory whose leftovers were *only* symlinks had count == 0, sailed
    // past the guard below, and had every link removed through `exec.remove_file` — not preserved,
    // not versioned, not in the trash. With `symlinks = "exclude"` (the default) those links are
    // invisible to both snapshots as well, so nothing anywhere recorded what was destroyed. Deleting
    // one `Foo.app` was enough: every `Versions/Current -> A` inside it went that way.
    let mut sample = Vec::new();
    let mut all_deletable = true;
    let mut count = 0usize;
    let mut files: Vec<String> = Vec::new();
    let mut dirs: Vec<String> = Vec::new();
    let mut stack = vec![rel.to_string()];
    while let Some(d) = stack.pop() {
        let list = match exec.read_dir(&d) {
            Ok(l) => l,
            Err(e) if e.kind == VfsErrorKind::NotFound => continue,
            Err(e) => return DirOutcome::Failed(e.into()),
        };
        for e in list {
            let child_rel = format!("{}/{}", d.trim_end_matches('/'), e.name);
            match e.meta.kind {
                EntryKind::Dir => {
                    dirs.push(child_rel.clone());
                    stack.push(child_rel);
                }
                EntryKind::File | EntryKind::Symlink => {
                    count += 1;
                    let deletable = filter.map(|f| f.is_deletable(&child_rel)).unwrap_or(false);
                    if !deletable {
                        all_deletable = false;
                    }
                    if sample.len() < 5 {
                        sample.push(child_rel.clone());
                    }
                    files.push(child_rel);
                }
            }
        }
    }
    if count > 0 && !all_deletable {
        return DirOutcome::NotEmpty { sample };
    }
    // Only deletable leftovers / only subdirectories: remove the tree, deepest first
    for f in &files {
        if let Err(e) = exec.remove_file(f) {
            if e.kind != VfsErrorKind::NotFound {
                return DirOutcome::Failed(e.into());
            }
        }
    }
    dirs.sort_by_key(|d| std::cmp::Reverse(d.matches('/').count()));
    for d in &dirs {
        if let Err(e) = exec.remove_dir(d) {
            if e.kind != VfsErrorKind::NotFound {
                return DirOutcome::Failed(e.into());
            }
        }
    }
    match exec.remove_dir(rel) {
        Ok(_) => DirOutcome::Removed,
        Err(e) if e.kind == VfsErrorKind::NotFound => DirOutcome::Removed,
        Err(e) => DirOutcome::Failed(e.into()),
    }
}
