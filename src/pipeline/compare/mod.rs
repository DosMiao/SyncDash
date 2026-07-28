//! compare: two snapshots in, a plan out — the middle of the three stages.
//!
//! Modes: `mirror` makes target match source; `sync` merges both ways using the archive as the
//! record of what was last agreed; `enrich` only ever adds. The decision for one path is made
//! once, from the evidence both snapshots carry, and every op records the reason it exists.
//!
//! Around the decision itself:
//! - `keys` — how two entries are judged equal, and how a path is keyed across platforms
//! - `moves` — pairing a delete with a copy of identical content into one move
//! - `winnames` — Windows naming legality, checked at plan time rather than at write time
//! - `evidence` — the read-only layer the UI reads; `compare` itself is unaffected by it

pub mod evidence;
pub mod keys;
pub mod moves;
pub mod winnames;

use std::collections::{BTreeMap, HashMap};

use crate::foundation::names::CONFLICT_INFIX;
use crate::foundation::path::{base_name, split_ext, split_parent};
use crate::foundation::text::{fold, safe_host};
use crate::foundation::text::norm_key;
use crate::foundation::time::stamp_compact;
use crate::foundation::time::now_ms;
use crate::model::plan::{Action, Op, Plan, PlanHeader, Side, MTIME_SLACK_MS};
use crate::model::table::{Entry, EntryKind, Snapshot};

use keys::{evidence_missing, files_equal, generation_of, map_of};
use moves::{detect_moves, move_reason};
use winnames::{name_rules_of, win_name_fault, WinNameFault};
use crate::fs::vfs::NameRules;

/// Conflict handling policy. Default is Report (report only, never arbitrate automatically) — this is
/// what SyncDash stands on; aligning with syncthing does not change the default.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ConflictPolicy {
    /// Report only; a human handles it
    Report,
    /// The loser is renamed to `<name>.sync-conflict-<ts>-<host><ext>`, the winner lands
    /// (syncthing `conflictName`, `lib/model/folder_sendrecv.go:2219`)
    Copy,
    /// Newer mtime wins; the older one is simply overwritten (no copy kept)
    Newer,
}

#[derive(Clone, Copy)]
pub struct CompareOptions {
    /// Default true: NTFS and APFS are both case-insensitive by default
    pub case_insensitive: bool,
    /// Conflict policy
    pub conflict: ConflictPolicy,
    /// Sync unix permission bits (only meaningful when both sides are unix; the Win side has no mode, so enabling it would report a difference forever)
    pub sync_mode: bool,
    /// How many conflict copies to keep per file at most (-1 = unlimited). Only effective for ConflictPolicy::Copy
    pub max_conflicts: i32,
    /// The no-hash equality window on mtime, in ms. Defaults to MTIME_SLACK_MS (FAT/SMB granularity);
    /// a remote backend with coarser timestamps (FTP LIST = minutes) widens it to its declared precision.
    /// Only the *hashless* judgment uses it — content evidence always wins over timestamps.
    pub mtime_window_ms: i64,
}

impl Default for CompareOptions {
    fn default() -> Self {
        CompareOptions {
            case_insensitive: true,
            conflict: ConflictPolicy::Report,
            sync_mode: false,
            max_conflicts: 5,
            mtime_window_ms: MTIME_SLACK_MS,
        }
    }
}

/// Conflict-copy name: `report.pdf` → `report.sync-conflict-20260726-143000-WIN01.pdf`
/// (isomorphic to syncthing's naming, so a human recognizes it at a glance and both sides' filters can spot it)
pub fn conflict_name(path: &str, host: &str, at_ms: u64) -> String {
    let (dir, base) = split_parent(path);
    // split_ext only recognizes the extension after the last dot; a hidden file (.gitignore) counts wholly as the stem
    let (stem, ext) = split_ext(base);
    let ts = stamp_compact(at_ms as i64);
    let host = safe_host(host);
    format!("{dir}{stem}{CONFLICT_INFIX}{ts}-{host}{ext}")
}

/// A conflict copy must not itself take part in sync/conflict decisions (syncthing `isConflict`, :2224)
pub fn is_conflict_copy(path: &str) -> bool {
    base_name(path).contains(CONFLICT_INFIX)
}

fn push_copy(ops: &mut Vec<Op>, side: Side, e: &Entry, reason: &str) {
    ops.push(Op { side, action: Action::Copy, path: e.path.clone(), from: None, size: Some(e.size), mtime_ms: Some(e.mtime_ms), hash: e.hash.clone(), link: None, mode: None, reason: reason.into() });
}

pub fn compare(source: &Snapshot, target: &Snapshot, mode: &str, archive: Option<&Snapshot>, resolve_newer: bool, copts: &CompareOptions) -> Plan {
    let ci = copts.case_insensitive;
    let win = copts.mtime_window_ms;
    let (s_files, s_dups) = map_of(source, EntryKind::File, ci);
    let (t_files, t_dups) = map_of(target, EntryKind::File, ci);
    let (s_dirs, _) = map_of(source, EntryKind::Dir, ci);
    let (t_dirs, _) = map_of(target, EntryKind::Dir, ci);
    let both_hashed = source.header.hashed && target.header.hashed;
    let mut ops: Vec<Op> = Vec::new();

    for d in s_dups {
        ops.push(Op { side: Side::Source, action: Action::Note, path: d, from: None, size: None, mtime_ms: None, hash: None, link: None, mode: None, reason: "duplicate-after-normalization (kept first; NFC/case twin)".into() });
    }
    for d in t_dups {
        ops.push(Op { side: Side::Target, action: Action::Note, path: d, from: None, size: None, mtime_ms: None, hash: None, link: None, mode: None, reason: "duplicate-after-normalization (kept first; NFC/case twin)".into() });
    }

    match mode {
        "mirror" | "enrich" => {
            let mut adds: Vec<&Entry> = Vec::new();
            let mut dels: Vec<&Entry> = Vec::new();
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
                            ops.push(Op { side: Side::Target, action: Action::Conflict, path: te.path.clone(), from: None, size: None, mtime_ms: None, hash: None, link: None, mode: None, reason: "evidence-unavailable (content unreadable on one side)".into() });
                        } else if !files_equal(se, te, win) && (mode == "mirror" || se.mtime_ms > te.mtime_ms + win) {
                            let reason = if mode == "mirror" { "differs-master-wins" } else { "source-newer" };
                            // The update writes onto a file that already exists on target: open it with target's own spelling, don't rewrite the other side's form
                            ops.push(Op { side: Side::Target, action: Action::Update, path: te.path.clone(), from: None, size: Some(se.size), mtime_ms: Some(se.mtime_ms), hash: se.hash.clone(), link: None, mode: None, reason: reason.into() });
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
                let (moves, rest_adds, rest_dels) = if both_hashed {
                    detect_moves(adds, dels)
                } else {
                    (Vec::new(), adds, dels)
                };
                for m in moves {
                    let base = if m.rename_in_place { "rename-detected-by-hash" } else { "move-detected-by-hash" };
                    let reason = move_reason(base, &m);
                    ops.push(Op { side: Side::Target, action: Action::Move, path: m.to, from: Some(m.from), size: Some(m.size), mtime_ms: None, hash: None, link: None, mode: None, reason });
                }
                for a in rest_adds { push_copy(&mut ops, Side::Target, a, "only-in-source"); }
                for d in rest_dels {
                    ops.push(Op { side: Side::Target, action: Action::Delete, path: d.path.clone(), from: None, size: Some(d.size), mtime_ms: None, hash: None, link: None, mode: None, reason: "gone-from-source".into() });
                }
                for (p, te) in &t_dirs {
                    if !s_dirs.contains_key(p) {
                        ops.push(Op { side: Side::Target, action: Action::DeleteDir, path: te.path.clone(), from: None, size: None, mtime_ms: None, hash: None, link: None, mode: None, reason: "dir-gone-from-source".into() });
                    }
                }
            } else {
                for a in adds { push_copy(&mut ops, Side::Target, a, "only-in-source"); }
            }
        }
        "sync" => {
            let (arch_files, _) = archive.map(|a| map_of(a, EntryKind::File, ci)).unwrap_or_default();
            let has_archive = archive.is_some();
            let mut s_adds: Vec<&Entry> = Vec::new();
            let mut t_adds: Vec<&Entry> = Vec::new();
            let mut del_on_target: Vec<&Entry> = Vec::new();
            let mut del_on_source: Vec<&Entry> = Vec::new();

            for (p, se) in &s_files {
                let se = *se;
                match t_files.get(p) {
                    Some(&te) => {
                        if evidence_missing(se, te) {
                            // Same reasoning as the mirror lane: with one side unreadable, neither
                            // the archive generations nor the mtime tiebreak below are standing on
                            // anything.
                            ops.push(Op { side: Side::Target, action: Action::Conflict, path: se.path.clone(), from: None, size: None, mtime_ms: None, hash: None, link: None, mode: None, reason: "evidence-unavailable (content unreadable on one side)".into() });
                            continue;
                        }
                        if files_equal(se, te, win) {
                            continue;
                        }
                        if has_archive {
                            // P1-3: don't only ask "is it equal to the archive's current generation" — look at historic generations too.
                            // One side merely being **behind** (stuck on some old version) is not a concurrent edit —
                            // syncthing achieves the same thing with PreviousBlocksHash
                            // (`lib/protocol/bep_fileinfo.go:200-207`).
                            // generation_of returns 0 = matches the archive's current generation, 1..n = the n-th historic generation.
                            let r = arch_files.get(p).copied();
                            let sg = r.and_then(|r| generation_of(se, r, win));
                            let tg = r.and_then(|r| generation_of(te, r, win));
                            let push_to_source = |ops: &mut Vec<Op>, why: &str| {
                                ops.push(Op { side: Side::Source, action: Action::Update, path: se.path.clone(), from: None, size: Some(te.size), mtime_ms: Some(te.mtime_ms), hash: te.hash.clone(), link: None, mode: None, reason: why.into() });
                            };
                            let push_to_target = |ops: &mut Vec<Op>, why: &str| {
                                ops.push(Op { side: Side::Target, action: Action::Update, path: te.path.clone(), from: None, size: Some(se.size), mtime_ms: Some(se.mtime_ms), hash: se.hash.clone(), link: None, mode: None, reason: why.into() });
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
                                _ => ops.push(Op { side: Side::Target, action: Action::Conflict, path: se.path.clone(), from: None, size: None, mtime_ms: None, hash: None, link: None, mode: None, reason: "both-changed".into() }),
                            }
                        } else if resolve_newer {
                            if se.mtime_ms >= te.mtime_ms {
                                ops.push(Op { side: Side::Target, action: Action::Update, path: te.path.clone(), from: None, size: Some(se.size), mtime_ms: Some(se.mtime_ms), hash: se.hash.clone(), link: None, mode: None, reason: "differs-newer-wins".into() });
                            } else {
                                ops.push(Op { side: Side::Source, action: Action::Update, path: se.path.clone(), from: None, size: Some(te.size), mtime_ms: Some(te.mtime_ms), hash: te.hash.clone(), link: None, mode: None, reason: "differs-newer-wins".into() });
                            }
                        } else {
                            ops.push(Op { side: Side::Target, action: Action::Conflict, path: se.path.clone(), from: None, size: None, mtime_ms: None, hash: None, link: None, mode: None, reason: "differs-no-archive".into() });
                        }
                    }
                    None => {
                        if has_archive {
                            if let Some(&r) = arch_files.get(p) {
                                if files_equal(se, r, win) {
                                    del_on_source.push(se);
                                } else {
                                    ops.push(Op { side: Side::Target, action: Action::Conflict, path: se.path.clone(), from: None, size: None, mtime_ms: None, hash: None, link: None, mode: None, reason: "deleted-on-target-but-changed-on-source".into() });
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
                            ops.push(Op { side: Side::Target, action: Action::Conflict, path: te.path.clone(), from: None, size: None, mtime_ms: None, hash: None, link: None, mode: None, reason: "deleted-on-source-but-changed-on-target".into() });
                        }
                        continue;
                    }
                }
                t_adds.push(te);
            }

            if has_archive && both_hashed {
                let (mv_on_target, rest_s_adds, rest_del_t) = detect_moves(s_adds, del_on_target);
                for m in mv_on_target {
                    let base = if m.rename_in_place { "rename-on-source-replayed" } else { "move-on-source-replayed" };
                    let reason = move_reason(base, &m);
                    ops.push(Op { side: Side::Target, action: Action::Move, path: m.to, from: Some(m.from), size: Some(m.size), mtime_ms: None, hash: None, link: None, mode: None, reason });
                }
                let (mv_on_source, rest_t_adds, rest_del_s) = detect_moves(t_adds, del_on_source);
                for m in mv_on_source {
                    let base = if m.rename_in_place { "rename-on-target-replayed" } else { "move-on-target-replayed" };
                    let reason = move_reason(base, &m);
                    ops.push(Op { side: Side::Source, action: Action::Move, path: m.to, from: Some(m.from), size: Some(m.size), mtime_ms: None, hash: None, link: None, mode: None, reason });
                }
                for a in rest_s_adds { push_copy(&mut ops, Side::Target, a, "added-on-source"); }
                for a in rest_t_adds { push_copy(&mut ops, Side::Source, a, "added-on-target"); }
                for d in rest_del_t {
                    ops.push(Op { side: Side::Target, action: Action::Delete, path: d.path.clone(), from: None, size: Some(d.size), mtime_ms: None, hash: None, link: None, mode: None, reason: "deleted-on-source".into() });
                }
                for d in rest_del_s {
                    ops.push(Op { side: Side::Source, action: Action::Delete, path: d.path.clone(), from: None, size: Some(d.size), mtime_ms: None, hash: None, link: None, mode: None, reason: "deleted-on-target".into() });
                }
            } else {
                if both_hashed {
                    let t_only: HashMap<&str, &str> = t_adds.iter()
                        .filter_map(|e| e.hash.as_deref().map(|h| (h, e.path.as_str())))
                        .collect();
                    for a in &s_adds {
                        if let Some(h) = a.hash.as_deref() {
                            if let Some(&other) = t_only.get(h) {
                                ops.push(Op { side: Side::Target, action: Action::Note, path: a.path.clone(), from: Some(other.to_string()), size: Some(a.size), mtime_ms: None, hash: None, link: None, mode: None, reason: "possible-move-needs-archive".into() });
                            }
                        }
                    }
                }
                for a in s_adds { push_copy(&mut ops, Side::Target, a, "only-in-source"); }
                for a in t_adds { push_copy(&mut ops, Side::Source, a, "only-in-target"); }
                for d in del_on_target {
                    ops.push(Op { side: Side::Target, action: Action::Delete, path: d.path.clone(), from: None, size: Some(d.size), mtime_ms: None, hash: None, link: None, mode: None, reason: "deleted-on-source".into() });
                }
                for d in del_on_source {
                    ops.push(Op { side: Side::Source, action: Action::Delete, path: d.path.clone(), from: None, size: Some(d.size), mtime_ms: None, hash: None, link: None, mode: None, reason: "deleted-on-target".into() });
                }
            }
        }
        other => panic!("unknown mode: {other}"),
    }

    // symlinks (Symlink entries only exist in both tables when symlinks="direct")
    // Compared by equality of the "link target string"; mirror falls in line with master, enrich only fills gaps, sync fills gaps + reports differences as conflicts
    {
        let (s_links, _) = map_of(source, EntryKind::Symlink, ci);
        let (t_links, _) = map_of(target, EntryKind::Symlink, ci);
        let link_op = |side: Side, action: Action, e: &Entry, reason: &str| Op {
            side,
            action,
            path: e.path.clone(),
            from: None,
            size: None,
            mtime_ms: None,
            hash: None,
            link: e.link.clone(),
            mode: None,
            reason: reason.into(),
        };
        for (p, se) in &s_links {
            let se = *se;
            match t_links.get(p) {
                None => {
                    if mode == "mirror" || mode == "enrich" || mode == "sync" {
                        ops.push(link_op(Side::Target, Action::Copy, se, "symlink-only-in-source"));
                    }
                }
                Some(&te) => {
                    if se.link != te.link {
                        match mode {
                            "mirror" => ops.push(link_op(Side::Target, Action::Update, se, "symlink-differs-master-wins")),
                            "sync" => ops.push(link_op(Side::Target, Action::Conflict, se, "symlink-differs")),
                            _ => {}
                        }
                    }
                }
            }
        }
        for (p, te) in &t_links {
            let te = *te;
            if !s_links.contains_key(p) {
                match mode {
                    "mirror" => ops.push(link_op(Side::Target, Action::Delete, te, "symlink-gone-from-source")),
                    "sync" => ops.push(link_op(Side::Source, Action::Copy, te, "symlink-only-in-target")),
                    _ => {}
                }
            }
        }
    }

    // unix permission bits (P2-4)
    // Only done when both sides are unix and the job explicitly enabled sync_mode: the Windows side has no mode,
    // so leaving it on would report a difference on every compare. Previously mode was only recorded into the snapshot
    // table and never took part in the compare, so scripts synced over the mounted-drive path lost their exec bit (the pack path did restore it — the two paths behaved differently).
    // Same trap as the name-legality gate: `header.os` is the protocol on a VFS root, so a
    // comparison against "windows" read `smb` as unix and offered to sync mode bits onto an
    // NTFS share. Ask the naming/semantics field instead, and treat Unknown as "do not".
    // (Unknown stays in: an SFTP root does carry unix modes, we just cannot name its OS.
    // What must come out is a Windows-semantics root, which has no mode bits to write.)
    let both_unix = name_rules_of(&source.header) != NameRules::Windows
        && name_rules_of(&target.header) != NameRules::Windows;
    if copts.sync_mode && both_unix {
        // 1) Ops that copy content carry the target mode along; apply writes it back right after copying, no extra pass needed
        for op in &mut ops {
            if matches!(op.action, Action::Copy | Action::Update) && op.link.is_none() {
                let key = norm_key(&op.path, ci);
                let from_entry = match op.side {
                    Side::Target => s_files.get(&key),
                    Side::Source => t_files.get(&key),
                };
                if let Some(e) = from_entry {
                    op.mode = e.mode;
                }
            }
        }
        // 2) Same content, only permissions differ → a standalone Chmod; never retransmit a file over a few permission bits
        //    (the same idea as syncthing's shortcutFile). sync mode has no "who is master", so
        //    permission attribution is undecidable and the pass is skipped entirely there —
        //    a mode-only difference produces no op and no note under sync.
        if mode == "mirror" || mode == "enrich" {
            for (p, se) in &s_files {
                let se = *se;
                if let Some(&te) = t_files.get(p) {
                    if files_equal(se, te, win) && se.mode.is_some() && se.mode != te.mode {
                        ops.push(Op {
                            side: Side::Target,
                            action: Action::Chmod,
                            path: te.path.clone(),
                            from: None,
                            size: None,
                            mtime_ms: None,
                            hash: None,
                            link: None,
                            mode: se.mode,
                            reason: format!(
                                "mode-differs-master-wins ({:04o} -> {:04o})",
                                te.mode.unwrap_or(0),
                                se.mode.unwrap_or(0)
                            ),
                        });
                    }
                }
            }
        }
    }

    // Write-collision preflight in case-sensitive mode (P2-3)
    // With case_sensitive = true the compare key distinguishes case, but the underlying NTFS/APFS usually does **not**:
    // writing `Foo.txt` to target would silently overwrite the existing `foo.txt`. syncthing resolves
    // the directory's real name before writing and raises CaseConflictError (`lib/fs/casefs.go:27-37`); we catch it at plan time.
    if !ci {
        // fold = foundation::text::fold, the very same normalization implementation the compare key norm_key uses
        let mut folded: HashMap<(bool, String), Vec<&str>> = HashMap::new();
        for (is_target, snap) in [(false, source), (true, target)] {
            for e in &snap.entries {
                folded.entry((is_target, fold(&e.path))).or_default().push(&e.path);
            }
        }
        for op in &mut ops {
            if !matches!(op.action, Action::Copy | Action::Move) {
                continue;
            }
            let is_target = op.side == Side::Target;
            if let Some(existing) = folded.get(&(is_target, fold(&op.path))) {
                // When a Move's from IS that "colliding" file, this is precisely a **case rename**
                // (`readme.md` → `Readme.md`) — the correct product of move detection, not a collision accident.
                let from = op.from.as_deref();
                if let Some(other) = existing.iter().find(|p| **p != op.path && Some(**p) != from) {
                    op.action = Action::Conflict;
                    op.reason = format!(
                        "case-collision: writing '{}' would overwrite existing '{other}' on a \
                         case-insensitive filesystem (set case_sensitive = false, or rename one side)",
                        op.path
                    );
                }
            }
        }
    }

    // Conflict policy (P1-2)
    // Default Report: report only, a human handles it — this is what SyncDash stands on; unchanged.
    // Copy/Newer are explicit opt-ins, so that in everyday two-machine use one conflict doesn't wedge a file until the end of time.
    // Only **content conflicts** are handled (both sides have the file and both changed it); delete-vs-edit conflicts and
    // illegal-on-windows always stay Report — automatically arbitrating "delete or keep" is too dangerous.
    if copts.conflict != ConflictPolicy::Report {
        const RESOLVABLE: [&str; 3] = ["both-changed", "differs-no-archive", "symlink-differs"];
        let now = now_ms();
        let mut extra: Vec<Op> = Vec::new();
        for op in &mut ops {
            if op.action != Action::Conflict || !RESOLVABLE.contains(&op.reason.as_str()) {
                continue;
            }
            // A conflict copy never spawns another conflict copy (syncthing isConflict, :1863)
            if is_conflict_copy(&op.path) {
                continue;
            }
            let key = norm_key(&op.path, ci);
            let (Some(&se), Some(&te)) = (s_files.get(&key), t_files.get(&key)) else { continue };
            // Newer mtime wins; on an exact tie the host name's lexicographic order is a stable tie-break
            // (syncthing uses the device id from the version vector; we have none, so host is the equivalent)
            let source_wins = match se.mtime_ms.cmp(&te.mtime_ms) {
                std::cmp::Ordering::Greater => true,
                std::cmp::Ordering::Less => false,
                std::cmp::Ordering::Equal => source.header.host <= target.header.host,
            };
            let (winner, loser, loser_side, loser_host) = if source_wins {
                (se, te, Side::Target, target.header.host.as_str())
            } else {
                (te, se, Side::Source, source.header.host.as_str())
            };
            if copts.conflict == ConflictPolicy::Copy {
                let kept = conflict_name(&loser.path, loser_host, now);
                extra.push(Op {
                    side: loser_side.clone(),
                    action: Action::Move,
                    path: kept.clone(),
                    from: Some(loser.path.clone()),
                    size: Some(loser.size),
                    mtime_ms: Some(loser.mtime_ms),
                    hash: loser.hash.clone(),
                    link: None,
                    mode: None,
                    reason: format!("conflict-loser-kept-as-copy ({})", op.reason),
                });
            }
            // The winner's content lands on the loser's side
            extra.push(Op {
                side: loser_side,
                action: Action::Update,
                path: loser.path.clone(),
                from: None,
                size: Some(winner.size),
                mtime_ms: Some(winner.mtime_ms),
                hash: winner.hash.clone(),
                link: winner.link.clone(),
                mode: None,
                reason: format!(
                    "conflict-resolved-newer-wins ({})",
                    if copts.conflict == ConflictPolicy::Copy { "loser kept as .sync-conflict copy" } else { "loser overwritten (recoverable from trash)" }
                ),
            });
            // The original conflict row is downgraded to a note, leaving an auditable trace
            op.action = Action::Note;
            op.reason = format!("auto-resolved: {}", op.reason);
        }
        ops.extend(extra);

        // max_conflicts: when the conflict copies for one path exceed the limit, drop the oldest few
        // (syncthing `lib/model/folder_sendrecv.go:1888-1898`).
        // The copy name carries a timestamp, so lexicographic order is chronological order.
        if copts.conflict == ConflictPolicy::Copy && copts.max_conflicts >= 0 {
            let limit = copts.max_conflicts as usize;
            for (is_target, snap) in [(false, source), (true, target)] {
                let mut groups: BTreeMap<String, Vec<&str>> = BTreeMap::new();
                for e in snap.entries.iter().filter(|e| e.kind == EntryKind::File) {
                    if is_conflict_copy(&e.path) {
                        // Group under the original file name: strip off the `.sync-conflict-…` part
                        if let Some(i) = e.path.find(CONFLICT_INFIX) {
                            let stem = &e.path[..i];
                            groups.entry(stem.to_string()).or_default().push(&e.path);
                        }
                    }
                }
                for (_stem, mut copies) in groups {
                    if copies.len() <= limit {
                        continue;
                    }
                    copies.sort_unstable(); // the timestamp is in the name → lexicographic = chronological
                    let doomed = copies.len() - limit;
                    for p in copies.into_iter().take(doomed) {
                        ops.push(Op {
                            side: if is_target { Side::Target } else { Side::Source },
                            action: Action::Delete,
                            path: p.to_string(),
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
    }

    // Name-legality preflight, caught at plan time so apply never blows up — or worse, quietly
    // succeeds against the wrong file — halfway through.
    //
    // Scope is deliberately wider than "the path being created". A `Mangled` path does not
    // address the file it names, so *reading* the source is just as wrong as writing the
    // target, and a delete of it removes a different file while reporting success. So each op
    // is checked against every root it touches: the executing side always, and the reading
    // side too whenever content moves.
    let mut unknown_rule_notes: Vec<(Side, String, String)> = Vec::new();
    for op in &mut ops {
        let (exec_rules, other_rules) = match op.side {
            Side::Target => (name_rules_of(&target.header), name_rules_of(&source.header)),
            Side::Source => (name_rules_of(&source.header), name_rules_of(&target.header)),
        };
        // Only Copy/Move bring a *new* name into existence. An Update means the file is
        // already sitting there and is addressable, so refusing it over a reserved name would
        // strand it un-synced forever — the same reasoning that lets a delete of one proceed.
        let creates = matches!(op.action, Action::Copy | Action::Move);
        let reads_other = matches!(op.action, Action::Copy | Action::Update);

        // `from` is the pre-move spelling and lives on the executing side too
        let candidates = [Some(op.path.as_str()), op.from.as_deref()];
        let mut verdict: Option<(bool, String)> = None; // (refuse, reason)
        for path in candidates.into_iter().flatten() {
            let Some((fault, r)) = win_name_fault(path) else { continue };
            // Which roots does this fault actually endanger for this op?
            let exec_hit = exec_rules == NameRules::Windows
                && (fault == WinNameFault::Mangled || creates);
            let read_hit = reads_other && other_rules == NameRules::Windows && fault == WinNameFault::Mangled;
            if exec_hit || read_hit {
                let where_ = if read_hit && !exec_hit { "reading side" } else { "executing side" };
                verdict = Some((true, format!("{r} ({where_})")));
                break;
            }
            // Unknown-OS roots: never refuse (the name may be perfectly legal there), never
            // stay quiet either.
            let exec_unknown = exec_rules == NameRules::Unknown && (fault == WinNameFault::Mangled || creates);
            let read_unknown = reads_other && other_rules == NameRules::Unknown && fault == WinNameFault::Mangled;
            if (exec_unknown || read_unknown) && verdict.is_none() {
                verdict = Some((false, r));
            }
        }
        match verdict {
            Some((true, reason)) => {
                op.action = Action::Conflict;
                op.reason = format!("illegal-on-windows: {reason}");
            }
            Some((false, reason)) => unknown_rule_notes.push((op.side.clone(), op.path.clone(), reason)),
            None => {}
        }
    }
    for (side, path, r) in unknown_rule_notes {
        ops.push(Op {
            side,
            action: Action::Note,
            path,
            from: None,
            size: None,
            mtime_ms: None,
            hash: None,
            link: None,
            mode: None,
            reason: format!("name-risk-on-unknown-server: {r} — this root's OS cannot be determined over its protocol, so the operation is attempted as planned"),
        });
    }

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
                b.path.matches('/').count().cmp(&a.path.matches('/').count())
            } else {
                a.path.cmp(&b.path)
            }
        })
    });

    let conflict_count = ops.iter().filter(|o| o.action == Action::Conflict).count() as u64;
    Plan {
        header: PlanHeader {
            schema: crate::model::table::SCHEMA,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::table::{Header, SCHEMA};

    fn snap(os: &str, entries: Vec<Entry>) -> Snapshot {
        Snapshot {
            header: Header {
                schema: SCHEMA, kind: "snapshot".into(), root: "/r".into(), host: "h".into(),
                os: os.into(), scanned_at_ms: 0, duration_ms: 0,
                entry_count: entries.len() as u64, hashed: true,
                excluded_dirs: 0, excluded_files: 0,
                walk_errors: 0, walk_err_samples: Vec::new(),
                icloud_stubs: 0, icloud_stub_samples: Vec::new(), dataless_files: 0, vfs: None,
            },
            entries,
        }
    }
    fn file(path: &str, hash: &str) -> Entry {
        Entry { path: path.into(), kind: EntryKind::File, size: 1, mtime_ms: 0, hash: Some(hash.into()), hash_failed: false, file_id: None, mode: None, link: None, prev: None }
    }
    /// A file whose content could not be read: same size and mtime as its twin, no hash.
    fn unreadable(path: &str) -> Entry {
        Entry { hash: None, hash_failed: true, ..file(path, "") }
    }

    /// The exact shape that used to pass silently: identical size and mtime, no hash on one side
    /// because the read failed, so `files_equal` fell through to the size+mtime line and declared
    /// them the same file — forever, since the read keeps failing. A restore, `touch -r`, or an SMB
    /// mtime round-trip all produce changed content under a preserved size and mtime.
    #[test]
    fn an_unreadable_file_is_a_conflict_not_an_equality() {
        let s = snap("linux", vec![unreadable("a.bin")]);
        let t = snap("linux", vec![file("a.bin", "deadbeef")]);
        let plan = compare(&s, &t, "mirror", None, false, &CompareOptions::default());
        let ops: Vec<_> = plan.ops.iter().filter(|o| o.path == "a.bin").collect();
        assert_eq!(ops.len(), 1, "exactly one op for the pair: {:?}", plan.ops);
        assert_eq!(ops[0].action, Action::Conflict, "unreadable content must not resolve to equal or to a blind update");
        assert!(ops[0].reason.contains("evidence-unavailable"), "{}", ops[0].reason);
    }

    /// The same pair with the read succeeding must go back to being ordinary — the guard must not
    /// fire on every hashless comparison, only on a failed one.
    #[test]
    fn a_hashless_comparison_is_still_judged_on_size_and_mtime() {
        let bare = |p: &str| Entry { hash: None, hash_failed: false, ..file(p, "") };
        let s = snap("linux", vec![bare("a.bin")]);
        let t = snap("linux", vec![bare("a.bin")]);
        let plan = compare(&s, &t, "mirror", None, false, &CompareOptions::default());
        assert!(
            !plan.ops.iter().any(|o| o.path == "a.bin"),
            "equal size and mtime with hashing switched off is equality, not a conflict: {:?}",
            plan.ops
        );
    }
    /// A file with an mtime (conflict arbitration goes by mtime)
    fn file_at(path: &str, hash: &str, mtime_ms: i64) -> Entry {
        Entry { mtime_ms, ..file(path, hash) }
    }
    fn sized(path: &str, hash: &str, size: u64) -> Entry {
        Entry { size, ..file(path, hash) }
    }
    /// An archive entry: current hash + historic generations
    fn arch(path: &str, hash: &str, prev: &[&str]) -> Entry {
        Entry {
            prev: if prev.is_empty() { None } else { Some(prev.iter().map(|s| s.to_string()).collect()) },
            ..file(path, hash)
        }
    }
    fn snap_named(os: &str, host: &str, entries: Vec<Entry>) -> Snapshot {
        let mut s = snap(os, entries);
        s.header.host = host.into();
        s
    }
    /// A snapshot of a VFS root: `header.os` carries the *protocol*, and the naming rules
    /// live in the VfsNote — exactly the shape `scan_vfs` writes.
    fn snap_vfs(protocol: &str, name_rules: &str, entries: Vec<Entry>) -> Snapshot {
        let mut s = snap(protocol, entries);
        s.header.vfs = Some(crate::model::table::VfsNote {
            protocol: protocol.into(),
            display_root: "/r".into(),
            mtime_precision_ms: 1,
            medium: crate::fs::vfs::Medium::NetworkShare.as_str().into(),
            evidence_effective: "full".into(),
            name_rules: name_rules.into(),
            degraded: Vec::new(),
        });
        s
    }
    fn actions(plan: &Plan) -> Vec<(&str, &str)> {
        plan.ops
            .iter()
            .map(|o| {
                (
                    match o.action {
                        Action::Copy => "copy",
                        Action::Update => "update",
                        Action::Move => "move",
                        Action::Delete => "delete",
                        Action::DeleteDir => "deletedir",
                        Action::Chmod => "chmod",
                        Action::Conflict => "conflict",
                        Action::Note => "note",
                    },
                    o.path.as_str(),
                )
            })
            .collect()
    }

    // P2-5: empty files / ambiguous pairing

    #[test]
    fn empty_files_are_never_paired_as_moves() {
        // Every zero-length file has the same blake3. They used to get paired into a pile of "renames" —
        // the resulting content was right, but the attribution was invented. syncthing simply excludes Size == 0 in findRename.
        let e = "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262";
        let s = snap("windows", vec![sized("new/a.py", e, 0), sized("new/b.py", e, 0)]);
        let t = snap("windows", vec![sized("old/x.py", e, 0), sized("old/y.py", e, 0)]);
        let plan = compare(&s, &t, "mirror", None, false, &CompareOptions::default());
        assert!(
            !plan.ops.iter().any(|o| o.action == Action::Move),
            "zero-length files must never be paired as renames: {:?}",
            actions(&plan)
        );
        assert_eq!(plan.ops.iter().filter(|o| o.action == Action::Copy).count(), 2);
        assert_eq!(plan.ops.iter().filter(|o| o.action == Action::Delete).count(), 2);
    }

    #[test]
    fn ambiguous_move_is_labeled_as_such() {
        // Several candidates with the same content: the pairing's content is correct, but from is picked arbitrarily — reason must tell the truth
        let s = snap("windows", vec![sized("moved/one.bin", "h", 10)]);
        let t = snap(
            "windows",
            vec![sized("a/one.bin", "h", 10), sized("b/one.bin", "h", 10), sized("c/one.bin", "h", 10)],
        );
        let plan = compare(&s, &t, "mirror", None, false, &CompareOptions::default());
        let mv = plan.ops.iter().find(|o| o.action == Action::Move).expect("should still pair one");
        assert!(mv.reason.contains("ambiguous"), "reason must admit the ambiguity, got {:?}", mv.reason);
        assert!(mv.reason.contains('3'), "and say how many candidates: {:?}", mv.reason);
    }

    #[test]
    fn unambiguous_move_stays_clean() {
        let s = snap("windows", vec![sized("moved/one.bin", "h", 10)]);
        let t = snap("windows", vec![sized("one.bin", "h", 10)]);
        let plan = compare(&s, &t, "mirror", None, false, &CompareOptions::default());
        let mv = plan.ops.iter().find(|o| o.action == Action::Move).unwrap();
        assert!(!mv.reason.contains("ambiguous"), "a single candidate must not be flagged: {:?}", mv.reason);
    }

    // P1-3: multi-generation archive attribution

    #[test]
    fn a_side_that_is_merely_behind_is_not_a_conflict() {
        // The archive has advanced to H2; source moved on to H3, target is still stuck at H1 (last sync didn't complete).
        // Previously both sides != the archive's current generation → false both-changed.
        let s = snap("windows", vec![file("f.txt", "H3")]);
        let t = snap("macos", vec![file("f.txt", "H1")]);
        let a = snap("windows", vec![arch("f.txt", "H2", &["H1", "H0"])]);
        let plan = compare(&s, &t, "sync", Some(&a), false, &CompareOptions::default());
        assert_eq!(plan.header.conflict_count, 0, "being behind is not concurrent editing: {:?}", actions(&plan));
        let up = plan.ops.iter().find(|o| o.action == Action::Update).expect("should propagate");
        assert_eq!(up.side, Side::Target);
        assert_eq!(up.hash.as_deref(), Some("H3"));
    }

    #[test]
    fn genuinely_novel_content_on_both_sides_is_still_a_conflict() {
        // Neither side's content was ever seen by the archive → this is the genuine concurrent edit; the multi-generation logic must never let it slip
        let s = snap("windows", vec![file("f.txt", "X")]);
        let t = snap("macos", vec![file("f.txt", "Y")]);
        let a = snap("windows", vec![arch("f.txt", "H2", &["H1", "H0"])]);
        let plan = compare(&s, &t, "sync", Some(&a), false, &CompareOptions::default());
        assert_eq!(plan.header.conflict_count, 1, "{:?}", actions(&plan));
    }

    #[test]
    fn newer_generation_wins_when_both_sides_are_behind() {
        // source sits at generation 1, target at generation 2 → source is newer, propagate to target
        let s = snap("windows", vec![file("f.txt", "H1")]);
        let t = snap("macos", vec![file("f.txt", "H0")]);
        let a = snap("windows", vec![arch("f.txt", "H2", &["H1", "H0"])]);
        let plan = compare(&s, &t, "sync", Some(&a), false, &CompareOptions::default());
        assert_eq!(plan.header.conflict_count, 0);
        let up = plan.ops.iter().find(|o| o.action == Action::Update).unwrap();
        assert_eq!(up.side, Side::Target);
        assert!(up.reason.contains("behind-by-generations"), "{}", up.reason);
    }

    #[test]
    fn roll_generations_builds_the_history_chain() {
        use crate::model::table::roll_generations;
        let old = vec![arch("f.txt", "H1", &["H0"])];
        let mut fresh = vec![file("f.txt", "H2")];
        roll_generations(&mut fresh, &old);
        assert_eq!(fresh[0].prev.as_ref().unwrap(), &vec!["H1".to_string(), "H0".to_string()]);

        // When the content hasn't changed, the same hash must not be poured into the history
        let mut same = vec![file("f.txt", "H1")];
        roll_generations(&mut same, &old);
        assert_eq!(same[0].prev.as_ref().unwrap(), &vec!["H0".to_string()]);
    }

    // P1-2: conflict copies

    #[test]
    fn conflict_policy_report_is_the_default_and_changes_nothing() {
        let s = snap_named("windows", "WIN", vec![file_at("f.txt", "X", 200)]);
        let t = snap_named("macos", "MAC", vec![file_at("f.txt", "Y", 100)]);
        let plan = compare(&s, &t, "sync", None, false, &CompareOptions::default());
        assert_eq!(plan.header.conflict_count, 1);
        assert!(!plan.ops.iter().any(|o| o.action == Action::Move), "report policy must not touch anything");
    }

    #[test]
    fn conflict_copy_keeps_the_loser_and_lands_the_winner() {
        let s = snap_named("windows", "WIN", vec![file_at("doc/report.pdf", "NEW", 5_000)]);
        let t = snap_named("macos", "MAC", vec![file_at("doc/report.pdf", "OLD", 1_000)]);
        let opts = CompareOptions { conflict: ConflictPolicy::Copy, ..Default::default() };
        let plan = compare(&s, &t, "sync", None, false, &opts);

        // The loser (target, older mtime) is renamed and archived first
        let mv = plan.ops.iter().find(|o| o.action == Action::Move).expect("loser must be kept");
        assert_eq!(mv.side, Side::Target);
        assert_eq!(mv.from.as_deref(), Some("doc/report.pdf"));
        assert!(mv.path.starts_with("doc/report.sync-conflict-"), "{}", mv.path);
        assert!(mv.path.ends_with(".pdf"), "extension must be preserved: {}", mv.path);
        // The winner's content lands on target
        let up = plan.ops.iter().find(|o| o.action == Action::Update && o.path == "doc/report.pdf").unwrap();
        assert_eq!(up.hash.as_deref(), Some("NEW"));
        // The original conflict row is downgraded to an auditable note and no longer counts as a conflict
        assert_eq!(plan.header.conflict_count, 0);
        assert!(plan.ops.iter().any(|o| o.action == Action::Note && o.reason.starts_with("auto-resolved")));
    }

    #[test]
    fn conflict_newer_overwrites_without_a_copy() {
        let s = snap_named("windows", "WIN", vec![file_at("f.txt", "NEW", 900)]);
        let t = snap_named("macos", "MAC", vec![file_at("f.txt", "OLD", 100)]);
        let opts = CompareOptions { conflict: ConflictPolicy::Newer, ..Default::default() };
        let plan = compare(&s, &t, "sync", None, false, &opts);
        assert!(!plan.ops.iter().any(|o| o.action == Action::Move), "newer policy keeps no copy");
        let up = plan.ops.iter().find(|o| o.action == Action::Update).unwrap();
        assert_eq!(up.side, Side::Target);
        assert_eq!(up.hash.as_deref(), Some("NEW"));
    }

    #[test]
    fn conflict_resolution_respects_the_older_side_winning() {
        // target is newer → target wins; both the copy and the overwrite happen on the source side
        let s = snap_named("windows", "WIN", vec![file_at("f.txt", "OLD", 100)]);
        let t = snap_named("macos", "MAC", vec![file_at("f.txt", "NEW", 900)]);
        let opts = CompareOptions { conflict: ConflictPolicy::Copy, ..Default::default() };
        let plan = compare(&s, &t, "sync", None, false, &opts);
        let mv = plan.ops.iter().find(|o| o.action == Action::Move).unwrap();
        assert_eq!(mv.side, Side::Source);
        let up = plan.ops.iter().find(|o| o.action == Action::Update && o.path == "f.txt").unwrap();
        assert_eq!(up.side, Side::Source);
        assert_eq!(up.hash.as_deref(), Some("NEW"));
    }

    #[test]
    fn delete_versus_change_conflicts_are_never_auto_resolved() {
        // "the other side deleted it but I changed it" — automatically arbitrating "delete or keep" is too dangerous; report only under every policy
        let s = snap_named("windows", "WIN", vec![file("f.txt", "CHANGED")]);
        let t = snap_named("macos", "MAC", Vec::new());
        let a = snap("windows", vec![file("f.txt", "ORIGINAL")]);
        let opts = CompareOptions { conflict: ConflictPolicy::Copy, ..Default::default() };
        let plan = compare(&s, &t, "sync", Some(&a), false, &opts);
        assert_eq!(plan.header.conflict_count, 1, "{:?}", actions(&plan));
        assert!(plan.ops.iter().any(|o| o.reason.contains("deleted-on-target-but-changed-on-source")));
    }

    #[test]
    fn conflict_names_are_well_formed() {
        let n = conflict_name("a/b/report.pdf", "WIN 01", 1_769_000_000_000);
        assert!(n.starts_with("a/b/report.sync-conflict-"), "{n}");
        assert!(n.ends_with("-WIN-01.pdf"), "host must be sanitized and extension kept: {n}");
        assert!(is_conflict_copy(&n));
        // A hidden file has no extension to speak of
        let h = conflict_name(".gitignore", "H", 0);
        assert!(h.starts_with(".gitignore.sync-conflict-"), "{h}");
        assert!(!is_conflict_copy("a/b/normal.pdf"));
    }

    #[test]
    fn conflict_copies_over_the_limit_are_pruned() {
        let mut entries = vec![file("f.txt", "SAME")];
        for i in 1..=4 {
            entries.push(file(&format!("f.sync-conflict-2026070{i}-120000-MAC.txt"), &format!("c{i}")));
        }
        let s = snap_named("windows", "WIN", vec![file("f.txt", "SAME")]);
        let t = snap_named("macos", "MAC", entries);
        let opts = CompareOptions { conflict: ConflictPolicy::Copy, max_conflicts: 2, ..Default::default() };
        let plan = compare(&s, &t, "sync", None, false, &opts);
        let pruned: Vec<&str> = plan
            .ops
            .iter()
            .filter(|o| o.reason.contains("conflict-copy-over-limit"))
            .map(|o| o.path.as_str())
            .collect();
        assert_eq!(pruned.len(), 2, "4 copies, limit 2 -> drop the 2 oldest: {pruned:?}");
        assert!(pruned.iter().all(|p| p.contains("20260701") || p.contains("20260702")), "{pruned:?}");
    }

    // P2-4: unix permission bits

    #[test]
    fn mode_only_difference_produces_a_chmod_not_a_recopy() {
        let mut se = file("run.sh", "SAME");
        se.mode = Some(0o755);
        let mut te = file("run.sh", "SAME");
        te.mode = Some(0o644);
        let s = snap("macos", vec![se]);
        let t = snap("linux", vec![te]);
        let opts = CompareOptions { sync_mode: true, ..Default::default() };
        let plan = compare(&s, &t, "mirror", None, false, &opts);
        assert_eq!(actions(&plan), vec![("chmod", "run.sh")], "content is identical; only the bits differ");
        assert_eq!(plan.ops[0].mode, Some(0o755));
    }

    #[test]
    fn mode_is_ignored_unless_enabled_and_both_sides_are_unix() {
        let mut se = file("run.sh", "SAME");
        se.mode = Some(0o755);
        let mut te = file("run.sh", "SAME");
        te.mode = Some(0o644);
        // Off by default
        let plan = compare(&snap("macos", vec![se.clone()]), &snap("linux", vec![te.clone()]), "mirror", None, false, &CompareOptions::default());
        assert!(plan.ops.is_empty());
        // The Windows side has no mode, so even switched on it must not report a difference
        let opts = CompareOptions { sync_mode: true, ..Default::default() };
        let plan2 = compare(&snap("macos", vec![se]), &snap("windows", vec![te]), "mirror", None, false, &opts);
        assert!(plan2.ops.is_empty(), "{:?}", actions(&plan2));
    }

    #[test]
    fn copies_carry_the_source_mode_when_enabled() {
        let mut se = file("new.sh", "H");
        se.mode = Some(0o755);
        let s = snap("macos", vec![se]);
        let t = snap("linux", Vec::new());
        let opts = CompareOptions { sync_mode: true, ..Default::default() };
        let plan = compare(&s, &t, "mirror", None, false, &opts);
        assert_eq!(plan.ops.len(), 1);
        assert_eq!(plan.ops[0].mode, Some(0o755), "a fresh copy must land with the right bits in one step");
    }

    // P2-3: case collisions

    #[test]
    fn case_sensitive_mode_flags_a_write_that_would_clobber_a_case_twin() {
        // With case_sensitive = true, Foo.txt and foo.txt are two files,
        // but on NTFS/APFS writing the former silently overwrites the latter.
        let s = snap("windows", vec![file("Foo.txt", "A"), file("foo.txt", "B")]);
        let t = snap("windows", vec![file("foo.txt", "B")]);
        let opts = CompareOptions { case_insensitive: false, ..Default::default() };
        let plan = compare(&s, &t, "mirror", None, false, &opts);
        let c = plan.ops.iter().find(|o| o.path == "Foo.txt").expect("Foo.txt must be planned somehow");
        assert_eq!(c.action, Action::Conflict, "{:?}", c.reason);
        assert!(c.reason.contains("case-collision"), "{}", c.reason);
    }

    /// The gap that shipped: a VFS root records `header.os` as the *protocol*, so the
    /// `os == "windows"` gate skipped every one of them. The recorded `name_rules` is what
    /// decides now — and a root can still carry `windows` while `os` says `smb`, because that
    /// is exactly what the tables written by the old OS-delegating SMB backend look like, and
    /// those tables are still on disk being compared against.
    #[test]
    fn windows_name_check_fires_on_an_smb_root_not_just_a_local_windows_one() {
        let bad = vec![
            file("report:2024.pdf", "h1"),
            file("trail.", "h2"),
            file("notes/CON", "h3"),
            file("a?b.txt", "h4"),
        ];
        let s = snap("macos", bad.clone());
        let t = snap_vfs("smb", "windows", vec![]);
        let plan = compare(&s, &t, "mirror", None, false, &CompareOptions::default());
        let refused: Vec<_> = plan.ops.iter().filter(|o| o.action == Action::Conflict).collect();
        assert_eq!(refused.len(), 4, "every one must be refused, got {:?}", actions(&plan));
        assert!(plan.ops.iter().all(|o| o.action != Action::Copy), "nothing may be copied");

        // The reason must classify the failure honestly — the three modes are not the same risk
        let why = |p: &str| refused.iter().find(|o| o.path == p).unwrap().reason.clone();
        assert!(why("report:2024.pdf").contains("alternate data stream"), "{}", why("report:2024.pdf"));
        assert!(why("trail.").contains("truncated to 'trail'"), "{}", why("trail."));
        assert!(why("notes/CON").contains("reserved device name"), "{}", why("notes/CON"));
        assert!(why("a?b.txt").contains("refuses the character"), "{}", why("a?b.txt"));
    }

    /// Deleting a mangled name is the worst case of all, because it *succeeds* against the
    /// wrong file. Measured: applying a delete of rel `trail.` removed `trail`, returned Ok,
    /// and left `trail.` standing — so the next round finds it again, forever, having eaten an
    /// innocent neighbour on the way. A delete must therefore be refused too, which the
    /// Copy/Move-only gate did not do.
    #[test]
    fn a_mangled_name_is_refused_for_deletes_as_well_as_creates() {
        // mirror: target has files the source does not → deletions on the Windows target
        let s = snap("macos", vec![]);
        let t = snap("windows", vec![file("trail.", "h1"), file("keep:me.txt", "h2"), file("CON", "h3")]);
        let plan = compare(&s, &t, "mirror", None, false, &CompareOptions::default());

        let by_path = |p: &str| plan.ops.iter().find(|o| o.path == p).map(|o| (o.action.clone(), o.reason.clone()));
        let (a, why) = by_path("trail.").expect("trail. must appear");
        assert_eq!(a, Action::Conflict, "a delete that would hit a different file must be refused: {why}");
        assert!(why.contains("truncated to 'trail'"), "{why}");
        let (a, _) = by_path("keep:me.txt").expect("the colon case must appear");
        assert_eq!(a, Action::Conflict, "a colon path does not address the file it names");

        // A reserved device name is addressable — std deletes it cleanly, so the delete stands.
        let (a, _) = by_path("CON").expect("CON must appear");
        assert_eq!(a, Action::Delete, "refusing to delete a reserved name would strand it forever");
    }

    /// The source root is the one being *read*. A mangled path there reads a different file,
    /// so the copy would land the wrong bytes under the right name on a perfectly healthy target.
    #[test]
    fn a_mangled_name_on_the_reading_side_is_refused_too() {
        let s = snap("windows", vec![file("trail.", "h1")]);
        let t = snap("linux", vec![]);
        let plan = compare(&s, &t, "mirror", None, false, &CompareOptions::default());
        let op = plan.ops.iter().find(|o| o.path == "trail.").expect("the op must exist");
        assert_eq!(op.action, Action::Conflict, "reason was: {}", op.reason);
        assert!(op.reason.contains("reading side"), "the message must say which root is at fault: {}", op.reason);
    }

    /// SFTP/FTP cannot tell us the server's OS. Refusing a name that is perfectly legal on
    /// Linux would be wrong; saying nothing would be worse. The op proceeds, with a Note.
    #[test]
    fn unknown_server_rules_warn_instead_of_refusing() {
        let s = snap("macos", vec![file("report:2024.pdf", "h1")]);
        let t = snap_vfs("sftp", "unknown", vec![]);
        let plan = compare(&s, &t, "mirror", None, false, &CompareOptions::default());
        assert!(plan.ops.iter().any(|o| o.action == Action::Copy && o.path == "report:2024.pdf"));
        let note = plan.ops.iter().find(|o| o.action == Action::Note).expect("a warning must exist");
        assert!(note.reason.contains("name-risk-on-unknown-server"), "{}", note.reason);
    }

    /// A posix target must not inherit any of this: colons and reserved names are ordinary
    /// file names there, and the plan says so by staying silent.
    #[test]
    fn posix_targets_keep_names_windows_would_reject() {
        let s = snap("macos", vec![file("report:2024.pdf", "h1"), file("CON", "h2")]);
        let t = snap("linux", vec![]);
        let plan = compare(&s, &t, "mirror", None, false, &CompareOptions::default());
        assert_eq!(plan.ops.iter().filter(|o| o.action == Action::Copy).count(), 2);
        assert!(plan.ops.iter().all(|o| o.action != Action::Note && o.action != Action::Conflict));
    }

    #[test]
    fn nfc_nfd_paths_match() {
        // "café" NFC (U+00E9) vs NFD (e + U+0301): the same file, must produce no op at all
        let nfc = "caf\u{00e9}.txt";
        let nfd = "cafe\u{0301}.txt";
        let s = snap("windows", vec![file(nfc, "h1")]);
        let t = snap("macos", vec![file(nfd, "h1")]);
        let plan = compare(&s, &t, "mirror", None, false, &CompareOptions::default());
        assert_eq!(plan.ops.len(), 0, "NFC/NFD spellings of the same name must match");
    }

    #[test]
    fn case_insensitive_match_and_opt_out() {
        let s = snap("windows", vec![file("Readme.md", "h1")]);
        let t = snap("macos", vec![file("readme.md", "h1")]);
        assert_eq!(compare(&s, &t, "mirror", None, false, &CompareOptions { case_insensitive: true, ..Default::default() }).ops.len(), 0);
        // Case-sensitive: case twins with the same hash get paired by move detection into a single rename — smarter than copy + delete
        let plan = compare(&s, &t, "mirror", None, false, &CompareOptions { case_insensitive: false, ..Default::default() });
        assert_eq!(plan.ops.len(), 1);
        assert_eq!(plan.ops[0].action, Action::Move);
    }

    #[test]
    fn update_keeps_target_spelling() {
        let s = snap("windows", vec![file("CAF\u{00c9}.TXT", "new")]);
        let t = snap("macos", vec![file("cafe\u{0301}.txt", "old")]);
        let plan = compare(&s, &t, "mirror", None, false, &CompareOptions::default());
        assert_eq!(plan.ops.len(), 1);
        assert_eq!(plan.ops[0].action, Action::Update);
        assert_eq!(plan.ops[0].path, "cafe\u{0301}.txt", "update must use target's own spelling");
    }

    #[test]
    fn illegal_windows_names_become_conflicts() {
        let s = snap("macos", vec![file("aux.log", "h1"), file("ok.txt", "h2"), file("bad. /x", "h3")]);
        let t = snap("windows", Vec::new());
        let plan = compare(&s, &t, "mirror", None, false, &CompareOptions::default());
        let conflicts: Vec<_> = plan.ops.iter().filter(|o| o.action == Action::Conflict).collect();
        assert_eq!(conflicts.len(), 2, "aux.log and 'bad. ' segment must be flagged");
        assert!(plan.ops.iter().any(|o| o.action == Action::Copy && o.path == "ok.txt"));
    }

    // sync-with-archive classification matrix
    // State notation: E = present with content x, ∅ = absent. archive = the consensus state at the last sync.

    fn plan_sync(s: Vec<Entry>, t: Vec<Entry>, a: Option<Vec<Entry>>) -> Plan {
        let s = snap("windows", s);
        let t = snap("macos", t);
        let a = a.map(|e| snap("windows", e));
        compare(&s, &t, "sync", a.as_ref(), false, &CompareOptions::default())
    }
    fn one(plan: &Plan) -> &Op {
        assert_eq!(plan.ops.len(), 1, "expected exactly 1 op, got: {:?}", plan.ops);
        &plan.ops[0]
    }

    #[test]
    fn matrix_equal_no_op() {
        let p = plan_sync(vec![file("a", "h")], vec![file("a", "h")], Some(vec![file("a", "h")]));
        assert_eq!(p.ops.len(), 0);
    }

    #[test]
    fn matrix_source_changed_propagates_to_target() {
        let p = plan_sync(vec![file("a", "h2")], vec![file("a", "h1")], Some(vec![file("a", "h1")]));
        let op = one(&p);
        assert_eq!((op.side.clone(), op.action.clone()), (Side::Target, Action::Update));
    }

    #[test]
    fn matrix_target_changed_propagates_to_source() {
        let p = plan_sync(vec![file("a", "h1")], vec![file("a", "h2")], Some(vec![file("a", "h1")]));
        let op = one(&p);
        assert_eq!((op.side.clone(), op.action.clone()), (Side::Source, Action::Update));
    }

    #[test]
    fn matrix_both_changed_conflict() {
        let p = plan_sync(vec![file("a", "h2")], vec![file("a", "h3")], Some(vec![file("a", "h1")]));
        assert_eq!(one(&p).action, Action::Conflict);
        assert_eq!(p.header.conflict_count, 1);
    }

    #[test]
    fn matrix_target_deleted_propagates_deletion() {
        // The archive has it, target doesn't, source is unchanged → delete on source
        let p = plan_sync(vec![file("a", "h1")], vec![], Some(vec![file("a", "h1")]));
        let op = one(&p);
        assert_eq!((op.side.clone(), op.action.clone()), (Side::Source, Action::Delete));
    }

    #[test]
    fn matrix_delete_vs_edit_conflict() {
        // target deleted it but source changed it → delete-vs-edit conflict; never delete silently
        let p = plan_sync(vec![file("a", "h2")], vec![], Some(vec![file("a", "h1")]));
        assert_eq!(one(&p).action, Action::Conflict);
    }

    #[test]
    fn matrix_new_on_source_copies() {
        let p = plan_sync(vec![file("a", "h1")], vec![], Some(vec![]));
        let op = one(&p);
        assert_eq!((op.side.clone(), op.action.clone()), (Side::Target, Action::Copy));
    }

    #[test]
    fn matrix_move_on_source_replayed_on_target() {
        // source moved a to b; target/archive still have a → replay the move on target
        let p = plan_sync(vec![file("b", "h1")], vec![file("a", "h1")], Some(vec![file("a", "h1")]));
        let op = one(&p);
        assert_eq!(op.action, Action::Move);
        assert_eq!(op.side, Side::Target);
        assert_eq!(op.from.as_deref(), Some("a"));
        assert_eq!(op.path, "b");
    }

    #[test]
    fn matrix_no_archive_differ_is_conflict_and_adds_flow_both_ways() {
        let p = plan_sync(vec![file("a", "h1"), file("s", "hs")], vec![file("a", "h2"), file("t", "ht")], None);
        assert!(p.ops.iter().any(|o| o.action == Action::Conflict && o.path == "a"));
        assert!(p.ops.iter().any(|o| o.action == Action::Copy && o.side == Side::Target && o.path == "s"));
        assert!(p.ops.iter().any(|o| o.action == Action::Copy && o.side == Side::Source && o.path == "t"));
        assert!(!p.ops.iter().any(|o| o.action == Action::Delete), "no-archive sync must never delete");
    }

    #[test]
    fn matrix_enrich_never_deletes_or_downgrades() {
        let s = snap("windows", vec![file("only-src", "h1")]);
        let mut old = file("shared", "h-old");
        old.mtime_ms = 0;
        let mut newer_on_target = file("shared", "h-new");
        newer_on_target.mtime_ms = 999_999;
        let t = snap("macos", vec![newer_on_target, file("only-tgt", "hx")]);
        let s = Snapshot { header: s.header, entries: vec![s.entries[0].clone(), old] };
        let p = compare(&s, &t, "enrich", None, false, &CompareOptions::default());
        assert!(p.ops.iter().any(|o| o.action == Action::Copy && o.path == "only-src"));
        assert!(!p.ops.iter().any(|o| o.action == Action::Delete));
        assert!(!p.ops.iter().any(|o| o.action == Action::Update), "enrich must not downgrade newer target");
    }
}
