//! Retention policy and recovery for the local trash directory (P2-2).
//!
//! Every `apply` run leaves the originals it deleted/overwrote under `<trash_root>/<millisecond timestamp>/`.
//! Those directories used to be **never cleaned up**: long-term use fills the disk, and finding one file among hundreds of timestamp directories is down to luck.
//!
//! Three things are added here (semantics modelled on syncthing `lib/versioner/`):
//!   - `find`  — locate historical versions of one path across every timestamp directory (what a trash is actually for)
//!   - `restore` — pull one version back (dry-run by default)
//!   - `prune` — clean up by retention days / total size cap; optional **staggered thinning**
//!     (the interval table from `lib/versioner/staggered.go:47-53`: one copy per 30s for the first hour,
//!      one per hour for the rest of the day, one per day within 30 days, one per week after that)
//!
//! Note: jobs with `versioning = true` go to `.version_syncDash/` inside each root instead (see version.rs),
//! a path entirely separate from the local trash here; this module only handles the local trash.

use std::path::{Path, PathBuf};

pub fn trash_root() -> PathBuf {
    let base = if let Ok(l) = std::env::var("LOCALAPPDATA") {
        PathBuf::from(l).join("syncdash")
    } else if let Ok(h) = std::env::var("HOME") {
        PathBuf::from(h).join(".cache").join("syncdash")
    } else {
        PathBuf::from(".syncdash")
    };
    base.join("trash")
}

#[derive(Clone, Debug)]
pub struct Run {
    /// Directory name (millisecond timestamp)
    pub id: String,
    pub at_ms: u64,
    pub dir: PathBuf,
    pub files: u64,
    pub bytes: u64,
}

fn dir_stats(d: &Path) -> (u64, u64) {
    let mut files = 0u64;
    let mut bytes = 0u64;
    for e in walkdir::WalkDir::new(d).follow_links(false).into_iter().flatten() {
        if e.file_type().is_file() {
            files += 1;
            bytes += e.metadata().map(|m| m.len()).unwrap_or(0);
        }
    }
    (files, bytes)
}

/// List every trash run, oldest to newest
pub fn list_runs() -> Vec<Run> {
    let root = trash_root();
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(&root) else {
        return out;
    };
    for e in rd.flatten() {
        if !e.path().is_dir() {
            continue;
        }
        let id = e.file_name().to_string_lossy().into_owned();
        let Ok(at_ms) = id.parse::<u64>() else { continue };
        let (files, bytes) = dir_stats(&e.path());
        out.push(Run { id, at_ms, dir: e.path(), files, bytes });
    }
    out.sort_by_key(|r| r.at_ms);
    out
}

#[derive(Clone, Debug)]
pub struct Found {
    pub run_id: String,
    pub at_ms: u64,
    /// Absolute path inside the trash directory
    pub stored: PathBuf,
    /// Original path relative to the root ('/'-separated)
    pub rel: String,
    pub size: u64,
}

/// Find files across every run whose path contains `needle` (case-insensitive substring).
/// Results are newest-first — when restoring you almost always want the most recent version.
pub fn find(needle: &str) -> Vec<Found> {
    let needle = needle.to_lowercase();
    let mut out = Vec::new();
    for run in list_runs() {
        for e in walkdir::WalkDir::new(&run.dir).follow_links(false).into_iter().flatten() {
            if !e.file_type().is_file() {
                continue;
            }
            // walkdir descends from run.dir, so strip_prefix cannot fail;
            // should it ever fail, fall back to the whole path as before
            let rel = crate::foundation::path::to_rel(e.path(), &run.dir)
                .unwrap_or_else(|| e.path().to_string_lossy().replace('\\', "/"));
            if needle.is_empty() || rel.to_lowercase().contains(&needle) {
                out.push(Found {
                    run_id: run.id.clone(),
                    at_ms: run.at_ms,
                    stored: e.path().to_path_buf(),
                    rel,
                    size: e.metadata().map(|m| m.len()).unwrap_or(0),
                });
            }
        }
    }
    out.sort_by(|a, b| b.at_ms.cmp(&a.at_ms));
    out
}

/// Restore: put a file from the trash back at `dest_root/<rel>`.
/// `run_id = None` takes the newest version. Dry-run by default.
/// An existing destination is **never overwritten** (move it aside yourself first) — a trash exists to recover things, not to destroy them a second time.
pub fn restore(
    needle: &str,
    run_id: Option<&str>,
    dest_root: &Path,
    dry_run: bool,
) -> std::io::Result<(u64, u64, u64)> {
    let hits: Vec<Found> = find(needle)
        .into_iter()
        .filter(|f| run_id.map(|r| f.run_id == r).unwrap_or(true))
        .collect();
    if hits.is_empty() {
        return Ok((0, 0, 0));
    }
    // Take only the newest version of each rel
    let mut seen = std::collections::HashSet::new();
    let mut restored = 0u64;
    let mut skipped = 0u64;
    let mut errors = 0u64;
    for f in hits {
        if !seen.insert(f.rel.clone()) {
            continue;
        }
        let dst = crate::foundation::path::join_native(dest_root, &f.rel);
        if dst.exists() {
            crate::log_warn!("trash", "skip (exists): {}", dst.display());
            skipped += 1;
            continue;
        }
        if dry_run {
            println!("WOULD RESTORE  {}  <-  {} ({})", dst.display(), f.run_id, crate::foundation::fmt::human_bytes(f.size));
            skipped += 1;
            continue;
        }
        let res = (|| -> std::io::Result<()> {
            if let Some(p) = dst.parent() {
                std::fs::create_dir_all(p)?;
            }
            // Atomic write: the restore itself must not leave a half-written file either
            let mut st = crate::fs::staged::Staged::create(&dst)?;
            let mut src = std::fs::File::open(&f.stored)?;
            st.write_all_from(&mut src)?;
            st.seal(true)?;
            st.commit()
        })();
        match res {
            Ok(_) => {
                println!("RESTORED  {}  <-  {}", dst.display(), f.run_id);
                restored += 1;
            }
            Err(e) => {
                crate::log_error!("trash", "ERR  {}: {e}", dst.display());
                errors += 1;
            }
        }
    }
    Ok((restored, skipped, errors))
}

#[derive(Clone, Copy, Debug)]
pub struct Retention {
    /// Anything older than this many days is deleted. <=0 disables it
    pub keep_days: i64,
    /// Total byte cap across all runs; once over, the oldest go first. 0 disables it
    pub max_bytes: u64,
    /// Enable staggered thinning (smarter than plain day-based retention: dense recently, sparse further back)
    pub staggered: bool,
}

impl Default for Retention {
    fn default() -> Self {
        Retention { keep_days: 30, max_bytes: 10 * 1024 * 1024 * 1024, staggered: true }
    }
}

/// syncthing's `staggered` interval table (`lib/versioner/staggered.go:47-53`):
/// (adjacent versions must be at least step seconds apart, the interval covers everything younger than end seconds)
const INTERVALS: [(i64, i64); 4] = [
    (30, 3600),                    // first hour: at most one copy per 30s
    (3600, 86_400),                // rest of the day: one per hour
    (86_400, 30 * 86_400),         // within 30 days: one per day
    (7 * 86_400, 365 * 86_400),    // within a year: one per week
];

/// Given each run's **second-resolution** timestamp (any order), return the timestamps that should be deleted.
/// A pure function, so it is easy to unit-test — the only part of the retention policy with real algorithm in it.
pub fn staggered_removals(times_secs: &[i64], now_secs: i64) -> Vec<i64> {
    let mut times: Vec<i64> = times_secs.to_vec();
    times.sort_unstable(); // oldest to newest
    let mut remove = Vec::new();
    let mut prev_age = 0i64;
    let mut first = true;
    let max_age = INTERVALS[INTERVALS.len() - 1].1;
    for t in times {
        let age = now_secs - t;
        if max_age > 0 && age > max_age {
            remove.push(t); // past the maximum retention period
            continue;
        }
        if first {
            // The oldest copy is kept unconditionally; it anchors every later interval check
            prev_age = age;
            first = false;
            continue;
        }
        let step = INTERVALS.iter().find(|(_, end)| age < *end).map(|(s, _)| *s).unwrap_or(max_age);
        if prev_age - age < step {
            // Too close to the previously kept copy → thin it out
            remove.push(t);
            continue;
        }
        prev_age = age;
    }
    remove
}

/// Clean up per the retention policy. Returns (runs deleted, bytes freed).
pub fn prune(r: &Retention, dry_run: bool) -> std::io::Result<(u64, u64)> {
    let runs = list_runs();
    if runs.is_empty() {
        return Ok((0, 0));
    }
    let now_ms = crate::foundation::time::now_ms() as i64;
    let mut doomed: std::collections::HashSet<String> = std::collections::HashSet::new();

    // 1) past retention age
    if r.keep_days > 0 {
        let cutoff = now_ms - r.keep_days * 86_400_000;
        for run in &runs {
            if (run.at_ms as i64) < cutoff {
                doomed.insert(run.id.clone());
            }
        }
    }

    // 2) staggered thinning
    if r.staggered {
        let times: Vec<i64> = runs.iter().map(|x| (x.at_ms / 1000) as i64).collect();
        for t in staggered_removals(&times, now_ms / 1000) {
            if let Some(run) = runs.iter().find(|x| (x.at_ms / 1000) as i64 == t) {
                doomed.insert(run.id.clone());
            }
        }
    }

    // 3) total size cap: evict oldest-first until back under the line
    if r.max_bytes > 0 {
        let mut total: u64 = runs.iter().filter(|x| !doomed.contains(&x.id)).map(|x| x.bytes).sum();
        for run in runs.iter() {
            if total <= r.max_bytes {
                break;
            }
            if doomed.insert(run.id.clone()) {
                total = total.saturating_sub(run.bytes);
            }
        }
    }

    // Always keep the newest run: it is the most likely undo for the mistake you just made
    if let Some(newest) = runs.last() {
        doomed.remove(&newest.id);
    }

    let mut n = 0u64;
    let mut freed = 0u64;
    for run in &runs {
        if !doomed.contains(&run.id) {
            continue;
        }
        if dry_run {
            println!("WOULD PRUNE  {}  ({} files, {})", run.id, run.files, crate::foundation::fmt::human_bytes(run.bytes));
        } else if let Err(e) = std::fs::remove_dir_all(&run.dir) {
            crate::log_error!("trash", "ERR  prune {}: {e}", run.dir.display());
            continue;
        }
        n += 1;
        freed += run.bytes;
    }
    Ok((n, freed))
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: i64 = 3600;
    const D: i64 = 86_400;

    #[test]
    fn staggered_drops_over_max_age() {
        let now = 1_000_000_000;
        let times = vec![now - 400 * D, now - 10];
        let rm = staggered_removals(&times, now);
        assert!(rm.contains(&(now - 400 * D)), "older than a year must go");
        assert!(!rm.contains(&(now - 10)));
    }

    #[test]
    fn staggered_thins_dense_recent_runs() {
        let now = 1_000_000_000;
        // one copy per 10s inside the first hour → the interval demands 30s, so most should be thinned out
        let times: Vec<i64> = (0..12).map(|i| now - i * 10).collect();
        let rm = staggered_removals(&times, now);
        let kept = times.len() - rm.len();
        assert!(kept >= 3 && kept <= 6, "12 runs 10s apart should thin to ~4, got {kept}");
    }

    #[test]
    fn staggered_keeps_well_spaced_runs() {
        let now = 1_000_000_000;
        // the same-day interval demands 1 hour of spacing; these are 2 hours apart → nothing should be deleted
        let times: Vec<i64> = (1..10).map(|i| now - i * 2 * H).collect();
        let rm = staggered_removals(&times, now);
        assert!(rm.is_empty(), "runs spaced wider than the interval must all survive, removed {rm:?}");
    }

    #[test]
    fn staggered_always_keeps_the_oldest_anchor() {
        let now = 1_000_000_000;
        let times = vec![now - 5 * D, now - 5 * D + 1, now - 5 * D + 2];
        let rm = staggered_removals(&times, now);
        assert!(!rm.contains(&(now - 5 * D)), "oldest is the anchor and must survive");
        assert_eq!(rm.len(), 2, "the two that crowd it must go");
    }

    #[test]
    fn empty_input_is_fine() {
        assert!(staggered_removals(&[], 0).is_empty());
    }
}
