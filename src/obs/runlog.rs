//! Run log (the data behind FFS's Log column / "Last sync" column).
//!
//! - Index: `<log_dir>/runs.jsonl` — one line per run (append-only, human-readable and auditable).
//! - Detail: `<log_dir>/<YYYYMMDD-HHMMSS>-<job>-<kind>/` — one directory per apply-class run:
//!   - `summary.json` run summary (= the same record as the index line, so the detail still explains itself if the index is corrupt)
//!   - `plan.jsonl`   the plan: what this run **intended** to do
//!   - `run.jsonl`    the event stream: narration, phase boundaries, errors
//!   - `errors.jsonl` the error detail: Error plus Log at warning level and above
//!   - `items.jsonl`  the execution detail: what this run **actually** did (one op per line with its outcome)
//! - compare has no side effects: one index line, no directory (a watch round every 30s = 2880 a day).
//! - Writing logs must never fail a sync: all persistence is best-effort, failures go to stderr.
//!
//! Two critical changes in v0.10 relative to v0.9:
//! 1. **Streaming**: events are persisted as they arrive (`logging::FileSink`) instead of written in
//!    one go at `finish` — v0.9 lost the entire log when the process was killed.
//! 2. **Plan and execution kept apart**: what v0.9 wrote into the detail were the **planned ops**
//!    handed to apply, with not one word on which succeeded, failed or were KEPT. Now plan and items are separate files.

use crate::model::plan::Op;
use crate::obs::logging::{self, FileSink};
use crate::model::event::{LogLevel, ProgressEvent};
use crate::obs::progress::{ApplyOutcome, ProgressSink, RunCtx};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::foundation::names::{
    RUNLOG_INDEX_FILE as INDEX_FILE, RUNLOG_PLAN_FILE as PLAN_FILE,
    RUNLOG_SUMMARY_FILE as SUMMARY_FILE,
};

#[derive(Serialize, Deserialize, Clone, Debug, ts_rs::TS)]
#[ts(export, export_to = "../typescript/core/types/generated/")]
pub struct RunRecord {
    /// When the run started (unix ms)
    #[ts(type = "number")]
    pub ts_ms: i64,
    pub job: String,
    /// "apply" | "remote-apply" | "compare" | "remote-compare"
    pub kind: String,
    #[ts(type = "number")]
    pub done: u64,
    #[ts(type = "number")]
    pub skipped: u64,
    #[ts(type = "number")]
    pub errors: u64,
    #[ts(type = "number")]
    pub bytes: u64,
    #[ts(type = "number")]
    pub elapsed_ms: u64,
    pub cancelled: bool,
    /// Run directory name (relative to log_dir). compare-class runs have only an index line, no directory → None
    #[serde(default)]
    pub run_id: Option<String>,
    /// How many warnings are in the error detail (the error count is in `errors`)
    #[serde(default)]
    #[ts(type = "number")]
    pub warnings: u64,
    /// compare-class: how many differences were found. None for apply-class
    #[serde(default)]
    #[ts(type = "number | null")]
    pub ops_found: Option<u64>,
    /// Whether the run went all the way through. `start` first writes a `finished:false` summary and
    /// `finish` overwrites it with true — a run killed midway has no index line (`finish` never ran),
    /// only a directory; this field is what lets that directory still say "I did not finish".
    /// Old records default to true: v0.9 only wrote a record inside `finish`, so anything written did finish.
    #[serde(default = "yes")]
    pub finished: bool,
    /// v0.9's detail file name. New records no longer write it, but old indexes carry it — readers still honor it.
    #[serde(default)]
    pub detail: Option<String>,
}

fn yes() -> bool {
    true
}

pub fn logs_dir() -> PathBuf {
    crate::store::settings::load().resolved_log_dir()
}

fn index_path() -> PathBuf {
    logs_dir().join(INDEX_FILE)
}

fn sanitize(name: &str) -> String {
    name.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect()
}

/// Reject any name that could escape log_dir. The entry guard for `artifact_lines` / `detail_lines`.
fn safe_component(s: &str) -> bool {
    !s.is_empty() && !s.contains('/') && !s.contains('\\') && !s.contains("..")
}


/// unix ms → `YYYYMMDD-HHMMSS` (UTC). Directory names have to sort, and that is not worth a chrono/time dependency.
/// Rendering local time is the frontend's job (`main.ts` already has `relTime`); the data always carries `ts_ms`.
fn stamp(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let sod = secs.rem_euclid(86_400);
    let (h, mi, s) = (sod / 3600, (sod % 3600) / 60, sod % 60);
    let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
    format!("{y:04}{m:02}{d:02}-{h:02}{mi:02}{s:02}")
}

/// days since 1970-01-01 → (year, month, day). Howard Hinnant's `civil_from_days`.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}


/// Tallies the makeup of the error detail in passing — the source of summary's "3 warnings, 2 errors".
#[derive(Default)]
struct Tally {
    warnings: AtomicU64,
    errors: AtomicU64,
}

struct TallySink(Arc<Tally>);

impl ProgressSink for TallySink {
    fn emit(&self, ev: ProgressEvent) {
        match &ev {
            ProgressEvent::Log { level: LogLevel::Warn, .. } => {
                self.0.warnings.fetch_add(1, Ordering::Relaxed);
            }
            ProgressEvent::Log { level: LogLevel::Error, .. } | ProgressEvent::Error { .. } => {
                self.0.errors.fetch_add(1, Ordering::Relaxed);
            }
            _ => {}
        }
    }
}

/// Recorder for one apply-class run.
///
/// `start` threads the file sink into the event stream **and installs it in the process registry** —
/// the latter is what lets the diagnostics in `trash` / `version` / `lock`, which have no `RunCtx`, land in the same directory.
/// `finish` only writes the summary and the index: the detail was already persisted as the run went.
pub struct Recorder {
    pub ctx: RunCtx,
    name: String,
    kind: String,
    ts_ms: i64,
    run_id: Option<String>,
    file: Option<Arc<FileSink>>,
    tally: Arc<Tally>,
    /// Restores the registry automatically on drop — leaking the guard cross-contaminates this directory with the next run's log
    _guard: Option<crate::obs::progress::SinkGuard>,
}

impl Recorder {
    pub fn start(name: &str, kind: &str, base: &RunCtx, ops: &[Op]) -> Recorder {
        let cfg = crate::store::settings::load();
        let ts_ms = crate::foundation::time::now_ms() as i64;
        let run_id = format!("{}-{}-{}", stamp(ts_ms), sanitize(name), sanitize(kind));
        let dir = cfg.resolved_log_dir().join(&run_id);
        let tally = Arc::new(Tally::default());

        // Do not thread `progress::current()` in here: `RunCtx::null()` already brought the process's
        // ambient sink (the StderrSink the CLI installs at startup) into `base.sink`; threading it again duplicates output.
        let mut sinks: Vec<Arc<dyn ProgressSink>> = vec![base.sink.clone(), Arc::new(TallySink(tally.clone()))];
        let (file, run_id) = match std::fs::create_dir_all(&dir) {
            Ok(_) => {
                write_plan(&dir, ops);
                // Write a finished:false summary up front — so even if the process is killed, this
                // directory can still say "who I am and that I did not finish" instead of becoming anonymous debris
                write_summary(&dir, &pending_record(name, kind, ts_ms, &run_id, ops.len() as u64));
                let f = Arc::new(FileSink::open(&dir, cfg.level));
                sinks.push(f.clone() as Arc<dyn ProgressSink>);
                (Some(f), Some(run_id))
            }
            Err(e) => {
                // A directory we cannot create costs us the detail, not the sync — the index line is still written
                eprintln!("runlog: cannot create {}: {e}", dir.display());
                (None, None)
            }
        };

        let sink: Arc<dyn ProgressSink> = Arc::new(logging::MultiSink::new(sinks));
        let guard = crate::obs::progress::install(sink.clone());
        Recorder {
            ctx: RunCtx::new(base.ctl.clone(), sink),
            name: name.to_string(),
            kind: kind.to_string(),
            ts_ms,
            run_id,
            file,
            tally,
            _guard: Some(guard),
        }
    }

    /// Best-effort persistence; returns the index record just written (the desktop echoes it straight back as "last sync")
    pub fn finish(self, out: &ApplyOutcome, elapsed_ms: u64) -> RunRecord {
        // Flush every buffer first, then write the summary — the summary existing means the detail is complete
        if let Some(f) = &self.file {
            f.flush_all();
        }
        let rec = RunRecord {
            ts_ms: self.ts_ms,
            job: self.name,
            kind: self.kind,
            done: out.done,
            skipped: out.skipped,
            errors: out.errors,
            bytes: out.bytes_copied,
            elapsed_ms,
            cancelled: out.cancelled,
            run_id: self.run_id.clone(),
            warnings: self.tally.warnings.load(Ordering::Relaxed),
            ops_found: None,
            finished: true,
            detail: None,
        };
        if let Some(id) = &self.run_id {
            // Overwrite the finished:false placeholder written by start
            write_summary(&logs_dir().join(id), &rec);
        }
        append_index(&rec);
        rec
    }
}

/// Placeholder summary written at the start of a run: the plan size is known, the results are empty, `finished:false`.
fn pending_record(name: &str, kind: &str, ts_ms: i64, run_id: &str, planned: u64) -> RunRecord {
    RunRecord {
        ts_ms,
        job: name.to_string(),
        kind: kind.to_string(),
        done: 0,
        skipped: planned,
        errors: 0,
        bytes: 0,
        elapsed_ms: 0,
        cancelled: false,
        run_id: Some(run_id.to_string()),
        warnings: 0,
        ops_found: None,
        finished: false,
        detail: None,
    }
}

fn write_summary(dir: &Path, rec: &RunRecord) {
    match serde_json::to_string_pretty(rec) {
        Ok(t) => {
            if let Err(e) = std::fs::write(dir.join(SUMMARY_FILE), t) {
                eprintln!("runlog: cannot write the summary into {}: {e}", dir.display());
            }
        }
        Err(e) => eprintln!("runlog: summary serialization failed: {e}"),
    }
}

fn write_plan(dir: &Path, ops: &[Op]) {
    let write = || -> std::io::Result<()> {
        let f = std::fs::File::create(dir.join(PLAN_FILE))?;
        let mut w = std::io::BufWriter::new(f);
        for op in ops {
            writeln!(w, "{}", serde_json::json!({ "op": op }))?;
        }
        w.flush()
    };
    if let Err(e) = write() {
        eprintln!("runlog: writing the plan failed: {e}");
    }
}

fn append_index(rec: &RunRecord) {
    let append = || -> std::io::Result<()> {
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(index_path())?;
        writeln!(f, "{}", serde_json::to_string(rec)?)
    };
    if let Err(e) = append() {
        eprintln!("runlog: appending to the index failed: {e}");
    }
}

/// The trace a compare-class run leaves: append one index line, **no directory**.
///
/// A watch round every 30s = 2880 a day, and creating a directory each time would flood the log disk;
/// the single line "when we compared and how many differences we found" is worth keeping on its own.
pub fn compare_summary(name: &str, kind: &str, ts_ms: i64, ops_found: u64, elapsed_ms: u64, cancelled: bool) {
    if !crate::store::settings::load().logs_compare() {
        return;
    }
    append_index(&RunRecord {
        ts_ms,
        job: name.to_string(),
        kind: kind.to_string(),
        done: 0,
        skipped: 0,
        errors: 0,
        bytes: 0,
        elapsed_ms,
        cancelled,
        run_id: None,
        warnings: 0,
        ops_found: Some(ops_found),
        finished: true,
        detail: None,
    });
}


/// History (newest → oldest). job = None means everything; corrupt lines are skipped, never fatal.
pub fn history(job: Option<&str>, limit: usize) -> Vec<RunRecord> {
    let Ok(text) = std::fs::read_to_string(index_path()) else {
        return Vec::new();
    };
    let mut out: Vec<RunRecord> = text
        .lines()
        .filter_map(|l| serde_json::from_str::<RunRecord>(l.trim()).ok())
        .filter(|r| job.map(|j| r.job == j).unwrap_or(true))
        .collect();
    out.reverse();
    out.truncate(limit);
    out
}

/// History plus **interrupted runs** (newest → oldest).
///
/// The index line is only appended inside `finish`, so a run whose process was killed does not exist
/// in the index at all — only a directory is left. A UI that reads only the index makes crashed runs
/// completely invisible, and those are exactly the ones that most need to be seen. This merges in the
/// directory's `summary.json` (the `finished:false` placeholder written by `start`).
pub fn history_merged(job: Option<&str>, limit: usize) -> Vec<RunRecord> {
    let mut out = history(job, usize::MAX);
    let known: std::collections::HashSet<String> =
        out.iter().filter_map(|r| r.run_id.clone()).collect();
    if let Ok(rd) = std::fs::read_dir(logs_dir()) {
        for e in rd.flatten() {
            let p = e.path();
            if !p.is_dir() {
                continue;
            }
            let Some(id) = p.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if known.contains(id) {
                continue;
            }
            let Ok(t) = std::fs::read_to_string(p.join(SUMMARY_FILE)) else {
                continue;
            };
            if let Ok(r) = serde_json::from_str::<RunRecord>(&t) {
                if job.map(|j| r.job == j).unwrap_or(true) {
                    out.push(r);
                }
            }
        }
    }
    out.sort_by(|a, b| b.ts_ms.cmp(&a.ts_ms));
    out.truncate(limit);
    out
}

/// The most recent run per job that **actually executed** (the sidebar's "last sync" dot).
/// compare lines do not count — they moved no data, and calling one "last sync" would be a lie.
pub fn latest_by_job() -> std::collections::HashMap<String, RunRecord> {
    let mut m = std::collections::HashMap::new();
    let Ok(text) = std::fs::read_to_string(index_path()) else {
        return m;
    };
    for l in text.lines() {
        if let Ok(r) = serde_json::from_str::<RunRecord>(l.trim()) {
            if r.ops_found.is_some() {
                continue;
            }
            m.insert(r.job.clone(), r); // Append-only file: whatever was written later is naturally newer
        }
    }
    m
}

/// One artifact of a run (raw JSONL lines; the line count is capped so memory cannot blow up).
/// `which` ∈ run / errors / items / plan / summary.
pub fn artifact_lines(run_id: &str, which: &str, max: usize) -> Vec<String> {
    if !safe_component(run_id) {
        return Vec::new();
    }
    let file = match which {
        "run" => FileSink::RUN,
        "errors" => FileSink::ERRORS,
        "items" => FileSink::ITEMS,
        "plan" => PLAN_FILE,
        "summary" => SUMMARY_FILE,
        _ => return Vec::new(),
    };
    let Ok(text) = std::fs::read_to_string(logs_dir().join(run_id).join(file)) else {
        return Vec::new();
    };
    text.lines().take(max).map(|s| s.to_string()).collect()
}

/// The detail file of an old v0.9 record (a single jsonl sitting flat under log_dir).
/// New records go through `artifact_lines`; this stays so pre-rework history still opens.
pub fn detail_lines(detail: &str, max: usize) -> Vec<String> {
    if !safe_component(detail) {
        return Vec::new();
    }
    let Ok(text) = std::fs::read_to_string(logs_dir().join(detail)) else {
        return Vec::new();
    };
    text.lines().take(max).map(|s| s.to_string()).collect()
}


fn dir_size(p: &Path) -> u64 {
    let Ok(rd) = std::fs::read_dir(p) else {
        return 0;
    };
    let mut n = 0;
    for e in rd.flatten() {
        let path = e.path();
        n += if path.is_dir() { dir_size(&path) } else { e.metadata().map(|m| m.len()).unwrap_or(0) };
    }
    n
}

/// Delete one run record's detail (the old format's flat file / the new format's directory).
fn drop_detail(r: &RunRecord, root: &Path) {
    if let Some(id) = &r.run_id {
        if safe_component(id) {
            let _ = std::fs::remove_dir_all(root.join(id));
        }
    }
    if let Some(d) = &r.detail {
        if safe_component(d) {
            let _ = std::fs::remove_file(root.join(d));
        }
    }
}

/// Retention on two conditions: age in days plus total size. Returns how many runs were deleted.
///
/// `keep_days == 0` turns off the age rule; `max_total_mb == 0` turns off the size rule.
/// The execution detail records everything (tens of thousands of lines for one big sync) — the size gate is its seatbelt.
pub fn prune(keep_days: u64, max_total_mb: u64) -> u64 {
    let root = logs_dir();
    let idx = root.join(INDEX_FILE);
    let Ok(text) = std::fs::read_to_string(&idx) else {
        return 0;
    };
    // Lines we cannot parse are conservatively kept, but they do not count toward the size budget
    let mut kept: Vec<(Option<RunRecord>, String)> = Vec::new();
    let mut dropped = 0u64;
    let cutoff = crate::foundation::time::now_ms() as i64 - (keep_days as i64) * 24 * 3600 * 1000;
    for l in text.lines() {
        let raw = l.trim();
        if raw.is_empty() {
            continue;
        }
        match serde_json::from_str::<RunRecord>(raw) {
            Ok(r) => {
                if keep_days > 0 && r.ts_ms < cutoff {
                    drop_detail(&r, &root);
                    dropped += 1;
                } else {
                    kept.push((Some(r), raw.to_string()));
                }
            }
            Err(_) => kept.push((None, raw.to_string())),
        }
    }

    // Size gate: if we are still over, delete from the oldest first (kept is in index order = chronological order)
    if max_total_mb > 0 {
        let cap = max_total_mb * 1024 * 1024;
        let mut sizes: Vec<u64> = kept
            .iter()
            .map(|(r, _)| match r.as_ref().and_then(|r| r.run_id.as_deref()) {
                Some(id) if safe_component(id) => dir_size(&root.join(id)),
                _ => 0,
            })
            .collect();
        let mut total: u64 = sizes.iter().sum();
        let mut i = 0;
        while total > cap && i < kept.len() {
            if let (Some(r), _) = &kept[i] {
                if r.run_id.is_some() || r.detail.is_some() {
                    drop_detail(r, &root);
                    total = total.saturating_sub(sizes[i]);
                    sizes[i] = 0;
                    kept[i].0 = None;
                    kept[i].1.clear(); // Mark as deleted
                    dropped += 1;
                }
            }
            i += 1;
        }
        kept.retain(|(_, raw)| !raw.is_empty());
    }

    if dropped > 0 {
        let body: String = kept.iter().map(|(_, l)| format!("{l}\n")).collect();
        if let Err(e) = std::fs::write(&idx, body) {
            eprintln!("runlog: rewriting the index failed: {e}");
        }
        // Sweep orphan directories too (left by a crash, absent from the index). Only touch those older
        // than the cutoff, so we do not delete the directory of a run that is **still going** — it has not written its index line yet.
        if keep_days > 0 {
            sweep_orphans(&root, &kept, cutoff);
        }
    }
    dropped
}

fn sweep_orphans(root: &Path, kept: &[(Option<RunRecord>, String)], cutoff: i64) {
    let live: std::collections::HashSet<&str> =
        kept.iter().filter_map(|(r, _)| r.as_ref().and_then(|r| r.run_id.as_deref())).collect();
    let Ok(rd) = std::fs::read_dir(root) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if !p.is_dir() {
            continue;
        }
        let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if live.contains(name) {
            continue;
        }
        let old_enough = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| (d.as_millis() as i64) < cutoff)
            .unwrap_or(false);
        if old_enough {
            let _ = std::fs::remove_dir_all(&p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_roundtrip_keeps_old_fields_readable() {
        let a = RunRecord {
            ts_ms: 1,
            job: "j".into(),
            kind: "apply".into(),
            done: 3,
            skipped: 0,
            errors: 1,
            bytes: 42,
            elapsed_ms: 100,
            cancelled: false,
            run_id: Some("20260101-000000-j-apply".into()),
            warnings: 2,
            ops_found: None,
            finished: true,
            detail: None,
        };
        let s = serde_json::to_string(&a).unwrap();
        let b: RunRecord = serde_json::from_str(&s).unwrap();
        assert_eq!((b.done, b.errors, b.warnings), (3, 1, 2));
        // Old index lines written by v0.9 must still read (every new field is a serde default)
        let old = r#"{"ts_ms":9,"job":"j","kind":"apply","done":1,"skipped":0,"errors":0,
            "bytes":0,"elapsed_ms":5,"cancelled":false,"detail":"9-j.jsonl"}"#;
        let c: RunRecord = serde_json::from_str(old).unwrap();
        assert_eq!(c.detail.as_deref(), Some("9-j.jsonl"));
        assert!(c.run_id.is_none() && c.ops_found.is_none());
        // v0.9 only wrote a record inside finish, so anything written did finish → old lines must read back as finished
        assert!(c.finished, "old index lines have no finished field; the default must be true");
    }

    #[test]
    fn stamp_matches_known_unix_times() {
        assert_eq!(stamp(0), "19700101-000000");
        assert_eq!(stamp(946_684_800_000), "20000101-000000"); // 2000-01-01T00:00:00Z
        assert_eq!(stamp(1_000_000_000_000), "20010909-014640"); // the classic billionth second
        assert_eq!(stamp(1_709_164_800_000), "20240229-000000"); // leap day
    }

    #[test]
    fn stamps_sort_chronologically() {
        // Directory names must sort lexicographically as they stand — the entire reason for not pulling in chrono
        let mut v = vec![stamp(1_000_000_000_000), stamp(0), stamp(946_684_800_000)];
        v.sort();
        assert_eq!(v, vec!["19700101-000000", "20000101-000000", "20010909-014640"]);
    }

    #[test]
    fn path_escapes_are_refused() {
        assert!(!safe_component("../../etc/passwd"));
        assert!(!safe_component("a/b"));
        assert!(!safe_component("a\\b"));
        assert!(!safe_component(""));
        assert!(safe_component("20260101-000000-job-apply"));
        assert!(artifact_lines("../secrets", "run", 10).is_empty());
        assert!(artifact_lines("ok-id", "no-such-artifact", 10).is_empty());
    }

    #[test]
    fn sanitize_strips_path_chars() {
        assert!(sanitize("a b/c\\d").chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-'));
    }
}
