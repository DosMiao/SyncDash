//! The three gates before anything is touched (P0-2 / P0-3).
//!
//! 1. **Mount-point marker** — semantics modelled on syncthing's `.stfolder`
//!    (CheckPath in `lib/config/folderconfiguration.go:236`). When an SMB share isn't mounted,
//!    target is usually an empty directory (it may even be auto-created locally), and mirror
//!    then plans "delete everything in target" or "re-send tens of GB". The marker file is the
//!    only reliable test: it travels with the **data**, so no mount means no marker.
//! 2. **Disk-space preflight** — every op in the plan carries a size, so summing them up front tells us how much gets written.
//!    Modelled on syncthing's `CheckAvailableSpace` / `minDiskFree` (1% by default).
//! 3. **Plan health check** — refuse to run when the deletion share is too high. syncthing has no
//!    equivalent (it syncs continuously; there is no "one big plan"), but our explicit model suits
//!    this gate well: it also catches a wrong filter, swapped source/target, and typo'd paths.

use crate::model::plan::{Action, Op, Side};
use crate::foundation::fmt::human_bytes;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::foundation::names::MARKER_NAME;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Marker {
    pub job: String,
    pub host: String,
    pub created_at_ms: u64,
    /// Free-form note, human-readable
    #[serde(default)]
    pub note: String,
}

pub fn marker_path(root: &Path) -> PathBuf {
    root.join(MARKER_NAME)
}

pub fn has_marker(root: &Path) -> bool {
    marker_path(root).is_file()
}

pub fn read_marker(root: &Path) -> Option<Marker> {
    let text = std::fs::read_to_string(marker_path(root)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Write the marker (`syncdash mark`). If one already exists, keep its content and report it — never overwrite.
pub fn write_marker(root: &Path, job: &str, note: &str) -> std::io::Result<(PathBuf, bool)> {
    let p = marker_path(root);
    if p.is_file() {
        return Ok((p, false));
    }
    if !root.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("not a directory: {}", root.display()),
        ));
    }
    let m = Marker {
        job: job.to_string(),
        host: crate::model::table::host_name(),
        created_at_ms: crate::foundation::time::now_ms(),
        note: note.to_string(),
    };
    std::fs::write(&p, format!("{}\n", serde_json::to_string_pretty(&m)?))?;
    Ok((p, true))
}

/// Returns (bytes available to the current user, total bytes on the volume). None if unavailable (not a blocker, just no check).
#[cfg(windows)]
pub fn disk_space(path: &Path) -> Option<(u64, u64)> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    extern "system" {
        fn GetDiskFreeSpaceExW(
            lp_directory_name: *const u16,
            lp_free_bytes_available_to_caller: *mut u64,
            lp_total_number_of_bytes: *mut u64,
            lp_total_number_of_free_bytes: *mut u64,
        ) -> i32;
    }
    // UNC paths (\\host\share\...) are supported too — exactly what an SMB target needs
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let (mut avail, mut total, mut free) = (0u64, 0u64, 0u64);
    let ok = unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut avail, &mut total, &mut free) };
    if ok == 0 {
        None
    } else {
        Some((avail, total))
    }
}

#[cfg(unix)]
pub fn disk_space(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    unsafe {
        let mut st: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(c.as_ptr(), &mut st) != 0 {
            return None;
        }
        // f_frsize is the "fragment size", the right unit for capacity math; fall back to f_bsize when it is 0
        let unit = if st.f_frsize > 0 { st.f_frsize as u64 } else { st.f_bsize as u64 };
        Some((st.f_bavail as u64 * unit, st.f_blocks as u64 * unit))
    }
}

#[derive(Default, Clone, Debug)]
pub struct SideStats {
    /// Bytes that have to be written (copy + update)
    pub write_bytes: u64,
    pub copies: u64,
    pub updates: u64,
    pub deletes: u64,
    pub delete_dirs: u64,
    pub moves: u64,
}

#[derive(Default, Clone, Debug)]
pub struct PlanStats {
    pub source: SideStats,
    pub target: SideStats,
    pub conflicts: u64,
}

pub fn stat_plan(ops: &[Op]) -> PlanStats {
    let mut st = PlanStats::default();
    for op in ops {
        if op.action == Action::Conflict {
            st.conflicts += 1;
            continue;
        }
        let s = match op.side {
            Side::Source => &mut st.source,
            Side::Target => &mut st.target,
        };
        match op.action {
            Action::Copy => {
                s.copies += 1;
                s.write_bytes += op.size.unwrap_or(0);
            }
            Action::Update => {
                s.updates += 1;
                s.write_bytes += op.size.unwrap_or(0);
            }
            Action::Move => s.moves += 1,
            Action::Delete => s.deletes += 1,
            Action::DeleteDir => s.delete_dirs += 1,
            _ => {}
        }
    }
    st
}

#[derive(Clone, Debug)]
pub struct Guards {
    /// Require a .syncdash-root marker on both roots (guards against an unmounted share)
    pub require_marker: bool,
    /// Minimum free ratio to keep (0.01 = 1%). <=0 disables
    pub min_free_pct: f64,
    /// Refuse to run when one side's deleted entries exceed this share of that side's total. <=0 or >=1 disables
    pub max_delete_ratio: f64,
    /// User allowed it through explicitly (--i-know); only lets the health-check gates pass, marker/space still block
    pub acknowledged: bool,
}

impl Default for Guards {
    fn default() -> Self {
        Guards { require_marker: false, min_free_pct: 0.01, max_delete_ratio: 0.5, acknowledged: false }
    }
}

/// The verdict of one preflight. Non-empty `blockers` = refuse to run.
pub struct Verdict {
    pub blockers: Vec<String>,
    pub warnings: Vec<String>,
}

impl Verdict {
    pub fn ok(&self) -> bool {
        self.blockers.is_empty()
    }
    /// Print the verdict to stderr; returns whether it is allowed through
    pub fn report(&self, tag: &str) -> bool {
        for w in &self.warnings {
            crate::log_warn!("preflight", "[{tag}] warning: {w}");
        }
        for b in &self.blockers {
            crate::log_error!("preflight", "[{tag}] REFUSED: {b}");
        }
        self.ok()
    }
}

/// Root availability + marker check. `label` is only used in messages ("source"/"target").
pub fn check_root(label: &str, root: &Path, require_marker: bool, v: &mut Verdict) {
    if !root.is_dir() {
        v.blockers.push(format!("{label} root not accessible: {}", root.display()));
        return;
    }
    if require_marker && !has_marker(root) {
        v.blockers.push(format!(
            "{label} root has no {MARKER_NAME} marker: {} \
             — the share may not be mounted. Run `syncdash mark <root>` once on the real data, \
             or set require_marker = false in the job.",
            root.display()
        ));
        return;
    }
    // Even without a required marker, an empty directory plus planned deletions is worth a warning (decided in check_plan)
    if !require_marker && !has_marker(root) {
        let empty = std::fs::read_dir(root).map(|mut d| d.next().is_none()).unwrap_or(false);
        if empty {
            v.warnings.push(format!(
                "{label} root is empty and unmarked: {} — if this share simply isn't mounted, \
                 stop now (enable require_marker to make this an error)",
                root.display()
            ));
        }
    }
}

/// The same three-level judgment for a VFS root (syncthing's folder-marker defense):
/// unreachable / not-a-directory → blocker; marker demanded but absent → blocker;
/// empty and unmarked → warning. Distinguishing "the mount is not up" from "the user
/// deleted everything" is the whole game on a network root.
pub fn check_root_vfs(label: &str, vfs: &std::sync::Arc<dyn crate::fs::vfs::Vfs>, require_marker: bool, v: &mut Verdict) {
    use crate::model::table::EntryKind;
    let disp = vfs.display();
    match vfs.stat("") {
        Ok(Some(m)) if m.kind == EntryKind::Dir => {}
        Ok(Some(_)) => {
            v.blockers.push(format!("{label} root is not a directory: {disp}"));
            return;
        }
        Ok(None) => {
            v.blockers.push(format!("{label} root does not exist: {disp}"));
            return;
        }
        Err(e) => {
            v.blockers.push(format!("{label} root not accessible: {disp} ({e})"));
            return;
        }
    }
    let marker = matches!(vfs.stat(MARKER_NAME), Ok(Some(_)));
    if require_marker && !marker {
        v.blockers.push(format!(
            "{label} root has no {MARKER_NAME} marker: {disp} \
             — the share may not be mounted. Run `syncdash mark <root>` once on the real data, \
             or set require_marker = false in the job."
        ));
        return;
    }
    if !require_marker && !marker {
        let empty = vfs.read_dir_names("").map(|l| l.is_empty()).unwrap_or(false);
        if empty {
            v.warnings.push(format!(
                "{label} root is empty and unmarked: {disp} — if this root simply isn't reachable, \
                 stop now (enable require_marker to make this an error)"
            ));
        }
    }
}

// ---- the capability report: the no-silent rule, mechanized ----

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapSeverity {
    /// Running would make the table or the plan lie — refuse outright.
    Block,
    /// Runnable, but only with the user's explicit consent (`--accept-caps` / a ticked box).
    NeedsAck,
    /// Stated for the record; nothing to decide.
    Info,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CapItem {
    /// The job feature concerned ("evidence=sampled", "mtime window", …)
    pub feature: String,
    /// "source" | "target" | "both"
    pub side: String,
    pub severity: CapSeverity,
    /// What the job asked for
    pub requested: String,
    /// What the backend can give
    pub actual: String,
    /// What this run will therefore do — in plain words, shown verbatim
    pub effect: String,
}

impl CapItem {
    pub fn render(&self) -> String {
        format!("[{}] {}: wanted {}, backend has {} — {}", self.side, self.feature, self.requested, self.actual, self.effect)
    }
}

#[derive(Clone, Debug, Default)]
pub struct CapReport {
    pub items: Vec<CapItem>,
}

impl CapReport {
    pub fn blockers(&self) -> Vec<&CapItem> {
        self.items.iter().filter(|i| i.severity == CapSeverity::Block).collect()
    }
    pub fn needs_ack(&self) -> Vec<&CapItem> {
        self.items.iter().filter(|i| i.severity == CapSeverity::NeedsAck).collect()
    }
    pub fn infos(&self) -> Vec<&CapItem> {
        self.items.iter().filter(|i| i.severity == CapSeverity::Info).collect()
    }
}

/// What the read side of a run asks of its backends — primitives only (this layer
/// must not know the Job schema; `Job::read_caps_query` builds one).
#[derive(Clone, Copy, Debug)]
pub struct ReadCapsQuery {
    pub hash: bool,
    pub sampled: bool,
    pub escalate: bool,
    pub symlinks_direct: bool,
    pub min_free_pct: f64,
    /// The effective no-hash mtime window (already widened to the coarser backend)
    pub window_ms: i64,
    pub src_local: bool,
    pub tgt_local: bool,
}

/// The read-side capability comparison. Write-side items (fsync, verify, trash,
/// delta, chmod) join when the VFS apply lane lands.
pub fn cap_report_read(
    q: &ReadCapsQuery,
    src: &crate::fs::vfs::VfsCaps,
    tgt: &crate::fs::vfs::VfsCaps,
) -> CapReport {
    use crate::fs::vfs::Support;
    let mut r = CapReport::default();

    // symlinks="direct" needs the backend to *see* links, or the table paints a false picture
    for (side, c) in [("source", src), ("target", tgt)] {
        if q.symlinks_direct && c.symlink == Support::No {
            r.items.push(CapItem {
                feature: "symlinks=direct".into(),
                side: side.into(),
                severity: CapSeverity::Block,
                requested: "symlinks recorded in the table".into(),
                actual: "backend cannot represent symlinks".into(),
                effect: "the table would silently omit every link — refusing to produce a false picture".into(),
            });
        }
    }

    // sampled evidence needs ranged reads; without them the scan upgrades to full reads, out loud
    if q.hash && q.sampled {
        for (side, c) in [("source", src), ("target", tgt)] {
            if c.ranged_read == Support::No {
                r.items.push(CapItem {
                    feature: "evidence=sampled".into(),
                    side: side.into(),
                    severity: CapSeverity::NeedsAck,
                    requested: "3-window sampled digests".into(),
                    actual: "no ranged reads".into(),
                    effect: "sampled digests only match other sampled digests, so BOTH sides upgrade to full reads — every changed file is read whole over its link; set evidence=none to skip content reads instead".into(),
                });
            }
        }
    }

    // A widened no-hash window is a blind spot exactly when there is no content evidence
    if q.window_ms > crate::model::plan::MTIME_SLACK_MS {
        let secs = q.window_ms / 1000;
        r.items.push(CapItem {
            feature: "mtime window".into(),
            side: "both".into(),
            severity: if q.hash { CapSeverity::Info } else { CapSeverity::NeedsAck },
            requested: format!("±{}s equality window", crate::model::plan::MTIME_SLACK_MS / 1000),
            actual: format!("backend timestamps are only ±{secs}s precise"),
            effect: if q.hash {
                format!("timestamp equality widens to ±{secs}s; content evidence still decides")
            } else {
                format!("with evidence=none, edits within {secs}s of each other are INVISIBLE to this comparison")
            },
        });
    }

    if q.escalate && q.sampled && (!q.src_local || !q.tgt_local) {
        r.items.push(CapItem {
            feature: "escalate".into(),
            side: "both".into(),
            severity: CapSeverity::Info,
            requested: "full re-read on sampled-digest/mtime disagreement".into(),
            actual: "a root lives on a remote backend".into(),
            effect: "escalation is skipped this run (it arrives with the VFS write lane)".into(),
        });
    }

    if q.min_free_pct > 0.0 {
        for (side, c) in [("source", src), ("target", tgt)] {
            if c.free_space == Support::No {
                r.items.push(CapItem {
                    feature: "min_free_pct".into(),
                    side: side.into(),
                    severity: CapSeverity::Info,
                    requested: "free-space gate before writing".into(),
                    actual: "backend cannot report free space".into(),
                    effect: "the space gate is inert on this side — writes proceed without it".into(),
                });
            }
        }
    }

    r
}

/// What the write side of a run asks of its backends (`Job::write_caps_query` builds one).
#[derive(Clone, Copy, Debug)]
pub struct WriteCapsQuery {
    pub fsync: bool,
    pub verify: bool,
    pub versioning: bool,
    pub delta: bool,
    pub src_local: bool,
    pub tgt_local: bool,
}

/// The write-side capability comparison, evaluated against the ops that will actually
/// run. Joins the read-side report before the apply stage starts.
pub fn cap_report_write(
    q: &WriteCapsQuery,
    ops: &[Op],
    src: &crate::fs::vfs::VfsCaps,
    tgt: &crate::fs::vfs::VfsCaps,
) -> CapReport {
    use crate::fs::vfs::Support;
    let mut r = CapReport::default();

    for (side, side_tag, caps, local) in [
        (Side::Source, "source", src, q.src_local),
        (Side::Target, "target", tgt, q.tgt_local),
    ] {
        let side_ops: Vec<&Op> = ops
            .iter()
            .filter(|o| o.side == side && !matches!(o.action, Action::Conflict | Action::Note))
            .collect();
        if side_ops.is_empty() {
            continue;
        }

        // A plan the backend cannot execute in full is a plan the table would lie about
        if caps.unix_mode == Support::No && side_ops.iter().any(|o| o.action == Action::Chmod) {
            r.items.push(CapItem {
                feature: "chmod ops".into(),
                side: side_tag.into(),
                severity: CapSeverity::Block,
                requested: format!("{} permission change(s) from the plan", side_ops.iter().filter(|o| o.action == Action::Chmod).count()),
                actual: "backend has no unix modes".into(),
                effect: "the plan cannot be executed in full — refusing rather than silently dropping ops".into(),
            });
        }
        if caps.symlink == Support::No && side_ops.iter().any(|o| o.link.is_some()) {
            r.items.push(CapItem {
                feature: "symlink ops".into(),
                side: side_tag.into(),
                severity: CapSeverity::Block,
                requested: "symlink creation from the plan".into(),
                actual: "backend cannot create symlinks".into(),
                effect: "the plan cannot be executed in full — refusing rather than silently dropping ops".into(),
            });
        }

        // The root-lock heartbeat rides set_mtime; a backend that cannot set mtimes
        // cannot prove it is alive to the other machine — writing there is refused
        if caps.set_mtime == Support::No && !local {
            r.items.push(CapItem {
                feature: "root lock".into(),
                side: side_tag.into(),
                severity: CapSeverity::Block,
                requested: "a heartbeat other machines can observe".into(),
                actual: "backend cannot set mtimes (FTP without MFMT)".into(),
                effect: "the lock protocol cannot signal liveness — refusing to write; comparing (read-only) still works".into(),
            });
        }

        if q.fsync {
            match caps.fsync {
                Support::No => r.items.push(CapItem {
                    feature: "fsync=true".into(),
                    side: side_tag.into(),
                    severity: CapSeverity::NeedsAck,
                    requested: "fsync before rename".into(),
                    actual: "backend has no fsync".into(),
                    effect: "renamed files may not be durable across a crash on this side — continuing means accepting the server's own caching".into(),
                }),
                Support::Unknown => r.items.push(CapItem {
                    feature: "fsync=true".into(),
                    side: side_tag.into(),
                    severity: CapSeverity::NeedsAck,
                    requested: "fsync before rename".into(),
                    actual: "support unknown until tried".into(),
                    effect: "fsync is attempted per file; where the server refuses it, that file counts as failed (set fsync=false to skip the attempt)".into(),
                }),
                Support::Yes => {}
            }
        }

        if q.verify && caps.read_back == Support::No {
            r.items.push(CapItem {
                feature: "verify_writes".into(),
                side: side_tag.into(),
                severity: CapSeverity::NeedsAck,
                requested: "re-read the staged file before rename".into(),
                actual: "backend cannot read the staged file back".into(),
                effect: "verification degrades to the copy-stream hash plus a length reconciliation — no on-disk read-back".into(),
            });
        }

        let destructive = side_ops
            .iter()
            .any(|o| matches!(o.action, Action::Delete | Action::Update | Action::Copy));
        if destructive && !local {
            if q.versioning {
                r.items.push(CapItem {
                    feature: "versioning".into(),
                    side: side_tag.into(),
                    severity: CapSeverity::NeedsAck,
                    requested: "rdelta version store on this root".into(),
                    actual: "remote backend (no local version machinery)".into(),
                    effect: "overwritten/deleted files are kept as whole files under <root>/.syncdash/trash/<run>/ instead — recover with any file browser; rdelta history does not accrue on this side".into(),
                });
            } else if !caps.local_trash {
                r.items.push(CapItem {
                    feature: "trash".into(),
                    side: side_tag.into(),
                    severity: CapSeverity::NeedsAck,
                    requested: "deleted/overwritten files into the local trash".into(),
                    actual: "the OS trash cannot reach this root".into(),
                    effect: "originals move into <root>/.syncdash/trash/<run>/ ON THE REMOTE side (a rename, nothing downloaded) — not this machine's recycle bin".into(),
                });
            }
        }
    }

    if q.delta && !(q.src_local && q.tgt_local) {
        r.items.push(CapItem {
            feature: "delta".into(),
            side: "both".into(),
            severity: CapSeverity::NeedsAck,
            requested: "chunk-wise delta updates".into(),
            actual: "a root lives on a remote backend".into(),
            effect: "delta is disabled this run — updates rewrite files in full over the link".into(),
        });
    }

    r
}

/// Space check: the writing side needs write_bytes, and must still have min_free_pct left afterwards.
pub fn check_space(label: &str, root: &Path, need: u64, min_free_pct: f64, v: &mut Verdict) {
    if need == 0 {
        return;
    }
    let Some((avail, total)) = disk_space(root) else {
        v.warnings.push(format!("{label}: cannot determine free space on {}", root.display()));
        return;
    };
    // 10% margin: the target may have cluster alignment / sparseness / metadata overhead, and the sizes in the plan are the source side's
    let need_padded = need.saturating_add(need / 10);
    let reserve = if min_free_pct > 0.0 { (total as f64 * min_free_pct) as u64 } else { 0 };
    if avail < need_padded.saturating_add(reserve) {
        v.blockers.push(format!(
            "{label}: insufficient space on {} — need {} (+10% margin) and want {} free afterwards, but only {} available",
            root.display(),
            human_bytes(need),
            human_bytes(reserve),
            human_bytes(avail),
        ));
    }
}

/// Plan health check: deletion share. `entries` is that side's snapshot entry count (0 = nothing to judge against, skip).
pub fn check_delete_ratio(
    label: &str,
    side: &SideStats,
    entries: u64,
    g: &Guards,
    v: &mut Verdict,
) {
    let removals = side.deletes;
    if removals == 0 || entries == 0 {
        return;
    }
    if !(g.max_delete_ratio > 0.0 && g.max_delete_ratio < 1.0) {
        return;
    }
    let ratio = removals as f64 / entries as f64;
    if ratio < g.max_delete_ratio {
        return;
    }
    let msg = format!(
        "{label}: plan deletes {removals} of {entries} entries ({:.0}%) — over the {:.0}% guard. \
         A wrong filter, an unmounted share, or swapped source/target all look exactly like this.",
        ratio * 100.0,
        g.max_delete_ratio * 100.0
    );
    if g.acknowledged {
        v.warnings.push(format!("{msg} (allowed by --i-know)"));
    } else {
        v.blockers.push(format!("{msg} Re-run with --i-know if this is really intended."));
    }
}

/// Run every gate in one pass. `source_entries` / `target_entries` come from the two snapshots.
#[allow(clippy::too_many_arguments)]
fn check_space_vfs(
    label: &str,
    v: &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
    need: u64,
    min_free_pct: f64,
    out: &mut Verdict,
) {
    if need == 0 {
        return;
    }
    let Some((avail, total)) = v.free_space().ok().flatten() else {
        out.warnings.push(format!("{label}: cannot determine free space on {}", v.display()));
        return;
    };
    let need_padded = need.saturating_add(need / 10);
    let reserve = if min_free_pct > 0.0 { (total as f64 * min_free_pct) as u64 } else { 0 };
    if avail < need_padded.saturating_add(reserve) {
        out.blockers.push(format!(
            "{label}: not enough free space on {}: writing needs ~{} (plus {} reserve), only {} available",
            v.display(),
            human_bytes(need_padded),
            human_bytes(reserve),
            human_bytes(avail)
        ));
    }
}

/// `run_all` over a backend pair: the same gates, with root and space checks going
/// through the VFS (an sftp root honestly reports "cannot determine" instead of a number).
pub fn run_all_vfs(
    ops: &[Op],
    source: &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
    target: &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
    source_entries: u64,
    target_entries: u64,
    g: &Guards,
) -> Verdict {
    let mut v = Verdict { blockers: Vec::new(), warnings: Vec::new() };
    check_root_vfs("source", source, g.require_marker, &mut v);
    check_root_vfs("target", target, g.require_marker, &mut v);
    if !v.ok() {
        return v; // with a root unavailable, the later checks are meaningless
    }
    let st = stat_plan(ops);
    check_space_vfs("target", target, st.target.write_bytes, g.min_free_pct, &mut v);
    check_space_vfs("source", source, st.source.write_bytes, g.min_free_pct, &mut v);
    check_delete_ratio("target", &st.target, target_entries, g, &mut v);
    check_delete_ratio("source", &st.source, source_entries, g, &mut v);
    v
}

pub fn run_all(
    ops: &[Op],
    source_root: &Path,
    target_root: &Path,
    source_entries: u64,
    target_entries: u64,
    g: &Guards,
) -> Verdict {
    let mut v = Verdict { blockers: Vec::new(), warnings: Vec::new() };
    check_root("source", source_root, g.require_marker, &mut v);
    check_root("target", target_root, g.require_marker, &mut v);
    if !v.ok() {
        return v; // with a root unavailable, the later checks are meaningless
    }
    let st = stat_plan(ops);
    check_space("target", target_root, st.target.write_bytes, g.min_free_pct, &mut v);
    check_space("source", source_root, st.source.write_bytes, g.min_free_pct, &mut v);
    check_delete_ratio("target", &st.target, target_entries, g, &mut v);
    check_delete_ratio("source", &st.source, source_entries, g, &mut v);
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::plan::{Action, Op, Side};

    fn op(side: Side, action: Action, path: &str, size: Option<u64>) -> Op {
        Op {
            side,
            action,
            path: path.into(),
            from: None,
            size,
            mtime_ms: None,
            hash: None,
            link: None,
            mode: None,
            reason: "t".into(),
        }
    }

    #[test]
    fn stats_split_by_side() {
        let ops = vec![
            op(Side::Target, Action::Copy, "a", Some(100)),
            op(Side::Target, Action::Update, "b", Some(50)),
            op(Side::Target, Action::Delete, "c", Some(7)),
            op(Side::Source, Action::Copy, "d", Some(9)),
            op(Side::Target, Action::Conflict, "e", None),
        ];
        let st = stat_plan(&ops);
        assert_eq!(st.target.write_bytes, 150);
        assert_eq!(st.target.deletes, 1);
        assert_eq!(st.source.write_bytes, 9);
        assert_eq!(st.conflicts, 1);
    }

    #[test]
    fn delete_ratio_blocks_and_can_be_acknowledged() {
        let side = SideStats { deletes: 60, ..Default::default() };
        let g = Guards::default();
        let mut v = Verdict { blockers: vec![], warnings: vec![] };
        check_delete_ratio("target", &side, 100, &g, &mut v);
        assert_eq!(v.blockers.len(), 1, "60% deletion must be blocked");

        let g2 = Guards { acknowledged: true, ..Guards::default() };
        let mut v2 = Verdict { blockers: vec![], warnings: vec![] };
        check_delete_ratio("target", &side, 100, &g2, &mut v2);
        assert!(v2.ok(), "--i-know must let it through");
        assert_eq!(v2.warnings.len(), 1, "but it must still be reported");
    }

    #[test]
    fn small_deletions_pass() {
        let side = SideStats { deletes: 3, ..Default::default() };
        let mut v = Verdict { blockers: vec![], warnings: vec![] };
        check_delete_ratio("target", &side, 1000, &Guards::default(), &mut v);
        assert!(v.ok());
    }

    #[test]
    fn missing_marker_blocks_when_required() {
        let d = std::env::temp_dir().join(format!("syncdash-pf-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&d);
        let mut v = Verdict { blockers: vec![], warnings: vec![] };
        check_root("target", &d, true, &mut v);
        assert_eq!(v.blockers.len(), 1);

        write_marker(&d, "test-job", "").unwrap();
        let mut v2 = Verdict { blockers: vec![], warnings: vec![] };
        check_root("target", &d, true, &mut v2);
        assert!(v2.ok(), "marker present -> pass");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn disk_space_reports_something_for_temp_dir() {
        // Don't assert specific numbers, only that this machine can report them — failing to would degrade to a warning, not a block
        let got = disk_space(&std::env::temp_dir());
        assert!(got.is_some(), "free space query should work on the temp volume");
        let (avail, total) = got.unwrap();
        assert!(total > 0 && avail <= total);
    }
}
