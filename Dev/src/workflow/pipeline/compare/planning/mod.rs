//! Plan construction and ordered safety-policy passes.

mod conflict_policy;
mod names;
mod permissions;
mod symlinks;

use std::collections::HashMap;

use crate::foundation::time::now_ms;
use crate::model::plan::{Action, Op, Plan, PlanHeader, Side};
use crate::model::table::{
    FileIdentityObservation, ObservedEntry, ObservedEntryKind, TableArtifact,
};

use super::entries::{observed_file, observed_path, push_copy};
use super::matching::moves::{detect_moves, move_reason};
use super::matching::{evidence_missing, files_equal, generation_of, map_of};
use super::policy::CompareOptions;

pub fn compare(
    source: &TableArtifact,
    target: &TableArtifact,
    mode: &str,
    archive: Option<&TableArtifact>,
    resolve_newer: bool,
    copts: &CompareOptions,
) -> Plan {
    let ci = copts.case_insensitive;
    let win = copts.mtime_window_ms;
    let (s_files, s_dups) = map_of(source, ObservedEntryKind::File, ci);
    let (t_files, t_dups) = map_of(target, ObservedEntryKind::File, ci);
    let (s_dirs, _) = map_of(source, ObservedEntryKind::Directory, ci);
    let (t_dirs, _) = map_of(target, ObservedEntryKind::Directory, ci);
    let mut ops: Vec<Op> = Vec::new();

    for d in s_dups {
        ops.push(Op {
            side: Side::Source,
            action: Action::Note,
            path: d,
            from: None,
            size: None,
            mtime_ms: None,
            hash: None,
            link: None,
            mode: None,
            reason: "duplicate-after-normalization (kept first; NFC/case twin)".into(),
        });
    }
    for d in t_dups {
        ops.push(Op {
            side: Side::Target,
            action: Action::Note,
            path: d,
            from: None,
            size: None,
            mtime_ms: None,
            hash: None,
            link: None,
            mode: None,
            reason: "duplicate-after-normalization (kept first; NFC/case twin)".into(),
        });
    }

    match mode {
        "mirror" | "enrich" => {
            let mut adds: Vec<&ObservedEntry> = Vec::new();
            let mut dels: Vec<&ObservedEntry> = Vec::new();
            for (p, se) in &s_files {
                let se = *se;
                match t_files.get(p) {
                    None => adds.push(se),
                    Some(&te) => {
                        // One side's content could not be read, so there is no evidence to judge on.
                        // Not an Update: that would re-copy this file on every run for as long as
                        // the read keeps failing, and would silently overwrite the other side on the
                        // strength of a size-and-mtime guess. Conflicts are never auto-arbitrated
                        // here, and "I could not look" is exactly that case.
                        if evidence_missing(se, te) {
                            ops.push(Op {
                                side: Side::Target,
                                action: Action::Conflict,
                                path: observed_path(te),
                                from: None,
                                size: None,
                                mtime_ms: None,
                                hash: None,
                                link: None,
                                mode: None,
                                reason: "evidence-unavailable (content unreadable on one side)"
                                    .into(),
                            });
                        } else if !files_equal(se, te, win)
                            && (mode == "mirror"
                                || observed_file(se).mtime_ms > observed_file(te).mtime_ms + win)
                        {
                            let reason = if mode == "mirror" {
                                "differs-master-wins"
                            } else {
                                "source-newer"
                            };
                            // The update writes onto a file that already exists on target: open it with target's own spelling, don't rewrite the other side's form
                            ops.push(Op {
                                side: Side::Target,
                                action: Action::Update,
                                path: observed_path(te),
                                from: None,
                                size: Some(observed_file(se).size),
                                mtime_ms: Some(observed_file(se).mtime_ms),
                                hash: observed_file(se).identity.plan_hash(),
                                link: None,
                                mode: None,
                                reason: reason.into(),
                            });
                        }
                    }
                }
            }
            if mode == "mirror" {
                for (p, te) in &t_files {
                    if !s_files.contains_key(p) {
                        dels.push(*te);
                    }
                }
                let (moves, rest_adds, rest_dels) = detect_moves(adds, dels);
                for m in moves {
                    let base = if m.rename_in_place {
                        "rename-detected-by-hash"
                    } else {
                        "move-detected-by-hash"
                    };
                    let reason = move_reason(base, &m);
                    ops.push(Op {
                        side: Side::Target,
                        action: Action::Move,
                        path: m.to,
                        from: Some(m.from),
                        size: Some(m.size),
                        mtime_ms: Some(m.mtime_ms),
                        hash: Some(m.hash),
                        link: None,
                        mode: m.mode,
                        reason,
                    });
                }
                for a in rest_adds {
                    push_copy(&mut ops, Side::Target, a, "only-in-source");
                }
                for d in rest_dels {
                    ops.push(Op {
                        side: Side::Target,
                        action: Action::Delete,
                        path: observed_path(d),
                        from: None,
                        size: Some(observed_file(d).size),
                        mtime_ms: None,
                        hash: None,
                        link: None,
                        mode: None,
                        reason: "gone-from-source".into(),
                    });
                }
                for (p, te) in &t_dirs {
                    if !s_dirs.contains_key(p) {
                        ops.push(Op {
                            side: Side::Target,
                            action: Action::DeleteDir,
                            path: observed_path(te),
                            from: None,
                            size: None,
                            mtime_ms: None,
                            hash: None,
                            link: None,
                            mode: None,
                            reason: "dir-gone-from-source".into(),
                        });
                    }
                }
            } else {
                for a in adds {
                    push_copy(&mut ops, Side::Target, a, "only-in-source");
                }
            }
        }
        "sync" => {
            let (arch_files, _) = archive
                .map(|a| map_of(a, ObservedEntryKind::File, ci))
                .unwrap_or_default();
            let has_archive = archive.is_some();
            let mut s_adds: Vec<&ObservedEntry> = Vec::new();
            let mut t_adds: Vec<&ObservedEntry> = Vec::new();
            let mut del_on_target: Vec<&ObservedEntry> = Vec::new();
            let mut del_on_source: Vec<&ObservedEntry> = Vec::new();

            for (p, se) in &s_files {
                let se = *se;
                match t_files.get(p) {
                    Some(&te) => {
                        if evidence_missing(se, te) {
                            // Same reasoning as the mirror lane: with one side unreadable, neither
                            // the archive generations nor the mtime tiebreak below are standing on
                            // anything.
                            ops.push(Op {
                                side: Side::Target,
                                action: Action::Conflict,
                                path: observed_path(se),
                                from: None,
                                size: None,
                                mtime_ms: None,
                                hash: None,
                                link: None,
                                mode: None,
                                reason: "evidence-unavailable (content unreadable on one side)"
                                    .into(),
                            });
                            continue;
                        }
                        if files_equal(se, te, win) {
                            continue;
                        }
                        if has_archive {
                            // Historic generations distinguish a lagging side from a concurrent edit.
                            // One side merely being **behind** (stuck on some old version) is not a concurrent edit —
                            // syncthing achieves the same thing with PreviousBlocksHash
                            // (`lib/protocol/bep_fileinfo.go:200-207`).
                            // generation_of returns 0 = matches the archive's current generation, 1..n = the n-th historic generation.
                            let r = arch_files.get(p).copied();
                            let sg = r.and_then(|r| generation_of(se, r, win));
                            let tg = r.and_then(|r| generation_of(te, r, win));
                            let push_to_source = |ops: &mut Vec<Op>, why: &str| {
                                ops.push(Op {
                                    side: Side::Source,
                                    action: Action::Update,
                                    path: observed_path(se),
                                    from: None,
                                    size: Some(observed_file(te).size),
                                    mtime_ms: Some(observed_file(te).mtime_ms),
                                    hash: observed_file(te).identity.plan_hash(),
                                    link: None,
                                    mode: None,
                                    reason: why.into(),
                                });
                            };
                            let push_to_target = |ops: &mut Vec<Op>, why: &str| {
                                ops.push(Op {
                                    side: Side::Target,
                                    action: Action::Update,
                                    path: observed_path(te),
                                    from: None,
                                    size: Some(observed_file(se).size),
                                    mtime_ms: Some(observed_file(se).mtime_ms),
                                    hash: observed_file(se).identity.plan_hash(),
                                    link: None,
                                    mode: None,
                                    reason: why.into(),
                                });
                            };
                            match (sg, tg) {
                                // source sits on a known version, target has new content → target changed it
                                (Some(_), None) => push_to_source(&mut ops, "target-changed"),
                                (None, Some(_)) => push_to_target(&mut ops, "source-changed"),
                                // Both sides sit on known versions but at different generations → the newer generation wins; not a conflict
                                (Some(a), Some(b)) if a < b => {
                                    push_to_target(&mut ops, "target-behind-by-generations")
                                }
                                (Some(a), Some(b)) if a > b => {
                                    push_to_source(&mut ops, "source-behind-by-generations")
                                }
                                // Neither side's content was ever seen by the archive → a genuine concurrent edit
                                _ => ops.push(Op {
                                    side: Side::Target,
                                    action: Action::Conflict,
                                    path: observed_path(se),
                                    from: None,
                                    size: None,
                                    mtime_ms: None,
                                    hash: None,
                                    link: None,
                                    mode: None,
                                    reason: "both-changed".into(),
                                }),
                            }
                        } else if resolve_newer {
                            if observed_file(se).mtime_ms >= observed_file(te).mtime_ms {
                                ops.push(Op {
                                    side: Side::Target,
                                    action: Action::Update,
                                    path: observed_path(te),
                                    from: None,
                                    size: Some(observed_file(se).size),
                                    mtime_ms: Some(observed_file(se).mtime_ms),
                                    hash: observed_file(se).identity.plan_hash(),
                                    link: None,
                                    mode: None,
                                    reason: "differs-newer-wins".into(),
                                });
                            } else {
                                ops.push(Op {
                                    side: Side::Source,
                                    action: Action::Update,
                                    path: observed_path(se),
                                    from: None,
                                    size: Some(observed_file(te).size),
                                    mtime_ms: Some(observed_file(te).mtime_ms),
                                    hash: observed_file(te).identity.plan_hash(),
                                    link: None,
                                    mode: None,
                                    reason: "differs-newer-wins".into(),
                                });
                            }
                        } else {
                            ops.push(Op {
                                side: Side::Target,
                                action: Action::Conflict,
                                path: observed_path(se),
                                from: None,
                                size: None,
                                mtime_ms: None,
                                hash: None,
                                link: None,
                                mode: None,
                                reason: "differs-no-archive".into(),
                            });
                        }
                    }
                    None => {
                        if has_archive {
                            if let Some(&r) = arch_files.get(p) {
                                if files_equal(se, r, win) {
                                    del_on_source.push(se);
                                } else {
                                    ops.push(Op {
                                        side: Side::Target,
                                        action: Action::Conflict,
                                        path: observed_path(se),
                                        from: None,
                                        size: None,
                                        mtime_ms: None,
                                        hash: None,
                                        link: None,
                                        mode: None,
                                        reason: "deleted-on-target-but-changed-on-source".into(),
                                    });
                                }
                                continue;
                            }
                        }
                        s_adds.push(se);
                    }
                }
            }
            for (p, te) in &t_files {
                let te = *te;
                if s_files.contains_key(p) {
                    continue;
                }
                if has_archive {
                    if let Some(&r) = arch_files.get(p) {
                        if files_equal(te, r, win) {
                            del_on_target.push(te);
                        } else {
                            ops.push(Op {
                                side: Side::Target,
                                action: Action::Conflict,
                                path: observed_path(te),
                                from: None,
                                size: None,
                                mtime_ms: None,
                                hash: None,
                                link: None,
                                mode: None,
                                reason: "deleted-on-source-but-changed-on-target".into(),
                            });
                        }
                        continue;
                    }
                }
                t_adds.push(te);
            }

            if has_archive {
                let (mv_on_target, rest_s_adds, rest_del_t) = detect_moves(s_adds, del_on_target);
                for m in mv_on_target {
                    let base = if m.rename_in_place {
                        "rename-on-source-replayed"
                    } else {
                        "move-on-source-replayed"
                    };
                    let reason = move_reason(base, &m);
                    ops.push(Op {
                        side: Side::Target,
                        action: Action::Move,
                        path: m.to,
                        from: Some(m.from),
                        size: Some(m.size),
                        mtime_ms: Some(m.mtime_ms),
                        hash: Some(m.hash),
                        link: None,
                        mode: m.mode,
                        reason,
                    });
                }
                let (mv_on_source, rest_t_adds, rest_del_s) = detect_moves(t_adds, del_on_source);
                for m in mv_on_source {
                    let base = if m.rename_in_place {
                        "rename-on-target-replayed"
                    } else {
                        "move-on-target-replayed"
                    };
                    let reason = move_reason(base, &m);
                    ops.push(Op {
                        side: Side::Source,
                        action: Action::Move,
                        path: m.to,
                        from: Some(m.from),
                        size: Some(m.size),
                        mtime_ms: Some(m.mtime_ms),
                        hash: Some(m.hash),
                        link: None,
                        mode: m.mode,
                        reason,
                    });
                }
                for a in rest_s_adds {
                    push_copy(&mut ops, Side::Target, a, "added-on-source");
                }
                for a in rest_t_adds {
                    push_copy(&mut ops, Side::Source, a, "added-on-target");
                }
                for d in rest_del_t {
                    ops.push(Op {
                        side: Side::Target,
                        action: Action::Delete,
                        path: observed_path(d),
                        from: None,
                        size: Some(observed_file(d).size),
                        mtime_ms: None,
                        hash: None,
                        link: None,
                        mode: None,
                        reason: "deleted-on-source".into(),
                    });
                }
                for d in rest_del_s {
                    ops.push(Op {
                        side: Side::Source,
                        action: Action::Delete,
                        path: observed_path(d),
                        from: None,
                        size: Some(observed_file(d).size),
                        mtime_ms: None,
                        hash: None,
                        link: None,
                        mode: None,
                        reason: "deleted-on-target".into(),
                    });
                }
            } else {
                let t_only: HashMap<&str, &str> = t_adds
                    .iter()
                    .filter_map(|entry| {
                        let file = observed_file(entry);
                        match &file.identity {
                            FileIdentityObservation::FullBlake3 { digest } => {
                                Some((digest.as_str(), file.path.as_str()))
                            }
                            FileIdentityObservation::SizeAndMtime
                            | FileIdentityObservation::SampledBlake3 { .. }
                            | FileIdentityObservation::Unreadable => None,
                        }
                    })
                    .collect();
                for a in &s_adds {
                    let file = observed_file(a);
                    if let FileIdentityObservation::FullBlake3 { digest } = &file.identity {
                        if let Some(&other) = t_only.get(digest.as_str()) {
                            ops.push(Op {
                                side: Side::Target,
                                action: Action::Note,
                                path: file.path.as_str().to_owned(),
                                from: Some(other.to_string()),
                                size: Some(file.size),
                                mtime_ms: None,
                                hash: None,
                                link: None,
                                mode: None,
                                reason: "possible-move-needs-archive".into(),
                            });
                        }
                    }
                }
                for a in s_adds {
                    push_copy(&mut ops, Side::Target, a, "only-in-source");
                }
                for a in t_adds {
                    push_copy(&mut ops, Side::Source, a, "only-in-target");
                }
                for d in del_on_target {
                    ops.push(Op {
                        side: Side::Target,
                        action: Action::Delete,
                        path: observed_path(d),
                        from: None,
                        size: Some(observed_file(d).size),
                        mtime_ms: None,
                        hash: None,
                        link: None,
                        mode: None,
                        reason: "deleted-on-source".into(),
                    });
                }
                for d in del_on_source {
                    ops.push(Op {
                        side: Side::Source,
                        action: Action::Delete,
                        path: observed_path(d),
                        from: None,
                        size: Some(observed_file(d).size),
                        mtime_ms: None,
                        hash: None,
                        link: None,
                        mode: None,
                        reason: "deleted-on-target".into(),
                    });
                }
            }
        }
        other => panic!("unknown mode: {other}"),
    }

    symlinks::plan(source, target, mode, ci, &mut ops);

    permissions::apply(source, target, mode, copts, &s_files, &t_files, &mut ops);

    names::reject_case_collisions(source, target, ci, &mut ops);

    conflict_policy::resolve(source, target, copts, &s_files, &t_files, &mut ops);

    names::validate_backend_legality(source, target, &mut ops);

    let rank = |o: &Op| match o.action {
        Action::Move => 0,
        Action::Copy | Action::Update => 1,
        Action::Chmod => 2,
        Action::Delete => 3,
        Action::DeleteDir => 4,
        Action::Conflict | Action::Note => 5,
    };
    ops.sort_by(|a, b| {
        rank(a).cmp(&rank(b)).then_with(|| {
            if a.action == Action::DeleteDir {
                b.path
                    .matches('/')
                    .count()
                    .cmp(&a.path.matches('/').count())
            } else {
                a.path.cmp(&b.path)
            }
        })
    });

    let conflict_count = ops.iter().filter(|o| o.action == Action::Conflict).count() as u64;
    Plan {
        header: PlanHeader {
            schema: crate::model::plan::PLAN_SCHEMA,
            kind: "plan".into(),
            mode: mode.into(),
            generated_at_ms: now_ms(),
            source_root: source.header.root.clone(),
            source_host: source.header.host.clone(),
            target_root: target.header.root.clone(),
            target_host: target.header.host.clone(),
            op_count: ops.len() as u64,
            conflict_count,
            source_entries: source.entries.len() as u64,
            target_entries: target.entries.len() as u64,
            source_excluded: source.header.excluded_dirs + source.header.excluded_files,
            target_excluded: target.header.excluded_dirs + target.header.excluded_files,
            source_walk_errors: source.header.walk_errors,
            target_walk_errors: target.header.walk_errors,
            source_walk_err_samples: source.header.walk_err_samples.clone(),
            target_walk_err_samples: target.header.walk_err_samples.clone(),
            source_icloud_stubs: source.header.icloud_stubs,
            target_icloud_stubs: target.header.icloud_stubs,
            source_icloud_stub_samples: source.header.icloud_stub_samples.clone(),
            target_icloud_stub_samples: target.header.icloud_stub_samples.clone(),
        },
        ops,
    }
}
