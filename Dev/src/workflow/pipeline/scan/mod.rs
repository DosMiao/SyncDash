//! scan: walk a root and produce a snapshot table — the first of the three stages.
//!
//! Two lanes sit behind one entry point. A backend with a retained `LocalRoot` takes the platform
//! metadata walker and rayon fast path; anything else takes the generic VFS lane. The split is
//! which primitives are available, not how far away the bytes are: an OS-mounted SMB root is local.
//!
//! Both speak the same filter contract and the same exclusion accounting, and both emit the same
//! events; the difference is only how bytes are reached.
//!
//! Content evidence is blake3, cached by `(path, size, mtime)` in `store::hashcache` so an
//! unchanged tree is not re-read. `digest` holds the sampled variant used by the fast tier.
//!
//! What the lanes may not each decide for themselves lives here rather than in either of them:
//! `model` owns the row under construction and the evidence label it becomes, `state` owns how the
//! acceleration tables are read, and `evidence_tier` owns the header's tier.

pub mod digest;
pub mod local;
mod model;
mod state;
pub mod vfs;

use std::path::Path;

use crate::model::table::{TableArtifact, TableEvidence};

use local::scan_local_root_impl;
use vfs::scan_vfs;

/// The evidence tier a scan actually ran at, from the two facts that decide it.
///
/// One mapping, three readers: both lanes stamp it into `TableHeader.evidence`, and
/// `run::evidence_label` renders the same decision for the archive check and for the peer lane's
/// `--evidence` argument. `sampled` is passed rather than read off `ScanOptions` because the
/// generic lane may have been forced down a tier by a backend that cannot do ranged reads; the
/// header has to record what ran, not what was asked for.
pub fn evidence_tier(hash: bool, sampled: bool) -> TableEvidence {
    if !hash {
        TableEvidence::None
    } else if sampled {
        TableEvidence::Sampled
    } else {
        TableEvidence::Full
    }
}

fn scan_local_path(
    root_path: &Path,
    options: &ScanOptions,
    progress: Option<ProgressFn<'_>>,
    context: Option<(&crate::obs::progress::RunCtx, crate::model::event::Phase)>,
) -> std::io::Result<TableArtifact> {
    use crate::fs::vfs::Vfs;
    let local = crate::fs::vfs::local::LocalVfs::open(root_path.to_path_buf())
        .map_err(|error| root_unreadable_error(root_path, std::io::Error::from(error)))?;
    scan_local_root_impl(
        local.local_root(),
        options,
        progress,
        context,
        local.caps().max_parallel_streams,
    )
}

pub struct ScanOptions {
    pub hash: bool,
    /// Sampled evidence: files ≥4MB are not read whole but get a sampled digest (size + blake3 of 256KB at head/middle/tail);
    /// its typed observation keeps it strictly apart from a full hash. Files <4MB are hashed in full. Cloud placeholders hydrate
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

pub fn scan(root: &Path, opt: &ScanOptions) -> std::io::Result<TableArtifact> {
    scan_local_path(root, opt, None, None)
}

pub fn scan_with_progress(
    root: &Path,
    opt: &ScanOptions,
    progress: Option<ProgressFn<'_>>,
) -> std::io::Result<TableArtifact> {
    scan_local_path(root, opt, progress, None)
}

/// Scan with cooperative run control and progress events.
pub fn scan_ctx(
    root: &Path,
    opt: &ScanOptions,
    ctx: &crate::obs::progress::RunCtx,
    phase: crate::model::event::Phase,
) -> std::io::Result<TableArtifact> {
    scan_local_path(root, opt, None, Some((ctx, phase)))
}

/// Route a root to the right scan lane: a local (or locally-translated) root keeps the
/// platform-native local fast path; everything else runs the generic VFS lane. Both lanes speak
/// the same filter contract and the same exclusion accounting —
/// `both_lanes_produce_the_same_table_for_the_same_tree` scans one tree through both and compares
/// the tables entry for entry.
pub fn scan_root(
    vfs: &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
    opt: &ScanOptions,
    ctx: &crate::obs::progress::RunCtx,
    phase: crate::model::event::Phase,
) -> std::io::Result<TableArtifact> {
    match vfs.local_root() {
        Some(root) => {
            let mut snap = scan_local_root_impl(
                root,
                opt,
                None,
                Some((ctx, phase)),
                vfs.caps().max_parallel_streams,
            )?;
            // The local lane used to leave this empty, which meant a table could not say whether
            // its root was a disk on this machine or a share on another — the very fact that
            // decides where its deletions were preserved.
            snap.header.vfs = Some(vfs_note(vfs.as_ref()));
            Ok(snap)
        }
        None => scan_vfs(vfs, opt, ctx, phase),
    }
}

/// The local lane's refusal when the root itself cannot be read.
///
/// It is one sentence in one place because the alternative — returning the bare I/O error — lets an
/// unreachable root arrive at compare as an empty table, and an empty table is a mass deletion. The
/// caller's error `kind()` is preserved so a retry can still classify the failure. The generic lane
/// refuses on the same grounds through `vfs::discovery`, which cannot separate the root from any
/// other unreadable directory and so reports both with its half-table sentence.
pub(crate) fn root_unreadable_error(root: &Path, error: std::io::Error) -> std::io::Error {
    std::io::Error::new(
        error.kind(),
        format!(
            "scan of '{}' could not read the root itself: {error} — refusing to report it as an empty tree (that reads as a mass deletion on the other side)",
            root.display()
        ),
    )
}

/// A snapshot's self-description, written by both lanes so they cannot drift.
///
/// `sampled` is passed rather than read off `opt` because the generic lane may have been forced
/// down a tier by a backend that cannot do ranged reads; the note has to record what ran, not
/// what was asked for.
pub(crate) fn vfs_note(vfs: &dyn crate::fs::vfs::Vfs) -> crate::model::table::VfsNote {
    let caps = vfs.caps();
    crate::model::table::VfsNote {
        protocol: caps.protocol.to_string(),
        display_root: vfs.display(),
        mtime_precision_ms: caps.mtime_precision_ms,
        medium: match caps.medium {
            crate::fs::vfs::Medium::FixedDisk => crate::model::table::ObservedMedium::FixedDisk,
            crate::fs::vfs::Medium::RemovableDisk => {
                crate::model::table::ObservedMedium::RemovableDisk
            }
            crate::fs::vfs::Medium::NetworkShare => {
                crate::model::table::ObservedMedium::NetworkShare
            }
            crate::fs::vfs::Medium::Unknown => crate::model::table::ObservedMedium::Unknown,
        },
        name_rules: match caps.name_rules {
            crate::fs::vfs::NameRules::Windows => crate::model::table::ObservedNameRules::Windows,
            crate::fs::vfs::NameRules::Posix => crate::model::table::ObservedNameRules::Posix,
            crate::fs::vfs::NameRules::Unknown => crate::model::table::ObservedNameRules::Unknown,
        },
        degraded: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::vfs::{memory::MemVfs, Vfs};
    use crate::model::event::{Phase, ProgressEvent};
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

    /// The base timestamp both fixtures stamp on their files, so a mtime difference between the
    /// two tables can only come from the lanes rather than from when the test happened to run.
    const DIFFERENTIAL_MTIME_MS: i64 = 1_700_000_000_000;

    /// One tree, described once, materialized into a real directory and into a `MemVfs`.
    ///
    /// Sizes and byte content are generated by the same `filler` both backends use, so a file is
    /// literally the same object on both sides and must hash identically.
    fn differential_files() -> Vec<(&'static str, Vec<u8>, i64)> {
        vec![
            ("empty.dat", Vec::new(), DIFFERENTIAL_MTIME_MS),
            (
                "large.bin",
                crate::fs::vfs::memory::filler(
                    crate::pipeline::scan::digest::SAMPLE_MIN + 1024,
                    (crate::pipeline::scan::digest::SAMPLE_MIN + 1024) as usize,
                ),
                DIFFERENTIAL_MTIME_MS + 1_000,
            ),
            (
                "notes.bak",
                b"excluded by mask".to_vec(),
                DIFFERENTIAL_MTIME_MS + 2_000,
            ),
            (
                "skip/inside.dat",
                b"inside a pruned subtree".to_vec(),
                DIFFERENTIAL_MTIME_MS + 3_000,
            ),
            (
                "sub/deep/leaf.dat",
                b"leaf contents".to_vec(),
                DIFFERENTIAL_MTIME_MS + 4_000,
            ),
        ]
    }

    /// Symlinks join the differential fixture only where creating one needs no privilege.
    #[cfg(unix)]
    const DIFFERENTIAL_LINK: (&str, &str) = ("link.rel", "sub/deep/leaf.dat");

    fn differential_local_root(tag: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "syncdash-differential-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        for (relative, bytes, mtime_ms) in differential_files() {
            let path = root.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &bytes).unwrap();
            filetime::set_file_mtime(
                &path,
                filetime::FileTime::from_unix_time(mtime_ms / 1_000, 0),
            )
            .unwrap();
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(DIFFERENTIAL_LINK.1, root.join(DIFFERENTIAL_LINK.0)).unwrap();
        root
    }

    fn differential_memory_root(tag: &str) -> Arc<MemVfs> {
        let memory = Arc::new(MemVfs::new(&format!(
            "differential-{tag}-{}",
            std::process::id()
        )));
        for (relative, bytes, mtime_ms) in differential_files() {
            memory.seed_bytes(relative, &bytes, mtime_ms);
        }
        #[cfg(unix)]
        memory
            .make_symlink(DIFFERENTIAL_LINK.0, DIFFERENTIAL_LINK.1)
            .unwrap();
        memory
    }

    /// Everything about an entry that both backends can actually report.
    ///
    /// Three fields are deliberately absent, and none of them is a lane decision:
    /// `file_system_id` (`MemVfs` declares `file_id: Support::No` while `LocalVfs` reports
    /// `dev:ino` on unix), `mode` (`MemVfs::seed_bytes` stores `None`), and the timestamps of
    /// directories and symlinks (`MemVfs` stamps every one of them `0`). Widening this projection
    /// without first teaching the fixture to align those fields would only prove that a test
    /// double is not a filesystem.
    fn comparable_rows(table: &TableArtifact) -> Vec<String> {
        use crate::model::table::{FileIdentityObservation, ObservedEntry};
        table
            .entries
            .iter()
            .map(|entry| match entry {
                ObservedEntry::Directory(directory) => format!("dir {}", directory.path.as_str()),
                ObservedEntry::Symlink(link) => {
                    format!("link {} -> {}", link.path.as_str(), link.target)
                }
                ObservedEntry::File(file) => {
                    let identity = match &file.identity {
                        FileIdentityObservation::SizeAndMtime => "size-and-mtime".to_owned(),
                        FileIdentityObservation::Unreadable => "unreadable".to_owned(),
                        FileIdentityObservation::SampledBlake3 { digest } => {
                            format!("sampled:{digest}")
                        }
                        FileIdentityObservation::FullBlake3 { digest } => format!("full:{digest}"),
                    };
                    format!(
                        "file {} size={} mtime={} identity={identity}",
                        file.path.as_str(),
                        file.size,
                        file.mtime_ms,
                    )
                }
            })
            .collect()
    }

    /// The four header counters the UI reports, plus the evidence tier that decides how a
    /// difference is judged.
    fn comparable_header(table: &TableArtifact) -> (u64, u64, u64, u64, u64, TableEvidence) {
        (
            table.header.entry_count,
            table.header.excluded_dirs,
            table.header.excluded_files,
            table.header.walk_errors,
            table.header.skipped_symlinks,
            table.header.evidence,
        )
    }

    /// The claim `scan_root` makes in its own doc comment: both lanes speak the same filter
    /// contract and the same exclusion accounting. Nothing pinned it until now, which is what made
    /// every later change to either lane an unverifiable one.
    #[test]
    fn both_lanes_produce_the_same_table_for_the_same_tree() {
        let root = differential_local_root("table");
        let memory = differential_memory_root("table");
        let vfs: Arc<dyn Vfs> = memory;
        let expected_links = if cfg!(unix) { 1 } else { 0 };

        let configurations: [(&str, bool, bool, bool); 3] = [
            ("full hash, symlinks recorded", true, false, true),
            ("sampled hash, symlinks recorded", true, true, true),
            ("no hash, symlinks skipped", false, false, false),
        ];
        for (label, hash, sampled, symlinks_direct) in configurations {
            let options = ScanOptions {
                hash,
                sampled,
                use_cache: false,
                symlinks_direct,
                filter: crate::pipeline::filter::PathFilter::build(
                    &[],
                    &["/skip/".into(), "*.bak".into()],
                ),
            };
            let local = scan_ctx(&root, &options, &RunCtx::null(), Phase::ScanSource).unwrap();
            let generic = scan_root(&vfs, &options, &RunCtx::null(), Phase::ScanSource).unwrap();

            assert_eq!(
                comparable_rows(&local),
                comparable_rows(&generic),
                "{label}: the two lanes disagree about the same tree"
            );
            assert_eq!(
                comparable_header(&local),
                comparable_header(&generic),
                "{label}: the two lanes disagree about the header the UI reports"
            );

            // Both lanes agreeing on a wrong answer would still pass the comparison above.
            assert_eq!(
                comparable_header(&local),
                (
                    // empty.dat, large.bin, sub/deep/leaf.dat, sub, sub/deep — and the symlink
                    // only when the run was asked to record it.
                    5 + if symlinks_direct { expected_links } else { 0 },
                    1, // the whole skip/ subtree, pruned without being listed
                    1, // notes.bak, matched by mask
                    0,
                    if symlinks_direct { 0 } else { expected_links },
                    evidence_tier(hash, sampled),
                ),
                "{label}: the expected accounting for this fixture"
            );
            assert!(
                comparable_rows(&local)
                    .iter()
                    .all(|row| !row.contains("skip/") && !row.contains("notes.bak")),
                "{label}: an excluded path reached the table"
            );
        }

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn scan_ctx_reports_exact_totals() {
        let root = mk_tree("totals", 20);
        let (ctx, store) = RunCtx::collecting();
        let snap = scan_ctx(&root, &opts(), &ctx, Phase::ScanSource).unwrap();
        assert_eq!(
            snap.entries
                .iter()
                .filter(|entry| entry.as_file().is_some())
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
        const SIZE: usize = 9 * 1024 * 1024;
        let root = std::env::temp_dir().join(format!(
            "syncdash-local-chunk-progress-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("large.bin"), vec![7u8; SIZE]).unwrap();

        let (ctx, events) = RunCtx::collecting();

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
        assert!(snapshot.entries.iter().any(|entry| {
            entry
                .as_file()
                .is_some_and(|file| file.identity.is_unreadable())
        }));
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
        let (ctx, events) = RunCtx::collecting();

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
        assert!(snapshot.entries.iter().any(|entry| {
            entry
                .as_file()
                .is_some_and(|file| file.identity.is_unreadable())
        }));
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
            .find(|entry| entry.path().as_str() == "included.bin")
            .and_then(|entry| entry.as_file())
            .and_then(|file| file.identity.digest().cloned())
            .unwrap();

        memory.seed_bytes("included.bin", b"new!", 1_700_000_000_000);
        let mut filtered = opts();
        filtered.filter = crate::pipeline::filter::PathFilter::build(&[], &["/excluded/".into()]);
        let second = scan_root(&vfs, &filtered, &RunCtx::null(), Phase::ScanSource).unwrap();
        let new_hash = second
            .entries
            .iter()
            .find(|entry| entry.path().as_str() == "included.bin")
            .and_then(|entry| entry.as_file())
            .and_then(|file| file.identity.digest())
            .unwrap();
        assert_ne!(new_hash, &old_hash);

        let cache = crate::store::hashcache::load_by_key(&vfs.identity());
        assert!(cache.contains_key("excluded/keep.bin"));
    }

    /// The generic lane's twin of `corrected_mtime_never_reuses_a_hash_for_a_replacement_object`.
    ///
    /// A corrected mtime is the one timestamp the cache key cannot be trusted against: the table
    /// reports the intended value, so a replacement object that kept the raw on-disk value would
    /// present exactly the `(path, size, corrected mtime)` triple the previous object cached. The
    /// correction match therefore has to disqualify the cache row, not just rewrite the timestamp.
    #[test]
    fn vfs_corrected_mtime_never_reuses_a_hash_for_a_replacement_object() {
        let memory = Arc::new(MemVfs::new(&format!(
            "vfs-corrected-cache-replacement-{}",
            std::process::id()
        )));
        let raw_ms = 1_700_000_000_000i64;
        let intended_ms = raw_ms + 1_500;
        memory.seed_bytes("same.bin", b"old!", raw_ms);
        let vfs: Arc<dyn Vfs> = memory.clone();

        assert_ne!(
            crate::store::mtimefix::record_by_key(
                &vfs.identity(),
                &[("same.bin".into(), raw_ms, intended_ms)],
            ),
            crate::store::StateWriteStatus::Failed,
        );

        let mut options = opts();
        options.use_cache = true;
        let first = scan_root(&vfs, &options, &RunCtx::null(), Phase::ScanSource).unwrap();
        assert_eq!(first.entries[0].mtime_ms(), intended_ms);
        let first_hash = first.entries[0]
            .as_file()
            .and_then(|file| file.identity.digest())
            .cloned()
            .unwrap();

        memory.seed_bytes("same.bin", b"new!", raw_ms);
        let second = scan_root(&vfs, &options, &RunCtx::null(), Phase::ScanSource).unwrap();
        assert_eq!(second.entries[0].mtime_ms(), intended_ms);
        assert_ne!(
            second.entries[0]
                .as_file()
                .and_then(|file| file.identity.digest()),
            Some(&first_hash),
            "a same-size replacement with the same raw mtime must be re-read on this lane too",
        );
    }

    /// The generic lane's twin of `no_hash_scan_ends_with_all_discovered_items_done`: without
    /// content evidence the walk's own item count is already final, so the phase adopts it as the
    /// total rather than restarting the item epoch and ending at zero of nothing.
    #[test]
    fn no_hash_vfs_scan_ends_with_all_discovered_items_done() {
        let memory = Arc::new(MemVfs::new(&format!(
            "vfs-no-hash-progress-{}",
            std::process::id()
        )));
        for index in 0..7 {
            memory.seed_file(&format!("sub/f{index}.dat"), 100, 1_700_000_000_000);
        }
        let vfs: Arc<dyn Vfs> = memory;
        let (ctx, store) = RunCtx::collecting();
        let mut opt = opts();
        opt.hash = false;

        scan_root(&vfs, &opt, &ctx, Phase::ScanSource).unwrap();
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
    }

    #[test]
    fn no_hash_scan_ends_with_all_discovered_items_done() {
        let root = mk_tree("no-hash-progress", 7);
        let (ctx, store) = RunCtx::collecting();
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

    /// The reason `local::discovery` aborts instead of counting: a directory is recorded when its
    /// parent lists it, before the walk descends into it, so a directory that cannot be read is
    /// already in the table with zero children — and compare cannot tell "no children" from
    /// "children deleted". Under mirror that is a delete for every file the other side still
    /// holds. On macOS the everyday cause is TCC, not a chmod: ~/Desktop, ~/Documents and every
    /// external volume are gated by default.
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
