//! scan: walk a root and produce a snapshot table — the first of the three stages.
//!
//! Two lanes behind one entry point. `scan_root` asks the backend for `as_local()`: a real
//! directory takes `local`, the platform metadata walker + rayon fast path; anything else takes `vfs`,
//! the generic lane driven entirely through the `Vfs` trait. The split is which primitives are
//! available, not how far away the bytes are — an SMB root the OS has mounted takes `local`.
//!
//! Both speak the same filter contract and the same exclusion accounting, and both emit the same
//! events; the difference is only how bytes are reached.
//!
//! Content evidence is blake3, cached by `(path, size, mtime)` in `store::hashcache` so an
//! unchanged tree is not re-read. `digest` holds the sampled variant used by the fast tier.

pub mod digest;
pub mod local;
mod local_walk;
#[cfg(target_os = "macos")]
mod macos_bulk;
pub mod vfs;

use std::path::Path;

use crate::model::table::Snapshot;

use local::scan_impl;
use vfs::scan_vfs;

fn local_parallelism(root: &Path) -> usize {
    use crate::fs::vfs::Vfs;
    crate::fs::vfs::local::LocalVfs::new(root.to_path_buf())
        .caps()
        .max_parallel_streams
}

pub struct ScanOptions {
    pub hash: bool,
    /// Sampled evidence: files ≥4MB are not read whole but get a sampled digest (size + blake3 of 256KB at head/middle/tail,
    /// the value `~`-prefixed to keep it strictly apart from a full hash); <4MB is hashed in full. Cloud placeholders hydrate
    /// only those three windows. Not a byte-for-byte equality proof —— the escalation rule (same digest, different mtime → full rehash) backstops it.
    pub sampled: bool,
    /// Whether to trust the (path,size,mtime) cache. **The ladder's decisive axis**:
    /// fast/balanced = true (only the changed surface is really read; the unchanged surface is cache memory);
    /// standard/paranoid = false (every file is really read this run —— "identical ✓" is measured now, not remembered).
    pub use_cache: bool,
    /// symlinks="direct": record the link itself (its target string); otherwise symlinks are ignored
    pub symlinks_direct: bool,
    /// Filter with FFS semantics (see filter.rs); the default exclusions are built in
    pub filter: crate::pipeline::filter::PathFilter,
}

/// Legacy CLI scan progress. `complete` is the only 100% boundary; byte totals alone cannot express
/// cached or zero-byte files that are still moving through the worker queue.
#[derive(Clone, Copy, Debug)]
pub struct ScanProgress {
    /// "walk" (metadata traversal) | "hash" (parallel hashing)
    pub phase: &'static str,
    pub files_done: u64,
    pub files_total: u64,
    pub bytes_total: u64,
    pub bytes_done: u64,
    pub mib_per_s: f64,
    pub complete: bool,
}

pub type ProgressFn<'a> = &'a (dyn Fn(ScanProgress) + Sync);

#[derive(Default)]
pub(crate) struct ScanMetrics {
    pub cache_load_ms: u64,
    pub mtime_load_ms: u64,
    pub walk_ms: u64,
    pub hash_ms: u64,
    pub finalize_ms: u64,
    pub state_write_ms: u64,
    pub state_failures: u64,
    pub files: u64,
    pub cache_hits: u64,
    pub read_bytes: u64,
    pub workers: usize,
}

impl ScanMetrics {
    pub fn emit(&self, ctx: &crate::obs::progress::RunCtx, side: &str, lane: &str) {
        ctx.log(
            crate::model::event::LogLevel::Info,
            "scan",
            format!(
                "scan metrics: side={side} lane={lane} files={} cache_hits={} read_bytes={} workers={} cache_load_ms={} mtime_load_ms={} walk_ms={} hash_ms={} finalize_ms={} state_write_ms={} state_failures={}",
                self.files,
                self.cache_hits,
                self.read_bytes,
                self.workers,
                self.cache_load_ms,
                self.mtime_load_ms,
                self.walk_ms,
                self.hash_ms,
                self.finalize_ms,
                self.state_write_ms,
                self.state_failures,
            ),
        );
    }
}

pub fn scan(root: &Path, opt: &ScanOptions) -> std::io::Result<Snapshot> {
    scan_impl(root, opt, None, None, local_parallelism(root))
}

pub fn scan_with_progress(
    root: &Path,
    opt: &ScanOptions,
    progress: Option<ProgressFn<'_>>,
) -> std::io::Result<Snapshot> {
    scan_impl(root, opt, progress, None, local_parallelism(root))
}

/// v0.9 M1 unified-foundation entry point: cancel/pause/ProgressEvent event stream (see progress.rs).
/// The legacy ScanProgress callback and event-stream path share the same scan_impl.
pub fn scan_ctx(
    root: &Path,
    opt: &ScanOptions,
    ctx: &crate::obs::progress::RunCtx,
    phase: crate::model::event::Phase,
) -> std::io::Result<Snapshot> {
    scan_impl(root, opt, None, Some((ctx, phase)), local_parallelism(root))
}

/// Route a root to the right scan lane: a local (or locally-translated) root keeps the
/// platform-native local fast path; everything else runs the generic VFS
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
            let mut snap = scan_impl(
                root,
                opt,
                None,
                Some((ctx, phase)),
                vfs.caps().max_parallel_streams,
            )?;
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
    use crate::fs::vfs::{memory::MemVfs, Vfs};
    use crate::model::event::{Phase, ProgressEvent};
    use crate::model::table::EntryKind;
    use crate::obs::progress::{is_cancelled, RunCtl, RunCtx};
    use std::sync::atomic::Ordering;
    use std::sync::{Arc, Mutex};

    fn mk_tree(tag: &str, n: usize) -> std::path::PathBuf {
        let root =
            std::env::temp_dir().join(format!("syncdash-scanctx-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sub")).unwrap();
        for i in 0..n {
            std::fs::write(
                root.join("sub").join(format!("f{i}.dat")),
                vec![i as u8; 100],
            )
            .unwrap();
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

    fn saw_intermediate_bytes(events: &[ProgressEvent], total: u64) -> bool {
        events.iter().any(|event| {
            matches!(
                event,
                ProgressEvent::Progress {
                    bytes_done,
                    bytes_total,
                    ..
                } if *bytes_total == total && *bytes_done > 0 && *bytes_done < total
            )
        })
    }

    #[test]
    fn scan_ctx_reports_exact_totals() {
        let root = mk_tree("totals", 20);
        let store: Arc<Mutex<Vec<ProgressEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let s2 = store.clone();
        let ctx = RunCtx::new(
            RunCtl::new(),
            Arc::new(move |ev| s2.lock().unwrap().push(ev)),
        );
        let snap = scan_ctx(&root, &opts(), &ctx, Phase::ScanSource).unwrap();
        assert_eq!(
            snap.entries
                .iter()
                .filter(|e| e.kind == EntryKind::File)
                .count(),
            20
        );
        let evs = store.lock().unwrap();
        let totals = evs.iter().find_map(|e| match e {
            ProgressEvent::Totals {
                reset: true,
                items_total,
                bytes_total,
                ..
            } => Some((*items_total, *bytes_total)),
            _ => None,
        });
        assert_eq!(
            totals,
            Some((20, 2000)),
            "the end of the walk must yield exact totals"
        );
        assert!(
            matches!(
                evs.last(),
                Some(ProgressEvent::PhaseEnd {
                    phase: Phase::ScanSource,
                    status: crate::model::event::PhaseStatus::Completed,
                    items_done: 20,
                    items_total: 20,
                    bytes_done: 2000,
                    bytes_total: 2000,
                    ..
                })
            ),
            "a successful scan must end explicitly with its final counters"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn local_large_file_reports_intermediate_hash_bytes() {
        const SIZE: usize = 17 * 1024 * 1024;
        let root = std::env::temp_dir().join(format!(
            "syncdash-local-chunk-progress-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("large.bin"), vec![7u8; SIZE]).unwrap();

        let events: Arc<Mutex<Vec<ProgressEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let copy = events.clone();
        let ctx = RunCtx::new(
            RunCtl::new(),
            Arc::new(move |event| {
                if matches!(&event, ProgressEvent::Totals { reset: true, .. }) {
                    // The discovery event may have just consumed the progress throttle window.
                    // Make the first hash chunk observable without relying on disk speed.
                    std::thread::sleep(std::time::Duration::from_millis(110));
                }
                copy.lock().unwrap().push(event);
            }),
        );

        scan_ctx(&root, &opts(), &ctx, Phase::ScanSource).unwrap();
        assert!(
            saw_intermediate_bytes(&events.lock().unwrap(), SIZE as u64),
            "a multi-chunk local read must advance bytes before the file completes",
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn failed_local_open_does_not_credit_the_planned_file_size() {
        const SIZE: usize = 4096;
        let root = std::env::temp_dir().join(format!(
            "syncdash-local-failed-read-progress-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("vanishes.bin");
        std::fs::write(&file, vec![3u8; SIZE]).unwrap();

        let events: Arc<Mutex<Vec<ProgressEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let copy = events.clone();
        let ctx = RunCtx::new(
            RunCtl::new(),
            Arc::new(move |event| {
                if matches!(&event, ProgressEvent::Totals { reset: true, .. }) {
                    let _ = std::fs::remove_file(&file);
                }
                copy.lock().unwrap().push(event);
            }),
        );

        let snapshot = scan_ctx(&root, &opts(), &ctx, Phase::ScanSource).unwrap();
        assert!(snapshot.entries.iter().any(|entry| entry.hash_failed));
        assert!(matches!(
            events.lock().unwrap().last(),
            Some(ProgressEvent::PhaseEnd {
                bytes_done: 0,
                bytes_total,
                ..
            }) if *bytes_total == SIZE as u64
        ));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn vfs_large_file_reports_intermediate_hash_bytes() {
        const SIZE: usize = 200_000;
        let memory = Arc::new(MemVfs::new("scan-vfs-chunk-progress"));
        memory.seed_file("large.bin", SIZE, 1_700_000_000_000);
        let vfs: Arc<dyn Vfs> = memory;
        let events: Arc<Mutex<Vec<ProgressEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let copy = events.clone();
        let ctx = RunCtx::new(
            RunCtl::new(),
            Arc::new(move |event| {
                if matches!(&event, ProgressEvent::Totals { reset: true, .. }) {
                    std::thread::sleep(std::time::Duration::from_millis(110));
                }
                copy.lock().unwrap().push(event);
            }),
        );

        scan_root(&vfs, &opts(), &ctx, Phase::ScanSource).unwrap();
        assert!(
            saw_intermediate_bytes(&events.lock().unwrap(), SIZE as u64),
            "a multi-chunk VFS read must advance bytes before the file completes",
        );
    }

    #[test]
    fn failed_vfs_open_does_not_credit_the_planned_file_size() {
        const SIZE: usize = 4096;
        let memory = Arc::new(MemVfs::new("scan-vfs-failed-read-progress"));
        memory.seed_file("vanishes.bin", SIZE, 1_700_000_000_000);
        let remover = memory.clone();
        let vfs: Arc<dyn Vfs> = memory;
        let events: Arc<Mutex<Vec<ProgressEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let copy = events.clone();
        let ctx = RunCtx::new(
            RunCtl::new(),
            Arc::new(move |event| {
                if matches!(&event, ProgressEvent::Totals { reset: true, .. }) {
                    remover.remove_file("vanishes.bin").unwrap();
                }
                copy.lock().unwrap().push(event);
            }),
        );

        let snapshot = scan_root(&vfs, &opts(), &ctx, Phase::ScanSource).unwrap();
        assert!(snapshot.entries.iter().any(|entry| entry.hash_failed));
        assert!(matches!(
            events.lock().unwrap().last(),
            Some(ProgressEvent::PhaseEnd {
                bytes_done: 0,
                bytes_total,
                ..
            }) if *bytes_total == SIZE as u64
        ));
    }

    #[test]
    fn filtered_no_cache_vfs_scan_retains_hashes_outside_its_domain() {
        let memory = Arc::new(MemVfs::new(&format!(
            "filtered-no-cache-vfs-{}",
            std::process::id()
        )));
        memory.seed_bytes("included.bin", b"old!", 1_700_000_000_000);
        memory.seed_bytes("excluded/keep.bin", b"outside", 1_700_000_000_000);
        let vfs: Arc<dyn Vfs> = memory.clone();

        let first = scan_root(&vfs, &opts(), &RunCtx::null(), Phase::ScanSource).unwrap();
        let old_hash = first
            .entries
            .iter()
            .find(|entry| entry.path == "included.bin")
            .and_then(|entry| entry.hash.clone())
            .unwrap();

        memory.seed_bytes("included.bin", b"new!", 1_700_000_000_000);
        let mut filtered = opts();
        filtered.filter = crate::pipeline::filter::PathFilter::build(&[], &["/excluded/".into()]);
        let second = scan_root(&vfs, &filtered, &RunCtx::null(), Phase::ScanSource).unwrap();
        let new_hash = second
            .entries
            .iter()
            .find(|entry| entry.path == "included.bin")
            .and_then(|entry| entry.hash.as_deref())
            .unwrap();
        assert_ne!(new_hash, old_hash);

        let cache = crate::store::hashcache::load_by_key(&vfs.identity());
        assert!(cache.contains_key("excluded/keep.bin"));
    }

    #[test]
    fn no_hash_scan_ends_with_all_discovered_items_done() {
        let root = mk_tree("no-hash-progress", 7);
        let store: Arc<Mutex<Vec<ProgressEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let s2 = store.clone();
        let ctx = RunCtx::new(
            RunCtl::new(),
            Arc::new(move |ev| s2.lock().unwrap().push(ev)),
        );
        let mut opt = opts();
        opt.hash = false;
        scan_ctx(&root, &opt, &ctx, Phase::ScanSource).unwrap();
        let evs = store.lock().unwrap();
        assert!(evs.iter().any(|ev| matches!(
            ev,
            ProgressEvent::Totals {
                reset: false,
                items_done: 7,
                items_total: 7,
                ..
            }
        )));
        assert!(matches!(
            evs.last(),
            Some(ProgressEvent::PhaseEnd {
                status: crate::model::event::PhaseStatus::Completed,
                items_done: 7,
                items_total: 7,
                bytes_done: 0,
                bytes_total: 0,
                ..
            })
        ));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn no_hash_cancellation_at_final_totals_does_not_return_a_snapshot() {
        let root = mk_tree("no-hash-cancel-boundary", 7);
        let events: Arc<Mutex<Vec<ProgressEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let copy = events.clone();
        let ctl = RunCtl::new();
        let cancel = ctl.clone();
        let ctx = RunCtx::new(
            ctl,
            Arc::new(move |ev| {
                if matches!(
                    &ev,
                    ProgressEvent::Totals {
                        phase: Phase::ScanSource,
                        ..
                    }
                ) {
                    cancel.request_cancel();
                }
                copy.lock().unwrap().push(ev);
            }),
        );
        let mut opt = opts();
        opt.hash = false;

        match scan_ctx(&root, &opt, &ctx, Phase::ScanSource) {
            Err(e) => assert!(is_cancelled(&e)),
            Ok(_) => panic!("a cancelled scan must not return a snapshot"),
        }
        assert!(matches!(
            events.lock().unwrap().last(),
            Some(ProgressEvent::PhaseEnd {
                phase: Phase::ScanSource,
                status: crate::model::event::PhaseStatus::Cancelled,
                ..
            })
        ));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The reason `scan_impl` aborts instead of counting: walkdir emits a directory before it
    /// descends, so a directory it cannot read is already in the table with zero children — and
    /// compare cannot tell "no children" from "children deleted". Under mirror that is a delete
    /// for every file the other side still holds. On macOS the everyday cause is TCC, not a chmod:
    /// ~/Desktop, ~/Documents and every external volume are gated by default.
    #[cfg(unix)]
    #[test]
    fn an_unreadable_subdirectory_aborts_the_scan_rather_than_reading_as_empty() {
        use std::os::unix::fs::PermissionsExt;
        let root = mk_tree("denied", 3);
        let locked = root.join("locked");
        std::fs::create_dir_all(&locked).unwrap();
        std::fs::write(locked.join("secret.txt"), b"x").unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();

        // root ignores the mode bits, so the precondition would be silently absent there. Say so
        // rather than reporting a pass that proved nothing.
        let denied = std::fs::read_dir(&locked).is_err();
        if denied {
            match scan(&root, &opts()) {
                Ok(s) => panic!(
                    "an unreadable subtree scanned clean with {} entries — that table says the tree is empty",
                    s.entries.len()
                ),
                Err(e) => {
                    let msg = e.to_string();
                    assert!(msg.contains("refusing to emit a half table"), "{msg}");
                    assert!(msg.contains("locked"), "the refusal must name what it could not read: {msg}");
                }
            }
        }

        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        let _ = std::fs::remove_dir_all(&root);
        assert!(
            denied,
            "precondition absent: this process can read a 0o000 directory (running as root?)"
        );
    }

    /// The other half of the same rule: a *vanished* entry is a race, not an unreadable tree, so it
    /// stays non-fatal — but it must still reach the header, because the guards judge the plan long
    /// after the snapshot is gone.
    #[test]
    fn a_clean_scan_reports_no_walk_errors() {
        let root = mk_tree("clean", 4);
        let snap = scan(&root, &opts()).unwrap();
        assert_eq!(snap.header.walk_errors, 0);
        assert!(snap.header.walk_err_samples.is_empty());
        assert_eq!(snap.header.icloud_stubs, 0);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// An evicted iCloud file leaves a `.<name>.icloud` plist where the real name was. Nothing in
    /// the entry itself says "placeholder" — it is a small real file — so without this the snapshot
    /// records the stub and records the original as absent, and mirror deletes the real copy on the
    /// other side. Name-shaped, hence testable off macOS too.
    #[test]
    fn an_icloud_placeholder_is_counted_and_sampled() {
        let root = mk_tree("icloud", 1);
        std::fs::write(root.join("sub").join(".Report.pdf.icloud"), b"<plist/>").unwrap();
        // A dotfile that merely ends in .icloud-ish text must not trip it
        std::fs::write(root.join("sub").join("notes.icloud.txt"), b"x").unwrap();
        let snap = scan(&root, &opts()).unwrap();
        assert_eq!(
            snap.header.icloud_stubs, 1,
            "exactly the placeholder, not its neighbour"
        );
        assert!(
            snap.header
                .icloud_stub_samples
                .iter()
                .any(|s| s.ends_with(".Report.pdf.icloud")),
            "the sample must name the placeholder: {:?}",
            snap.header.icloud_stub_samples
        );
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
