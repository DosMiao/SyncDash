//! Opt-in automatic content-conflict resolution and conflict-copy retention.

use std::collections::BTreeMap;

use crate::foundation::names::CONFLICT_INFIX;
use crate::foundation::text::norm_key;
use crate::foundation::time::now_ms;
use crate::model::plan::{Action, Op, Side};
use crate::model::table::{ObservedEntry, ObservedEntryKind, TableArtifact};

use super::super::conflicts::{conflict_name, is_conflict_copy};
use super::super::entries::observed_file;
use super::super::policy::{CompareOptions, ConflictPolicy};

pub(super) fn resolve(
    source: &TableArtifact,
    target: &TableArtifact,
    options: &CompareOptions,
    source_files: &BTreeMap<String, &ObservedEntry>,
    target_files: &BTreeMap<String, &ObservedEntry>,
    operations: &mut Vec<Op>,
) {
    if options.conflict == ConflictPolicy::Report {
        return;
    }

    const RESOLVABLE: [&str; 3] = ["both-changed", "differs-no-archive", "symlink-differs"];
    let now = now_ms();
    let mut resolutions = Vec::new();
    for operation in operations.iter_mut() {
        if operation.action != Action::Conflict
            || !RESOLVABLE.contains(&operation.reason.as_str())
            || is_conflict_copy(&operation.path)
        {
            continue;
        }
        let key = norm_key(&operation.path, options.case_insensitive);
        let (Some(&source_entry), Some(&target_entry)) =
            (source_files.get(&key), target_files.get(&key))
        else {
            continue;
        };
        let source_file = observed_file(source_entry);
        let target_file = observed_file(target_entry);
        let source_wins = match source_file.mtime_ms.cmp(&target_file.mtime_ms) {
            std::cmp::Ordering::Greater => true,
            std::cmp::Ordering::Less => false,
            std::cmp::Ordering::Equal => source.header.host <= target.header.host,
        };
        let (winner, loser, loser_side, loser_host) = if source_wins {
            (
                source_file,
                target_file,
                Side::Target,
                target.header.host.as_str(),
            )
        } else {
            (
                target_file,
                source_file,
                Side::Source,
                source.header.host.as_str(),
            )
        };

        if options.conflict == ConflictPolicy::Copy {
            let kept = conflict_name(loser.path.as_str(), loser_host, now);
            resolutions.push(Op {
                side: loser_side.clone(),
                action: Action::Move,
                path: kept,
                from: Some(loser.path.as_str().to_owned()),
                size: Some(loser.size),
                mtime_ms: Some(loser.mtime_ms),
                hash: loser.identity.plan_hash(),
                link: None,
                mode: loser.mode,
                reason: format!("conflict-loser-kept-as-copy ({})", operation.reason),
            });
        }
        resolutions.push(Op {
            side: loser_side,
            action: Action::Update,
            path: loser.path.as_str().to_owned(),
            from: None,
            size: Some(winner.size),
            mtime_ms: Some(winner.mtime_ms),
            hash: winner.identity.plan_hash(),
            link: None,
            mode: None,
            reason: format!(
                "conflict-resolved-newer-wins ({})",
                if options.conflict == ConflictPolicy::Copy {
                    "loser kept as .sync-conflict copy"
                } else {
                    "loser overwritten (recoverable from trash)"
                }
            ),
        });
        operation.action = Action::Note;
        operation.reason = format!("auto-resolved: {}", operation.reason);
    }
    operations.extend(resolutions);

    if options.conflict != ConflictPolicy::Copy || options.max_conflicts < 0 {
        return;
    }
    let limit = options.max_conflicts as usize;
    for (is_target, snapshot) in [(false, source), (true, target)] {
        let mut groups: BTreeMap<String, Vec<&str>> = BTreeMap::new();
        for entry in snapshot
            .entries
            .iter()
            .filter(|entry| entry.kind() == ObservedEntryKind::File)
        {
            let path = entry.path().as_str();
            if is_conflict_copy(path) {
                if let Some(index) = path.find(CONFLICT_INFIX) {
                    groups
                        .entry(path[..index].to_owned())
                        .or_default()
                        .push(path);
                }
            }
        }
        for mut copies in groups.into_values() {
            if copies.len() <= limit {
                continue;
            }
            copies.sort_unstable();
            let doomed = copies.len() - limit;
            for path in copies.into_iter().take(doomed) {
                operations.push(Op {
                    side: if is_target {
                        Side::Target
                    } else {
                        Side::Source
                    },
                    action: Action::Delete,
                    path: path.to_owned(),
                    from: None,
                    size: None,
                    mtime_ms: None,
                    hash: None,
                    link: None,
                    mode: None,
                    reason: format!("conflict-copy-over-limit (max_conflicts = {limit})"),
                });
            }
        }
    }
}
