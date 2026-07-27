//! apply: execute the plan (local / mounted mode).
//! - DRY-RUN by default; only --apply actually touches anything (same philosophy as GitDash: preview by default, never act silently)
//! - **every write is atomic**: temp file in the same directory → fsync → rename (see atomic.rs).
//!   An interruption can never leave half a file at the final path.
//! - deleted and overwritten files are moved into the local trash / `.version_syncDash` first (recoverable), never destroyed in place
//! - after a copy the mtime is set to the value from the source table and **read back for correction** — the next comparison's equality test depends on it
//! - v0.9 M1: execution runs in five **serial phases**, with the Copy/Update phase parallel inside
//!   (the only I/O-heavy class; width = `ApplyOptions::parallel`). Input order is not trusted — ops flipped
//!   in the desktop UI break the plan's rank ordering, so apply re-classifies and re-phases them itself.

use crate::compare::{Action, Op, Side};
use crate::foundation::path::join_native;
use crate::progress::{ApplyOutcome, ItemOutcome, Phase, PhaseProgress, RunCtx};
use filetime::FileTime;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;

/// Above this size, skip the in-memory delta path (don't read several GB into memory)
const DELTA_MEM_CAP: u64 = 1024 * 1024 * 1024;

pub struct ApplyOptions {
    pub dry_run: bool,
    pub trash: Option<PathBuf>,
    pub verbose: bool,
    /// paranoid rigor tier: after a copy/update, re-read the destination and verify blake3 (same as FFS "verify copied files")
    pub verify: bool,
    /// Versioning (optional): deleted/overwritten files go into each root's .version_syncDash/ instead of the local trash
    pub versioning: bool,
    /// Whether to fsync the temp file before the rename. Default true; can be turned off if it feels slow over SMB (at your own risk)
    pub fsync: bool,
    /// Used on directory deletion to decide "may the leftovers inside be removed along with it" (syncthing's `(?d)`)
    pub filter: Option<crate::filter::PathFilter>,
    /// Delta updates for local/mounted disks (see the comments on update_with_delta; off by default)
    pub delta: bool,
    /// Parallel width of the Copy/Update phase (1 = sequential). Default 4: over SMB, 2-4 streams basically
    /// saturate the uplink, and going wider only worsens queueing on the far end. Clamped to 1..=16.
    pub parallel: usize,
}

impl Default for ApplyOptions {
    fn default() -> Self {
        ApplyOptions {
            dry_run: true,
            trash: None,
            verbose: false,
            verify: false,
            versioning: false,
            fsync: true,
            filter: None,
            delta: false,
            parallel: 4,
        }
    }
}

fn default_trash() -> PathBuf {
    crate::trash::trash_root().join(crate::foundation::time::now_ms().to_string())
}

fn move_to_trash(file: &Path, rel: &str, trash: &Path) -> std::io::Result<()> {
    let dest = join_native(trash, rel);
    if let Some(p) = dest.parent() {
        std::fs::create_dir_all(p)?;
    }
    match std::fs::rename(file, &dest) {
        Ok(_) => Ok(()),
        Err(_) => {
            // Cross-volume: fall back to copy+delete
            std::fs::copy(file, &dest)?;
            std::fs::remove_file(file)
        }
    }
}

fn set_mtime(path: &Path, mtime_ms: i64) {
    let ft = FileTime::from_unix_time(mtime_ms / 1000, ((mtime_ms % 1000) * 1_000_000) as u32);
    let _ = filetime::set_file_mtime(path, ft);
}

/// Read a file's mtime (unix milliseconds). None only means "could not read it" (metadata/modified failed);
/// the caller uses that to decide "then don't set the mtime". The conversion itself is left to `foundation::time::systime_ms`.
fn read_mtime_ms(path: &Path) -> Option<i64> {
    let t = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(crate::foundation::time::systime_ms(t))
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}
#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> std::io::Result<()> {
    // Windows has no unix permission bits. A plan carrying a mode can only arise between unix↔unix,
    // so reaching here means the executing side is Windows — skip silently rather than error.
    Ok(())
}

#[cfg(unix)]
fn create_symlink(target: &str, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}
#[cfg(windows)]
fn create_symlink(target: &str, link: &Path) -> std::io::Result<()> {
    use std::os::windows::fs::{symlink_dir, symlink_file};
    // If the file link fails (target is a directory, or permissions), try a directory link; if that fails too, report it honestly (Windows needs developer mode or admin)
    symlink_file(target, link).or_else(|_| symlink_dir(target, link))
}

fn exists_no_follow(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok()
}

/// Classified outcome of a failed directory deletion (P0-4).
/// This used to be `Err(_) => Ok(())`: safe behavior (no recursive delete) but **completely silent** —
/// the user sees "0 errors" while the directory is still there, the next comparison emits the same DeleteDir again, and it never converges.
enum DirOutcome {
    Removed,
    /// It was never there in the first place
    Absent,
    /// Non-empty; `sample` names a few of the leftovers so the error can say what is still in there.
    /// Deletability was already decided before this point — a directory whose leftovers all match
    /// `deletable` is removed outright and reports `Removed`, so reaching here means it is staying.
    NotEmpty { sample: Vec<String> },
    Failed(std::io::Error),
}

fn try_delete_dir(dst: &Path, rel: &str, filter: Option<&crate::filter::PathFilter>) -> DirOutcome {
    match std::fs::remove_dir(dst) {
        Ok(_) => return DirOutcome::Removed,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return DirOutcome::Absent,
        Err(e) if e.kind() != std::io::ErrorKind::DirectoryNotEmpty && e.raw_os_error() != Some(145) && e.raw_os_error() != Some(39) => {
            // 145 = ERROR_DIR_NOT_EMPTY (Windows), 39 = ENOTEMPTY (unix).
            // Any other error (permissions, etc.) is reported as-is
            return DirOutcome::Failed(e);
        }
        Err(_) => {}
    }
    // Non-empty: see what is left inside
    let mut sample = Vec::new();
    let mut all_deletable = true;
    let mut count = 0usize;
    for e in walkdir::WalkDir::new(dst).follow_links(false).min_depth(1).into_iter().flatten() {
        if !e.file_type().is_file() {
            continue;
        }
        count += 1;
        let child_rel = format!(
            "{}/{}",
            rel.trim_end_matches('/'),
            // fall back to the whole path if strip_prefix fails, as before
            crate::foundation::path::to_rel(e.path(), dst)
                .unwrap_or_else(|| e.path().to_string_lossy().replace('\\', "/"))
        );
        let deletable = filter.map(|f| f.is_deletable(&child_rel)).unwrap_or(false);
        if !deletable {
            all_deletable = false;
        }
        if sample.len() < 5 {
            sample.push(child_rel);
        }
    }
    if count == 0 {
        // Only subdirectories left: recursively remove the empty directory tree
        return match std::fs::remove_dir_all(dst) {
            Ok(_) => DirOutcome::Removed,
            Err(e) => DirOutcome::Failed(e),
        };
    }
    if all_deletable {
        // Everything left is a `(?d)` deletable item / atomic-write debris → remove it along with the directory (same semantics as syncthing)
        return match std::fs::remove_dir_all(dst) {
            Ok(_) => DirOutcome::Removed,
            Err(e) => DirOutcome::Failed(e),
        };
    }
    DirOutcome::NotEmpty { sample }
}

/// Delta update (P1-1 step B, opt-in).
///
/// How it works: first copy dst into a temp file in the same directory (`fs::copy` uses server-side copychunk
/// on SMB2+, and is near-free on local filesystems with reflink support), then write only the **FastCDC chunks
/// whose content differs** into that temp file, and rename at the end. Same atomic path, no loss of interruption safety.
///
/// The trade-off, stated plainly: this path reads dst one extra time (a remote read) to avoid writing a lot of bytes (remote writes).
/// It is a net win where writes cost far more than reads (SMB / WAN uplinks) and a wash on symmetric links — hence off by default, enabled explicitly per job.
/// The remote pack pipeline (already present in v0.7) is where the delta payoff is most certain.
fn update_with_delta(
    src: &Path,
    dst: &Path,
    staged: &mut crate::atomic::Staged,
) -> std::io::Result<Option<(u64, u64, String)>> {
    let (Ok(smd), Ok(dmd)) = (std::fs::metadata(src), std::fs::metadata(dst)) else {
        return Ok(None);
    };
    if smd.len() < crate::chunk::DELTA_MIN_SIZE
        || smd.len() > DELTA_MEM_CAP
        || dmd.len() > DELTA_MEM_CAP
    {
        return Ok(None);
    }
    let old = std::fs::read(dst)?;
    let new = std::fs::read(src)?;
    // Lay the whole old content into the temp file first (the opening for server-side copy / reflink)
    staged.write_at(0, &old)?;
    let old_chunks = crate::chunk::chunk_bytes(&old);
    let new_chunks = crate::chunk::chunk_bytes(&new);
    let have: std::collections::HashMap<&str, (u64, u32)> =
        old_chunks.iter().map(|c| (c.hash.as_str(), (c.off, c.len))).collect();
    let mut written = 0u64;
    for c in &new_chunks {
        let start = c.off as usize;
        let end = start + c.len as usize;
        // Only when the chunk content matches **and it sits at the same offset** can this write be skipped
        if let Some(&(off, len)) = have.get(c.hash.as_str()) {
            if off == c.off && len == c.len {
                continue;
            }
        }
        staged.write_at(c.off, &new[start..end])?;
        written += c.len as u64;
    }
    // A shorter new file needs its tail truncated
    if (new.len() as u64) < old.len() as u64 {
        let f = std::fs::OpenOptions::new().write(true).open(staged.path())?;
        f.set_len(new.len() as u64)?;
    }
    // The new content is right there in memory — hash it in full while we're at it, for post-copy verification against the readback from disk
    let h = blake3::hash(&new).to_hex().to_string();
    Ok(Some((written, new.len() as u64, h)))
}

/// Execution environment shared by the worker threads. All counters are atomic and writers sit behind locks — a worker only borrows &Shared.
struct Shared<'a> {
    opt: &'a ApplyOptions,
    ctx: &'a RunCtx,
    source_root: &'a Path,
    target_root: &'a Path,
    trash: PathBuf,
    ver_source: Mutex<Option<crate::version::VersionWriter>>,
    ver_target: Mutex<Option<crate::version::VersionWriter>>,
    // P1-4: when the mtime the filesystem actually stored differs from the one we wanted (FAT's 2-second
    // granularity, truncation by some SMB servers), record (ondisk, intended) for the next scan to convert with,
    // instead of brute-forcing it with a ±2s tolerance. Same approach as syncthing's mtimeFS.
    mtime_fixes: Mutex<Vec<(bool, String, i64, i64)>>,
    delta_saved: AtomicU64,
}

#[derive(Default)]
struct Counters {
    done: AtomicU64,
    skipped: AtomicU64,
    errors: AtomicU64,
}

/// Archive the original before it is overwritten/deleted (trash or .version_syncDash)
fn preserve(sh: &Shared, op: &Op, exec_root: &Path, dst: &Path, why: &str, newer: Option<&Path>) -> std::io::Result<()> {
    if sh.opt.versioning {
        let slot = if op.side == Side::Source { &sh.ver_source } else { &sh.ver_target };
        let mut w = slot.lock().unwrap();
        if w.is_none() {
            *w = Some(crate::version::VersionWriter::begin(exec_root)?);
        }
        w.as_mut().unwrap().preserve(&op.path, dst, newer, why)
    } else {
        move_to_trash(dst, &op.path, &sh.trash)
    }
}

/// Execute a single op. Cancel/pause are honoured at chunk boundaries via `pp.checkpoint()`;
/// a cancel returns Interrupted, and Staged::Drop guarantees no debris at the destination.
fn exec_op(sh: &Shared, op: &Op, pp: &PhaseProgress) -> std::io::Result<()> {
    let (exec_root, other_root) = match op.side {
        Side::Target => (sh.target_root, sh.source_root),
        Side::Source => (sh.source_root, sh.target_root),
    };
    let dst = join_native(exec_root, &op.path);
    match op.action {
        Action::Copy | Action::Update => {
            if let Some(p) = dst.parent() {
                std::fs::create_dir_all(p)?;
            }
            // symlink op: create the link itself, don't copy content (a link is metadata; atomic writes don't apply)
            if let Some(target) = &op.link {
                if exists_no_follow(&dst) {
                    preserve(sh, op, exec_root, &dst, "overwritten", None)?;
                }
                return create_symlink(target, &dst);
            }
            let src = join_native(other_root, &op.path);

            // atomic write (P0-1)
            let mut staged = crate::atomic::Staged::create(&dst)?;
            let mut used_delta = false;
            // The expected value for post-copy verification = **the full blake3 of this copy stream** (the copy reads
            // the whole file anyway, so hashing on the stream is free). Not op.hash — scan evidence may be only a sampled
            // digest (`~` prefix), whereas the correctness of the write path must hold over the entire file.
            let mut expect_hash: Option<String> = None;
            if sh.opt.delta && op.action == Action::Update && exists_no_follow(&dst) {
                if let Some((written, total, h)) = update_with_delta(&src, &dst, &mut staged)? {
                    sh.delta_saved.fetch_add(total.saturating_sub(written), Ordering::Relaxed);
                    // Progress is booked as "this file is done" (same basis as bytes_total); the bytes saved are reported separately
                    pp.add_bytes(total, &op.path);
                    expect_hash = Some(h);
                    used_delta = true;
                }
            }
            if !used_delta {
                if sh.opt.verify {
                    let mut hasher = blake3::Hasher::new();
                    staged.copy_from(&src, &mut |chunk| {
                        hasher.update(chunk);
                        pp.add_bytes(chunk.len() as u64, &op.path);
                        pp.checkpoint()
                    })?;
                    expect_hash = Some(hasher.finalize().to_hex().to_string());
                } else {
                    staged.copy_from(&src, &mut |chunk| {
                        pp.add_bytes(chunk.len() as u64, &op.path);
                        pp.checkpoint()
                    })?;
                }
            }
            staged.seal(sh.opt.fsync)?;

            // Verification runs on the temp file: readback from disk vs the copy stream — a failure never becomes the final file
            if sh.opt.verify {
                if let Some(expect) = &expect_hash {
                    let mut hasher = blake3::Hasher::new();
                    hasher.update_mmap_rayon(staged.path())?;
                    let got = hasher.finalize().to_hex().to_string();
                    if &got != expect {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            format!("write verify failed: staged readback {got} != copy stream {expect}"),
                        ));
                    }
                }
            }
            // mtime / mode are set on the temp file too, so the rename lands it already in its final state
            let intended = match op.mtime_ms {
                Some(mt) => {
                    set_mtime(staged.path(), mt);
                    Some(mt)
                }
                None => read_mtime_ms(&src).inspect(|mt| set_mtime(staged.path(), *mt)),
            };
            if let Some(m) = op.mode {
                set_mode(staged.path(), m)?;
            }
            // Archive the old file at the last moment before commit, so the window is a single rename
            if exists_no_follow(&dst) {
                preserve(sh, op, exec_root, &dst, "overwritten", Some(&src))?;
            }
            staged.commit()?;

            // mtime readback correction (P1-4)
            if let Some(want) = intended {
                if let Some(got) = read_mtime_ms(&dst) {
                    if got != want {
                        sh.mtime_fixes.lock().unwrap().push((op.side == Side::Source, op.path.clone(), got, want));
                    }
                }
            }
            Ok(())
        }
        Action::Chmod => {
            let m = op.mode.ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "chmod op without mode")
            })?;
            set_mode(&dst, m)
        }
        Action::Move => {
            let from = join_native(exec_root, op.from.as_deref().unwrap_or_default());
            if let Some(p) = dst.parent() {
                std::fs::create_dir_all(p)?;
            }
            match std::fs::rename(&from, &dst) {
                Ok(_) => Ok(()),
                Err(_) => {
                    // Cross-volume fallback: still an atomic write, so an interruption leaves nothing half-done.
                    // Move's bytes are not part of bytes_total, so only hook the checkpoint here, don't count bytes.
                    let mut staged = crate::atomic::Staged::create(&dst)?;
                    staged.copy_from(&from, &mut |_| pp.checkpoint())?;
                    staged.seal(sh.opt.fsync)?;
                    if let Some(mt) = read_mtime_ms(&from) {
                        set_mtime(staged.path(), mt);
                    }
                    staged.commit()?;
                    std::fs::remove_file(&from)
                }
            }
        }
        Action::Delete => {
            // symlink_metadata: exists() falsely reports false for a broken symlink, so don't follow here
            if exists_no_follow(&dst) {
                preserve(sh, op, exec_root, &dst, "deleted", None)?;
            }
            Ok(())
        }
        Action::DeleteDir => {
            // P0-4: report by classification, no longer swallowed silently
            match try_delete_dir(&dst, &op.path, sh.opt.filter.as_ref()) {
                DirOutcome::Removed | DirOutcome::Absent => Ok(()),
                DirOutcome::NotEmpty { sample } => Err(std::io::Error::new(
                    std::io::ErrorKind::DirectoryNotEmpty,
                    format!(
                        "directory not empty, kept: {} (protected by filters or unknown to the plan). \
                         Add them to `deletable` in the job to have them removed with the directory.",
                        sample.join(", ")
                    ),
                )),
                DirOutcome::Failed(e) => Err(e),
            }
        }
        Action::Conflict | Action::Note => Ok(()),
    }
}

/// Book the result of a single op: an error never aborts the whole run (FFS accumulating semantics),
/// each one emits an Error event through the sink (the first time the windowed desktop build can really see errors) plus keeps the eprintln for the CLI.
fn record(sh: &Shared, op: &Op, res: std::io::Result<()>, pp: &PhaseProgress, acc: &Counters, ms: u64) {
    let label = format!(
        "[{}] {:?} {}",
        if op.side == Side::Target { "target" } else { "source" },
        op.action,
        op.path
    );
    let side = if op.side == Side::Target { "target" } else { "source" };
    // Every op's outcome emits one ItemResult — this is the only place in the codebase that knows "did this one actually succeed";
    // outside this function all that remains are three aggregate counters. The execution ledger (items.jsonl) rests entirely on it.
    let ledger = |outcome: ItemOutcome| {
        sh.ctx.sink.emit(crate::progress::ProgressEvent::ItemResult {
            ts_ms: crate::foundation::time::now_ms(),
            path: op.path.clone(),
            action: format!("{:?}", op.action),
            side: side.to_string(),
            outcome,
            // The item's own size (a delta update actually writes fewer bytes — that is a link metric, not a ledger metric)
            bytes: op.size.unwrap_or(0),
            ms,
        });
    };
    match res {
        Ok(_) => {
            acc.done.fetch_add(1, Ordering::Relaxed);
            if sh.opt.verbose {
                println!("OK   {label}");
            }
            ledger(ItemOutcome::Ok);
            pp.item_done(&op.path);
        }
        Err(e) if e.kind() == std::io::ErrorKind::DirectoryNotEmpty => {
            // Keeping the directory is not an error (protecting a filtered file is correct), but it must be visible
            acc.skipped.fetch_add(1, Ordering::Relaxed);
            println!("KEPT      {} ({e})", op.path);
            ledger(ItemOutcome::Kept);
            pp.item_done(&op.path);
        }
        Err(e) if crate::progress::is_cancelled(&e) && sh.ctx.ctl.cancelled() => {
            // User cancelled: this op not completing is not an error — cancelled=true in the summary says it all.
            // But it must leave a trace in the ledger, or "why wasn't this one done" has no answer after the fact.
            ledger(ItemOutcome::Cancelled);
        }
        Err(e) => {
            acc.errors.fetch_add(1, Ordering::Relaxed);
            crate::log_error!("apply", "ERR  {label}: {e}");
            pp.error(&op.path, &format!("{:?}", op.action), side, &e.to_string());
            ledger(ItemOutcome::Failed);
            pp.item_done(&op.path);
        }
    }
}

/// Run one class of ops. width==1 runs sequentially on the current thread; otherwise a scoped thread pool
/// (an AtomicUsize work-ticket index rather than range splitting — one big file can't drag a worker into a long tail).
/// No rayon: verify's blake3 already runs in rayon's global pool, and nesting would oversubscribe and tangle the pause semantics.
fn run_class(class: &[&Op], width: usize, sh: &Shared, pp: &PhaseProgress, acc: &Counters) {
    if class.is_empty() {
        return;
    }
    let width = width.clamp(1, 16).min(class.len());
    let next = AtomicUsize::new(0);
    let work = || {
        loop {
            let i = next.fetch_add(1, Ordering::Relaxed);
            if i >= class.len() {
                break;
            }
            let op = class[i];
            // The cooperation point between two adjacent ops (a pause spins here, a cancel exits here)
            if pp.checkpoint().is_err() {
                break;
            }
            // Per-item timing: "which file slowed this sync down" in the execution ledger can only be measured here
            let t_op = std::time::Instant::now();
            let res = exec_op(sh, op, pp);
            let bail = matches!(&res, Err(e) if crate::progress::is_cancelled(e) && sh.ctx.ctl.cancelled());
            record(sh, op, res, pp, acc, t_op.elapsed().as_millis() as u64);
            if bail {
                break;
            }
        }
    };
    if width == 1 {
        work();
    } else {
        std::thread::scope(|s| {
            for _ in 0..width {
                s.spawn(&work);
            }
        });
    }
}

pub fn apply(ops: &[Op], source_root: &Path, target_root: &Path, opt: &ApplyOptions) -> (u64, u64, u64) {
    apply_with(ops, source_root, target_root, opt, &RunCtx::null()).into_tuple()
}

/// v0.9 M1: the execution body with progress/cancel/pause. Five serial phases:
/// Moves → **Copy/Update (parallel)** → Chmod → Delete → DeleteDir (deepest-first within the class).
/// Updates with delta enabled stay in the serial lane — update_with_delta reads both the old and the new copy into memory (≤1GB cap),
/// and 4 parallel workers × a 2GB peak is not acceptable.
pub fn apply_with(
    ops: &[Op],
    source_root: &Path,
    target_root: &Path,
    opt: &ApplyOptions,
    ctx: &RunCtx,
) -> ApplyOutcome {
    // Apply's totals are known before it starts: the UI percentage formula is valid from t=0
    let items_total = ops.iter().filter(|o| !matches!(o.action, Action::Conflict | Action::Note)).count() as u64;
    let bytes_total: u64 = ops
        .iter()
        .filter(|o| matches!(o.action, Action::Copy | Action::Update) && o.link.is_none())
        .filter_map(|o| o.size)
        .sum();
    let pp = PhaseProgress::begin(ctx, Phase::Apply, None, items_total, bytes_total);
    let acc = Counters::default();

    // Conflict/Note are report-only and unrelated to the execution phases — get them out of the way first
    for op in ops {
        match op.action {
            Action::Conflict => {
                println!("CONFLICT  {} ({})", op.path, op.reason);
                acc.skipped.fetch_add(1, Ordering::Relaxed);
            }
            Action::Note => {
                println!("NOTE      {} ({} from={})", op.path, op.reason, op.from.clone().unwrap_or_default());
                acc.skipped.fetch_add(1, Ordering::Relaxed);
            }
            _ => {}
        }
    }

    if opt.dry_run {
        for op in ops.iter().filter(|o| !matches!(o.action, Action::Conflict | Action::Note)) {
            if opt.verbose {
                let label = format!(
                    "[{}] {:?} {}",
                    if op.side == Side::Target { "target" } else { "source" },
                    op.action,
                    op.path
                );
                println!("DRY  {label}  ({})", op.reason);
            }
            acc.skipped.fetch_add(1, Ordering::Relaxed);
        }
        return ApplyOutcome {
            done: 0,
            skipped: acc.skipped.load(Ordering::Relaxed),
            errors: 0,
            bytes_copied: 0,
            cancelled: false,
        };
    }

    // The FFS dir_lock idea: lock both roots (with a heartbeat) before touching anything, so two machines cannot apply to the same directory at once.
    // Pause spins on 100ms instead of suspending and returning precisely so these two locks' heartbeat threads keep beating while paused.
    let _lock_guard: (crate::lock::RootLock, crate::lock::RootLock) = {
        let ls = match crate::lock::RootLock::acquire(source_root) {
            Ok(l) => l,
            Err(e) => {
                crate::log_error!("apply", "cannot lock source root: {e}");
                return ApplyOutcome { done: 0, skipped: ops.len() as u64, errors: 1, bytes_copied: 0, cancelled: false };
            }
        };
        let lt = match crate::lock::RootLock::acquire(target_root) {
            Ok(l) => l,
            Err(e) => {
                crate::log_error!("apply", "cannot lock target root: {e}");
                return ApplyOutcome { done: 0, skipped: ops.len() as u64, errors: 1, bytes_copied: 0, cancelled: false };
            }
        };
        (ls, lt)
    };

    let sh = Shared {
        opt,
        ctx,
        source_root,
        target_root,
        trash: opt.trash.clone().unwrap_or_else(default_trash),
        ver_source: Mutex::new(None),
        ver_target: Mutex::new(None),
        mtime_fixes: Mutex::new(Vec::new()),
        delta_saved: AtomicU64::new(0),
    };

    // Split into phases (input order is not trusted)
    let mut moves: Vec<&Op> = Vec::new();
    let mut copies: Vec<&Op> = Vec::new();
    let mut copies_delta: Vec<&Op> = Vec::new();
    let mut chmods: Vec<&Op> = Vec::new();
    let mut deletes: Vec<&Op> = Vec::new();
    let mut deldirs: Vec<&Op> = Vec::new();
    for op in ops {
        match op.action {
            Action::Move => moves.push(op),
            Action::Copy | Action::Update => {
                if opt.delta && op.action == Action::Update && op.link.is_none() {
                    copies_delta.push(op);
                } else {
                    copies.push(op);
                }
            }
            Action::Chmod => chmods.push(op),
            Action::Delete => deletes.push(op),
            Action::DeleteDir => deldirs.push(op),
            Action::Conflict | Action::Note => {}
        }
    }
    // Delete deep directories first, so a parent has a chance to become empty
    deldirs.sort_by_key(|o| std::cmp::Reverse(o.path.matches('/').count()));

    // The same (side, path) appearing twice (a plan shouldn't generate that, but flipping direction by hand can) → parallel would race on the write, so force sequential
    let mut seen = std::collections::HashSet::new();
    let has_dup = copies.iter().any(|o| !seen.insert((o.side == Side::Source, o.path.as_str())));
    let width = if has_dup { 1 } else { opt.parallel };

    run_class(&moves, 1, &sh, &pp, &acc);
    run_class(&copies, width, &sh, &pp, &acc);
    run_class(&copies_delta, 1, &sh, &pp, &acc);
    run_class(&chmods, 1, &sh, &pp, &acc);
    run_class(&deletes, 1, &sh, &pp, &acc);
    run_class(&deldirs, 1, &sh, &pp, &acc);

    // serial wrap-up
    let mtime_fixes = std::mem::take(&mut *sh.mtime_fixes.lock().unwrap());
    if !mtime_fixes.is_empty() {
        let mut src_fix = Vec::new();
        let mut tgt_fix = Vec::new();
        for (is_source, rel, ondisk, intended) in mtime_fixes {
            if is_source {
                src_fix.push((rel, ondisk, intended));
            } else {
                tgt_fix.push((rel, ondisk, intended));
            }
        }
        if !src_fix.is_empty() {
            crate::scan::record_mtime_fixes(source_root, &src_fix);
        }
        if !tgt_fix.is_empty() {
            crate::scan::record_mtime_fixes(target_root, &tgt_fix);
        }
    }
    let delta_saved = sh.delta_saved.load(Ordering::Relaxed);
    if delta_saved > 0 {
        println!("delta: {} not re-written", crate::foundation::fmt::human_bytes(delta_saved));
    }
    if let Some(w) = sh.ver_source.into_inner().unwrap() {
        let side_ops: Vec<Op> = ops.iter().filter(|o| o.side == Side::Source).cloned().collect();
        if let Ok(Some(id)) = w.finish(&side_ops) {
            println!("version saved: {} (id {id})", source_root.join(crate::foundation::names::VERSION_STORE_DIR).display());
        }
    }
    if let Some(w) = sh.ver_target.into_inner().unwrap() {
        let side_ops: Vec<Op> = ops.iter().filter(|o| o.side == Side::Target).cloned().collect();
        if let Ok(Some(id)) = w.finish(&side_ops) {
            println!("version saved: {} (id {id})", target_root.join(crate::foundation::names::VERSION_STORE_DIR).display());
        }
    }
    let done = acc.done.load(Ordering::Relaxed);
    if !opt.versioning && done > 0 {
        println!("trash (deleted/overwritten files kept at): {}", sh.trash.display());
    }
    ApplyOutcome {
        done,
        skipped: acc.skipped.load(Ordering::Relaxed),
        errors: acc.errors.load(Ordering::Relaxed),
        bytes_copied: pp.counts().2,
        cancelled: ctx.ctl.cancelled(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compare::{Action, Op, Side};

    fn tmproot(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("syncdash-apply-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn op(action: Action, path: &str) -> Op {
        Op {
            side: Side::Target,
            action,
            path: path.into(),
            from: None,
            size: None,
            mtime_ms: None,
            hash: None,
            link: None,
            mode: None,
            reason: "test".into(),
        }
    }

    fn opts(trash: PathBuf) -> ApplyOptions {
        ApplyOptions { dry_run: false, trash: Some(trash), fsync: false, ..Default::default() }
    }

    /// A ctx that collects ItemResult: the contents of the execution ledger (items.jsonl) are exactly these events
    fn ledger_ctx() -> (RunCtx, std::sync::Arc<Mutex<Vec<(String, ItemOutcome, u64)>>>) {
        let store: std::sync::Arc<Mutex<Vec<(String, ItemOutcome, u64)>>> =
            std::sync::Arc::new(Mutex::new(Vec::new()));
        let s2 = store.clone();
        let sink = move |ev: crate::progress::ProgressEvent| {
            if let crate::progress::ProgressEvent::ItemResult { path, outcome, bytes, .. } = ev {
                s2.lock().unwrap().push((path, outcome, bytes));
            }
        };
        (RunCtx::new(crate::progress::RunCtl::new(), std::sync::Arc::new(sink)), store)
    }

    #[test]
    fn ledger_records_ok_kept_and_failed_per_item() {
        let base = tmproot("ledger");
        let (s, t) = (base.join("s"), base.join("t"));
        std::fs::create_dir_all(&s).unwrap();
        std::fs::create_dir_all(t.join("d")).unwrap();
        std::fs::write(s.join("a.txt"), b"hello").unwrap();
        std::fs::write(t.join("d").join("protected.log"), b"x").unwrap();

        let (ctx, log) = ledger_ctx();
        let ops = [
            op(Action::Copy, "a.txt"),          // → ok
            op(Action::DeleteDir, "d"),         // → kept (non-empty and its contents are not deletable)
            op(Action::Copy, "missing.txt"),    // → failed (the source file does not exist)
        ];
        let out = apply_with(&ops, &s, &t, &opts(base.join("trash")), &ctx);
        assert_eq!((out.done, out.skipped, out.errors), (1, 1, 1));

        let rows = log.lock().unwrap();
        // Key invariant: every entry in the plan leaves a trace in the ledger — not one more, not one fewer
        assert_eq!(rows.len(), ops.len(), "every op must leave a trace: {rows:?}");
        let find = |p: &str| rows.iter().find(|(path, _, _)| path == p).map(|(_, o, _)| *o);
        assert_eq!(find("a.txt"), Some(ItemOutcome::Ok));
        assert_eq!(find("d"), Some(ItemOutcome::Kept), "keeping the directory is not an error, but it must be traceable");
        assert_eq!(find("missing.txt"), Some(ItemOutcome::Failed));
        drop(rows);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn ledger_marks_untouched_items_as_cancelled() {
        let base = tmproot("ledgercancel");
        let (s, t) = (base.join("s"), base.join("t"));
        std::fs::create_dir_all(&s).unwrap();
        std::fs::create_dir_all(&t).unwrap();
        for i in 0..6 {
            std::fs::write(s.join(format!("f{i}.txt")), vec![b'x'; 4096]).unwrap();
        }
        let (ctx, log) = ledger_ctx();
        ctx.ctl.request_cancel(); // not a single one gets its turn to run
        let ops: Vec<Op> = (0..6).map(|i| op(Action::Copy, &format!("f{i}.txt"))).collect();
        let out = apply_with(&ops, &s, &t, &opts(base.join("trash")), &ctx);

        assert_eq!(out.done, 0);
        let rows = log.lock().unwrap();
        // Assert honestly: the checkpoint stopped things **before any work began**, so not a single op ran
        // and the ledger naturally holds no rows. `all()` is vacuously true on an empty set, which would
        // turn this into a false pass — the row count must be pinned explicitly, or a future missing emit in record would go unnoticed.
        assert_eq!(rows.len(), 0, "the ledger must be empty when the cancel lands before any op: {rows:?}");
        assert!(rows.iter().all(|(_, o, _)| *o == ItemOutcome::Cancelled));
        drop(rows);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn copy_lands_atomically_and_leaves_no_temp_files() {
        let base = tmproot("copy");
        let (s, t) = (base.join("s"), base.join("t"));
        std::fs::create_dir_all(&s).unwrap();
        std::fs::create_dir_all(&t).unwrap();
        std::fs::write(s.join("a.txt"), b"hello").unwrap();

        let (done, _, errors) = apply(&[op(Action::Copy, "a.txt")], &s, &t, &opts(base.join("trash")));
        assert_eq!((done, errors), (1, 0));
        assert_eq!(std::fs::read(t.join("a.txt")).unwrap(), b"hello");
        let leftovers: Vec<_> = std::fs::read_dir(&t)
            .unwrap()
            .flatten()
            .filter(|e| crate::atomic::is_temp_name(&e.file_name().to_string_lossy()))
            .collect();
        assert!(leftovers.is_empty(), "no temp files may survive a successful apply");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn failed_update_leaves_the_original_intact() {
        // The source file does not exist → the copy is bound to fail. The destination must be untouched, which is exactly what the atomic write guarantees:
        // fs::copy used to write the destination directly, so a failure left truncated content behind and the next sync propagated it back to source.
        let base = tmproot("fail");
        let (s, t) = (base.join("s"), base.join("t"));
        std::fs::create_dir_all(&s).unwrap();
        std::fs::create_dir_all(&t).unwrap();
        std::fs::write(t.join("keep.txt"), b"precious original").unwrap();

        let (done, _, errors) = apply(&[op(Action::Update, "keep.txt")], &s, &t, &opts(base.join("trash")));
        assert_eq!(done, 0);
        assert_eq!(errors, 1);
        assert_eq!(
            std::fs::read(t.join("keep.txt")).unwrap(),
            b"precious original",
            "a failed update must never damage the destination"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn update_preserves_old_content_in_trash() {
        let base = tmproot("trash");
        let (s, t, tr) = (base.join("s"), base.join("t"), base.join("trash"));
        std::fs::create_dir_all(&s).unwrap();
        std::fs::create_dir_all(&t).unwrap();
        std::fs::write(s.join("f.txt"), b"new").unwrap();
        std::fs::write(t.join("f.txt"), b"old").unwrap();

        let (done, _, errors) = apply(&[op(Action::Update, "f.txt")], &s, &t, &opts(tr.clone()));
        assert_eq!((done, errors), (1, 0));
        assert_eq!(std::fs::read(t.join("f.txt")).unwrap(), b"new");
        assert_eq!(std::fs::read(tr.join("f.txt")).unwrap(), b"old", "old version must be recoverable");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn delete_dir_reports_kept_contents_instead_of_silence() {
        let base = tmproot("deldir");
        let (s, t) = (base.join("s"), base.join("t"));
        std::fs::create_dir_all(&s).unwrap();
        std::fs::create_dir_all(t.join("d")).unwrap();
        std::fs::write(t.join("d").join("protected.log"), b"x").unwrap();

        // No filter → the leftovers are not deletable → count it as skipped and print the reason, rather than pretending it succeeded
        let (done, skipped, errors) = apply(&[op(Action::DeleteDir, "d")], &s, &t, &opts(base.join("trash")));
        assert_eq!(done, 0, "the directory was not actually removed");
        assert_eq!(skipped, 1, "it must be reported, not silently counted as done");
        assert_eq!(errors, 0, "keeping a protected file is not an error");
        assert!(t.join("d").is_dir());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn delete_dir_removes_when_leftovers_are_deletable() {
        let base = tmproot("deldir2");
        let (s, t) = (base.join("s"), base.join("t"));
        std::fs::create_dir_all(&s).unwrap();
        std::fs::create_dir_all(t.join("d")).unwrap();
        std::fs::write(t.join("d").join("cache.tmp"), b"x").unwrap();

        let mut o = opts(base.join("trash"));
        o.filter = Some(crate::filter::PathFilter::build_full(&[], &[], &["*/*.tmp".to_string()]));
        let (done, _, errors) = apply(&[op(Action::DeleteDir, "d")], &s, &t, &o);
        assert_eq!((done, errors), (1, 0));
        assert!(!t.join("d").exists(), "deletable leftovers must not block the directory removal");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn delta_update_produces_identical_content() {
        let base = tmproot("delta");
        let (s, t) = (base.join("s"), base.join("t"));
        std::fs::create_dir_all(&s).unwrap();
        std::fs::create_dir_all(&t).unwrap();
        // Larger than DELTA_MIN_SIZE, with only a short stretch in the middle differing
        let mut old = vec![0u8; 6 * 1024 * 1024];
        for (i, b) in old.iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        let mut new = old.clone();
        new[3_000_000..3_001_000].fill(0xAB);
        std::fs::write(s.join("big.bin"), &new).unwrap();
        std::fs::write(t.join("big.bin"), &old).unwrap();

        let mut o = opts(base.join("trash"));
        o.delta = true;
        let (done, _, errors) = apply(&[op(Action::Update, "big.bin")], &s, &t, &o);
        assert_eq!((done, errors), (1, 0));
        assert_eq!(std::fs::read(t.join("big.bin")).unwrap(), new, "delta path must be byte-exact");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn delta_update_handles_shrinking_files() {
        let base = tmproot("delta2");
        let (s, t) = (base.join("s"), base.join("t"));
        std::fs::create_dir_all(&s).unwrap();
        std::fs::create_dir_all(&t).unwrap();
        let old = vec![7u8; 8 * 1024 * 1024];
        let new = vec![7u8; 5 * 1024 * 1024];
        std::fs::write(s.join("shrink.bin"), &new).unwrap();
        std::fs::write(t.join("shrink.bin"), &old).unwrap();

        let mut o = opts(base.join("trash"));
        o.delta = true;
        let (done, _, errors) = apply(&[op(Action::Update, "shrink.bin")], &s, &t, &o);
        assert_eq!((done, errors), (1, 0));
        assert_eq!(std::fs::read(t.join("shrink.bin")).unwrap().len(), new.len(), "tail must be truncated");
        let _ = std::fs::remove_dir_all(&base);
    }

    // v0.9 M1: parallelism / progress / cancellation

    use crate::progress::{ProgressEvent, RunCtl, RunCtx};
    use std::sync::Arc;

    fn collecting_ctx() -> (RunCtx, Arc<Mutex<Vec<ProgressEvent>>>) {
        let store: Arc<Mutex<Vec<ProgressEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let s2 = store.clone();
        let sink = move |ev: ProgressEvent| {
            s2.lock().unwrap().push(ev);
        };
        (RunCtx::new(RunCtl::new(), Arc::new(sink)), store)
    }

    /// Parallel and sequential must produce byte-identical result trees (including the set preserved in trash)
    #[test]
    fn parallel_and_sequential_agree() {
        let run = |tag: &str, parallel: usize| -> (PathBuf, PathBuf, (u64, u64, u64)) {
            let base = tmproot(&format!("par-{tag}"));
            let (s, t) = (base.join("s"), base.join("t"));
            std::fs::create_dir_all(&s).unwrap();
            std::fs::create_dir_all(&t).unwrap();
            let mut ops = Vec::new();
            for i in 0..12 {
                let name = format!("f{i:02}.bin");
                std::fs::write(s.join(&name), vec![i as u8; 20_000 + i * 1000]).unwrap();
                ops.push(op(Action::Copy, &name));
            }
            // Mix in one update (old content goes to trash) and one delete
            std::fs::write(s.join("up.txt"), b"NEW").unwrap();
            std::fs::write(t.join("up.txt"), b"OLD").unwrap();
            ops.push(op(Action::Update, "up.txt"));
            std::fs::write(t.join("gone.txt"), b"bye").unwrap();
            ops.push(op(Action::Delete, "gone.txt"));

            let mut o = opts(base.join("trash"));
            o.parallel = parallel;
            let r = apply(&ops, &s, &t, &o);
            (base.clone(), t, r)
        };
        let (b1, t1, r1) = run("seq", 1);
        let (b2, t2, r2) = run("par", 4);
        assert_eq!(r1, r2, "counts must match");
        // The result trees agree file by file
        for e in std::fs::read_dir(&t1).unwrap().flatten() {
            let name = e.file_name();
            let a = std::fs::read(e.path()).unwrap();
            let b = std::fs::read(t2.join(&name)).unwrap();
            assert_eq!(a, b, "file {:?} differs between seq and par runs", name);
        }
        assert_eq!(
            std::fs::read_dir(&t1).unwrap().count(),
            std::fs::read_dir(&t2).unwrap().count()
        );
        let _ = std::fs::remove_dir_all(&b1);
        let _ = std::fs::remove_dir_all(&b2);
    }

    /// Errors don't abort: 10 good, 1 bad → 10 applied + 1 Error event, byte ledger = Σ of the successful files
    #[test]
    fn errors_accumulate_without_aborting() {
        let base = tmproot("errs");
        let (s, t) = (base.join("s"), base.join("t"));
        std::fs::create_dir_all(&s).unwrap();
        std::fs::create_dir_all(&t).unwrap();
        let mut ops = Vec::new();
        let mut expect_bytes = 0u64;
        for i in 0..10 {
            let name = format!("g{i}.bin");
            let body = vec![9u8; 5_000];
            std::fs::write(s.join(&name), &body).unwrap();
            expect_bytes += body.len() as u64;
            let mut o = op(Action::Copy, &name);
            o.size = Some(body.len() as u64);
            ops.push(o);
        }
        ops.push(op(Action::Copy, "missing-on-source.bin")); // guaranteed to fail

        let (ctx, store) = collecting_ctx();
        let out = apply_with(&ops, &s, &t, &opts(base.join("trash")), &ctx);
        assert_eq!(out.done, 10);
        assert_eq!(out.errors, 1);
        assert!(!out.cancelled);
        assert_eq!(out.bytes_copied, expect_bytes, "byte ledger must equal the sum of successful copies");
        let evs = store.lock().unwrap();
        assert_eq!(
            evs.iter().filter(|e| matches!(e, ProgressEvent::Error { .. })).count(),
            1,
            "exactly one Error event"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Cancel mid-copy of a big file: honoured between chunks, no half file at the destination, zero .syncdash.tmp debris
    #[test]
    fn cancel_mid_copy_leaves_no_debris() {
        let base = tmproot("cancel");
        let (s, t) = (base.join("s"), base.join("t"));
        std::fs::create_dir_all(&s).unwrap();
        std::fs::create_dir_all(&t).unwrap();
        std::fs::write(s.join("big.bin"), vec![3u8; 64 * 1024 * 1024]).unwrap();
        let mut o = op(Action::Copy, "big.bin");
        o.size = Some(64 * 1024 * 1024);

        let ctl = RunCtl::new();
        let ctl2 = ctl.clone();
        // Request the cancel right after the first Progress event (= the first 1MiB chunk)
        let sink = move |ev: ProgressEvent| {
            if matches!(ev, ProgressEvent::Progress { .. }) {
                ctl2.request_cancel();
            }
        };
        let ctx = RunCtx::new(ctl, Arc::new(sink));
        let out = apply_with(&[o], &s, &t, &opts(base.join("trash")), &ctx);
        assert!(out.cancelled);
        assert_eq!(out.done, 0);
        assert_eq!(out.errors, 0, "cancellation is not an error");
        assert!(!t.join("big.bin").exists(), "no partial file may reach the destination");
        let leftovers: Vec<_> = std::fs::read_dir(&t)
            .unwrap()
            .flatten()
            .filter(|e| crate::atomic::is_temp_name(&e.file_name().to_string_lossy()))
            .collect();
        assert!(leftovers.is_empty(), "temp files must be cleaned on cancel");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// DeleteDir depth ordering: subdirectories go before their parent, so the parent has a chance to be empty
    #[test]
    fn delete_dirs_deepest_first_regardless_of_input_order() {
        let base = tmproot("depth");
        let (s, t) = (base.join("s"), base.join("t"));
        std::fs::create_dir_all(&s).unwrap();
        std::fs::create_dir_all(t.join("a").join("b").join("c")).unwrap();
        // Deliberately hand over the ops in the wrong shallow→deep order
        let ops = vec![op(Action::DeleteDir, "a"), op(Action::DeleteDir, "a/b"), op(Action::DeleteDir, "a/b/c")];
        let (done, _, errors) = apply(&ops, &s, &t, &opts(base.join("trash")));
        assert_eq!((done, errors), (3, 0), "all three directories must go");
        assert!(!t.join("a").exists());
        let _ = std::fs::remove_dir_all(&base);
    }
}
