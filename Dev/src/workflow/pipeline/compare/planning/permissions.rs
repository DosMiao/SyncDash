//! Unix-mode propagation without unnecessary content retransmission.

use std::collections::BTreeMap;

use crate::foundation::text::norm_key;
use crate::fs::vfs::NameRules;
use crate::model::plan::{Action, Op, Side};
use crate::model::table::{ObservedEntry, TableArtifact};

use super::super::entries::observed_file;
use super::super::matching::files_equal;
use super::super::matching::name_rules::name_rules_of;
use super::super::policy::CompareOptions;

pub(super) fn apply(
    source: &TableArtifact,
    target: &TableArtifact,
    mode: &str,
    options: &CompareOptions,
    source_files: &BTreeMap<String, &ObservedEntry>,
    target_files: &BTreeMap<String, &ObservedEntry>,
    operations: &mut Vec<Op>,
) {
    let both_unix = name_rules_of(&source.header) != NameRules::Windows
        && name_rules_of(&target.header) != NameRules::Windows;
    if !options.sync_mode || !both_unix {
        return;
    }

    // Content-copy operations carry their source mode and need no second pass after landing.
    for operation in operations.iter_mut() {
        if matches!(operation.action, Action::Copy | Action::Update) && operation.link.is_none() {
            let key = norm_key(&operation.path, options.case_insensitive);
            let source_entry = match operation.side {
                Side::Target => source_files.get(&key),
                Side::Source => target_files.get(&key),
            };
            if let Some(entry) = source_entry {
                operation.mode = observed_file(entry).mode;
            }
        }
    }

    // Sync has no master for a mode-only difference, so it deliberately makes no attribution.
    if !matches!(mode, "mirror" | "enrich") {
        return;
    }
    for (path, source_entry) in source_files {
        let source_entry = *source_entry;
        if let Some(&target_entry) = target_files.get(path) {
            let source_file = observed_file(source_entry);
            let target_file = observed_file(target_entry);
            if files_equal(source_entry, target_entry, options.mtime_window_ms)
                && source_file.mode.is_some()
                && source_file.mode != target_file.mode
            {
                operations.push(Op {
                    side: Side::Target,
                    action: Action::Chmod,
                    path: target_file.path.as_str().to_owned(),
                    from: None,
                    size: None,
                    mtime_ms: None,
                    hash: None,
                    link: None,
                    mode: source_file.mode,
                    reason: format!(
                        "mode-differs-master-wins ({:04o} -> {:04o})",
                        target_file.mode.unwrap_or(0),
                        source_file.mode.unwrap_or(0)
                    ),
                });
            }
        }
    }
}
