//! Typed snapshot-entry access used by the planning engine.

use crate::model::plan::{Action, Op, Side};
use crate::model::table::{ObservedEntry, ObservedFile, ObservedSymlink};

pub(super) fn push_copy(operations: &mut Vec<Op>, side: Side, entry: &ObservedEntry, reason: &str) {
    let file = observed_file(entry);
    operations.push(Op {
        side,
        action: Action::Copy,
        path: file.path.as_str().to_owned(),
        from: None,
        size: Some(file.size),
        mtime_ms: Some(file.mtime_ms),
        hash: file.identity.plan_hash(),
        link: None,
        mode: None,
        reason: reason.into(),
    });
}

pub(super) fn observed_file(entry: &ObservedEntry) -> &ObservedFile {
    entry
        .as_file()
        .expect("the file observation map contains only files")
}

pub(super) fn observed_symlink(entry: &ObservedEntry) -> &ObservedSymlink {
    entry
        .as_symlink()
        .expect("the symlink observation map contains only symlinks")
}

pub(super) fn observed_path(entry: &ObservedEntry) -> String {
    entry.path().as_str().to_owned()
}
