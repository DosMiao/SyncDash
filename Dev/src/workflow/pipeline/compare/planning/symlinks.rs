//! Direct-symlink comparison, kept separate from regular-file evidence.

use crate::model::plan::{Action, Op, Side};
use crate::model::table::{ObservedEntry, ObservedEntryKind, TableArtifact};

use super::super::entries::observed_symlink;
use super::super::matching::{map_of, UnreadScope};

pub(super) fn plan(
    source: &TableArtifact,
    target: &TableArtifact,
    mode: &str,
    case_insensitive: bool,
    operations: &mut Vec<Op>,
) {
    let unread = UnreadScope::of(source, target, case_insensitive);
    let (source_links, _) = map_of(
        source,
        ObservedEntryKind::Symlink,
        case_insensitive,
        &unread,
    );
    let (target_links, _) = map_of(
        target,
        ObservedEntryKind::Symlink,
        case_insensitive,
        &unread,
    );
    let link_operation = |side: Side, action: Action, entry: &ObservedEntry, reason: &str| {
        let symlink = observed_symlink(entry);
        Op {
            side,
            action,
            path: symlink.path.as_str().to_owned(),
            from: None,
            size: None,
            mtime_ms: None,
            hash: None,
            link: Some(symlink.target.clone()),
            mode: None,
            reason: reason.into(),
        }
    };

    for (path, source_entry) in &source_links {
        let source_entry = *source_entry;
        match target_links.get(path) {
            None => {
                if matches!(mode, "mirror" | "enrich" | "sync") {
                    operations.push(link_operation(
                        Side::Target,
                        Action::Copy,
                        source_entry,
                        "symlink-only-in-source",
                    ));
                }
            }
            Some(&target_entry) => {
                if observed_symlink(source_entry).target != observed_symlink(target_entry).target {
                    match mode {
                        "mirror" => operations.push(link_operation(
                            Side::Target,
                            Action::Update,
                            source_entry,
                            "symlink-differs-master-wins",
                        )),
                        "sync" => operations.push(link_operation(
                            Side::Target,
                            Action::Conflict,
                            source_entry,
                            "symlink-differs",
                        )),
                        _ => {}
                    }
                }
            }
        }
    }

    for (path, target_entry) in &target_links {
        let target_entry = *target_entry;
        if source_links.contains_key(path) {
            continue;
        }
        match mode {
            "mirror" => operations.push(link_operation(
                Side::Target,
                Action::Delete,
                target_entry,
                "symlink-gone-from-source",
            )),
            "sync" => operations.push(link_operation(
                Side::Source,
                Action::Copy,
                target_entry,
                "symlink-only-in-target",
            )),
            _ => {}
        }
    }
}
