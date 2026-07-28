//! scan: walk a root and produce a snapshot table — the first of the three stages.
//!
//! Two lanes behind one entry point. `scan_root` asks the backend for `as_local()`: a real
//! directory takes `local`, the walkdir + rayon + mmap fast path; anything else takes `remote`,
//! the generic lane driven entirely through the `Vfs` trait. Both speak the same filter contract
//! and the same exclusion accounting, and both emit the same events — the difference is only how
//! bytes are reached.
//!
//! Content evidence is blake3, cached by `(path, size, mtime)` in `store::hashcache` so an
//! unchanged tree is not re-read. `digest` holds the sampled variant used by the fast tier.

pub mod digest;
pub mod local;
pub mod remote;

use std::path::Path;

use crate::model::table::Snapshot;

use local::scan_impl;
use remote::scan_vfs;

pub struct ScanOptions {
    pub hash: bool,
    /// Sampled evidence: files ≥4MB are not read whole but get a sampled digest (size + blake3 of 256KB at head/middle/tail,
    /// the value `~`-prefixed to keep it strictly apart from a full hash); <4MB is hashed in full. Cloud placeholders hydrate
    /// only those three windows. Not a byte-for-byte equality proof —— the escalation rule (same digest, different mtime → full rehash) backstops it.
    pub sampled: bool,
    /// Whether to trust the (path,size,mtime) cache. **The ladder's decisive axis**:
    /// fast = true (only the changed surface is really read; the unchanged surface is cache memory);
    /// standard/paranoid = false (every file is really read this run —— "identical ✓" is measured now, not remembered).
    pub use_cache: bool,
    /// symlinks="direct": record the link itself (its target string); otherwise symlinks are ignored
    pub symlinks_direct: bool,
    /// Filter with FFS semantics (see filter.rs); the default exclusions are built in
    pub filter: crate::pipeline::filter::PathFilter,
}
/// Scan progress (P2-6). The same amount of information as syncthing's `FolderScanProgress`:
/// phase + bytes done/total + rate, enough for the frontend to draw a bar and estimate the time remaining.
#[derive(Clone, Copy, Debug)]
pub struct ScanProgress {
    /// "walk" (metadata traversal) | "hash" (parallel hashing)
    pub phase: &'static str,
    pub files_total: u64,
    pub bytes_total: u64,
    pub bytes_done: u64,
    pub mib_per_s: f64,
}
pub type ProgressFn<'a> = &'a (dyn Fn(ScanProgress) + Sync);
pub fn scan(root: &Path, opt: &ScanOptions) -> std::io::Result<Snapshot> {
    scan_impl(root, opt, None, None)
}
pub fn scan_with_progress(
    root: &Path,
    opt: &ScanOptions,
    progress: Option<ProgressFn<'_>>,
) -> std::io::Result<Snapshot> {
    scan_impl(root, opt, progress, None)
}
/// v0.9 M1 unified-foundation entry point: cancel/pause/ProgressEvent event stream (see progress.rs).
/// The old ScanProgress callback shape (P2-6) is kept as-is —— both paths share the same scan_impl.
pub fn scan_ctx(
    root: &Path,
    opt: &ScanOptions,
    ctx: &crate::obs::progress::RunCtx,
    phase: crate::model::event::Phase,
) -> std::io::Result<Snapshot> {
    scan_impl(root, opt, None, Some((ctx, phase)))
}
/// Route a root to the right scan lane: a local (or locally-translated) root keeps the
/// existing walkdir+mmap fast path byte-for-byte; everything else runs the generic VFS
/// lane. Both lanes speak the same filter contract and the same exclusion accounting —
/// the differential tests pin those numbers against each other.
pub fn scan_root(
    vfs: &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
    opt: &ScanOptions,
    ctx: &crate::obs::progress::RunCtx,
    phase: crate::model::event::Phase,
) -> std::io::Result<Snapshot> {
    match vfs.as_local() {
        Some(root) => {
            let mut snap = scan_impl(root, opt, None, Some((ctx, phase)))?;
            // The local lane used to leave this empty, which meant a table could not say whether
            // its root was a disk on this machine or a share on another — the very fact that
            // decides where its deletions were preserved.
            snap.header.vfs = Some(vfs_note(vfs.as_ref(), opt, opt.sampled));
            Ok(snap)
        }
        None => scan_vfs(vfs, opt, ctx, phase),
    }
}

/// A snapshot's self-description, written by both lanes so they cannot drift.
///
/// `sampled` is passed rather than read off `opt` because the generic lane may have been forced
/// down a tier by a backend that cannot do ranged reads; the note has to record what ran, not
/// what was asked for.
pub(crate) fn vfs_note(
    vfs: &dyn crate::fs::vfs::Vfs,
    opt: &ScanOptions,
    sampled: bool,
) -> crate::model::table::VfsNote {
    let caps = vfs.caps();
    crate::model::table::VfsNote {
        protocol: caps.protocol.to_string(),
        display_root: vfs.display(),
        mtime_precision_ms: caps.mtime_precision_ms,
        medium: caps.medium.as_str().to_string(),
        evidence_effective: if !opt.hash {
            "none".into()
        } else if sampled {
            "sampled".into()
        } else {
            "full".into()
        },
        name_rules: caps.name_rules.as_str().into(),
        degraded: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::event::{Phase, ProgressEvent};
    use crate::obs::progress::{is_cancelled, RunCtl, RunCtx};
    use crate::model::table::EntryKind;
    use std::sync::atomic::Ordering;
    use std::sync::{Arc, Mutex};

    fn mk_tree(tag: &str, n: usize) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("syncdash-scanctx-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sub")).unwrap();
        for i in 0..n {
            std::fs::write(root.join("sub").join(format!("f{i}.dat")), vec![i as u8; 100]).unwrap();
        }
        root
    }

    fn opts() -> ScanOptions {
        ScanOptions {
            hash: true,
            sampled: false,
            use_cache: false, // tests never eat the cache
            symlinks_direct: false,
            filter: crate::pipeline::filter::PathFilter::build(&[], &[]),
        }
    }

    #[test]
    fn scan_ctx_reports_exact_totals() {
        let root = mk_tree("totals", 20);
        let store: Arc<Mutex<Vec<ProgressEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let s2 = store.clone();
        let ctx = RunCtx::new(RunCtl::new(), Arc::new(move |ev| s2.lock().unwrap().push(ev)));
        let snap = scan_ctx(&root, &opts(), &ctx, Phase::ScanSource).unwrap();
        assert_eq!(snap.entries.iter().filter(|e| e.kind == EntryKind::File).count(), 20);
        let evs = store.lock().unwrap();
        let totals = evs.iter().find_map(|e| match e {
            ProgressEvent::Totals { items_total, bytes_total, .. } => Some((*items_total, *bytes_total)),
            _ => None,
        });
        assert_eq!(totals, Some((20, 2000)), "the end of the walk must yield exact totals");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn scan_ctx_cancels_midway() {
        let root = mk_tree("cancel", 50);
        let ctl = RunCtl::new();
        let ctl2 = ctl.clone();
        let sink = move |ev: ProgressEvent| {
            if matches!(ev, ProgressEvent::Progress { .. }) {
                ctl2.cancel.store(true, Ordering::SeqCst); // call a halt on the very first progress event
            }
        };
        let ctx = RunCtx::new(ctl, Arc::new(sink));
        match scan_ctx(&root, &opts(), &ctx, Phase::ScanSource) {
            Err(e) => assert!(is_cancelled(&e)),
            Ok(_) => panic!("expected cancellation"),
        }
        let _ = std::fs::remove_dir_all(&root);
    }
}
