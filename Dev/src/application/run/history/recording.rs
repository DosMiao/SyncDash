//! Best-effort event recording and atomic publication into the run index.

use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::foundation::names::{
    RUNLOG_PLAN_FILE as PLAN_FILE, RUNLOG_SUMMARY_FILE as SUMMARY_FILE,
};
use crate::foundation::path::EntryName;
use crate::fs::local_root::{LocalDirectory, LocalRoot};
use crate::model::event::{LogLevel, ProgressEvent};
use crate::model::plan::Op;
use crate::obs::logging::{self, FileSink};
use crate::obs::progress::{ApplyOutcome, ProgressSink, RunCtx};

use super::codec::validate_record;
use super::migration::with_index_lock;
use super::model::{RunArtifacts, RunKind, RunRecord, RunSubject, RUN_RECORD_SCHEMA};
use super::paths::{index_relative_path, random_record_id, root_directory, sanitize};

static NEXT_RUN_ID: AtomicU64 = AtomicU64::new(1);

pub(super) fn create_run_dir(
    root: &LocalRoot,
    ts_ms: i64,
    name: &str,
    kind: RunKind,
) -> std::io::Result<(String, LocalDirectory)> {
    let root_directory = root.open_directory(&root_directory())?;
    loop {
        let sequence = NEXT_RUN_ID.fetch_add(1, Ordering::Relaxed);
        let run_id = format!(
            "{}-{:03}-{}-{}-{}-{sequence}",
            crate::foundation::time::stamp_compact(ts_ms),
            ts_ms.rem_euclid(1_000),
            sanitize(name),
            kind.as_str(),
            std::process::id(),
        );
        let entry_name = EntryName::try_from(run_id.as_str())
            .expect("generated run identifiers satisfy the entry-name contract");
        match root_directory.create_child_directory(&entry_name) {
            Ok(directory) => return Ok((run_id, directory)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
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
            ProgressEvent::Log {
                level: LogLevel::Warn,
                ..
            } => {
                self.0.warnings.fetch_add(1, Ordering::Relaxed);
            }
            ProgressEvent::Log {
                level: LogLevel::Error,
                ..
            }
            | ProgressEvent::Error { .. } => {
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
    subject: RunSubject,
    kind: RunKind,
    record_id: Option<String>,
    ts_ms: i64,
    root: Option<LocalRoot>,
    dir: Option<LocalDirectory>,
    run_id: Option<String>,
    file: Option<Arc<FileSink>>,
    tally: Arc<Tally>,
    /// Restores the registry automatically on drop — leaking the guard cross-contaminates this directory with the next run's log
    _guard: Option<crate::obs::progress::SinkGuard>,
}

impl Recorder {
    pub fn start(subject: RunSubject, kind: RunKind, base: &RunCtx, ops: &[Op]) -> Recorder {
        let cfg = crate::store::settings::load();
        let ts_ms = crate::foundation::time::now_ms() as i64;
        let record_id = match random_record_id() {
            Ok(record_id) => Some(record_id),
            Err(error) => {
                eprintln!("runlog: {error}");
                None
            }
        };
        let root_path = cfg.resolved_log_dir();
        let root = match LocalRoot::create(root_path.clone())
            .and_then(|root| with_index_lock(&root, |_| Ok(())).map(|()| root))
        {
            Ok(root) if record_id.is_some() => Some(root),
            Ok(_) => None,
            Err(error) => {
                eprintln!(
                    "runlog: cannot prepare the configured log root {}: {error}",
                    root_path.display()
                );
                None
            }
        };
        let tally = Arc::new(Tally::default());

        // Do not thread `progress::current()` in here: `RunCtx::null()` already brought the process's
        // ambient sink (the StderrSink the CLI installs at startup) into `base.sink`; threading it again duplicates output.
        let mut sinks: Vec<Arc<dyn ProgressSink>> =
            vec![base.sink.clone(), Arc::new(TallySink(tally.clone()))];
        let run_directory = root
            .as_ref()
            .map(|root| create_run_dir(root, ts_ms, &subject.job_name, kind));
        let (file, run_id, dir) = match run_directory {
            Some(Ok((run_id, dir))) => {
                write_plan(&dir, ops);
                // Write a finished:false summary up front — so even if the process is killed, this
                // directory can still say "who I am and that I did not finish" instead of becoming anonymous debris
                write_summary(
                    &dir,
                    &pending_record(
                        record_id
                            .as_deref()
                            .expect("a prepared run-log root requires a record identity"),
                        &subject,
                        kind,
                        ts_ms,
                        &run_id,
                        ops.len() as u64,
                    ),
                );
                let f = Arc::new(FileSink::open_in(&dir, cfg.level));
                sinks.push(f.clone() as Arc<dyn ProgressSink>);
                (Some(f), Some(run_id), Some(dir))
            }
            Some(Err(e)) => {
                // A directory we cannot create costs us the detail, not the sync — the index line is still written
                eprintln!(
                    "runlog: cannot create a run directory under {}: {e}",
                    root_path.display()
                );
                (None, None, None)
            }
            None => (None, None, None),
        };

        let sink: Arc<dyn ProgressSink> = Arc::new(logging::MultiSink::new(sinks));
        let guard = crate::obs::progress::install(sink.clone());
        Recorder {
            ctx: RunCtx::new(base.ctl.clone(), sink),
            subject,
            kind,
            record_id,
            ts_ms,
            root,
            dir,
            run_id,
            file,
            tally,
            _guard: Some(guard),
        }
    }

    /// Returns the record when a cryptographic record identity was available; persistence remains best-effort.
    pub fn finish(self, out: &ApplyOutcome, elapsed_ms: u64) -> Option<RunRecord> {
        // Flush every buffer first, then write the summary — the summary existing means the detail is complete
        if let Some(f) = &self.file {
            f.flush_all();
        }
        let record_id = self.record_id?;
        let rec = RunRecord {
            schema: RUN_RECORD_SCHEMA,
            record_id,
            ts_ms: self.ts_ms,
            subject: self.subject,
            kind: self.kind,
            done: out.done,
            skipped: out.skipped,
            errors: out.errors,
            bytes: out.bytes_copied,
            elapsed_ms,
            cancelled: out.cancelled,
            artifacts: self
                .run_id
                .clone()
                .map_or(RunArtifacts::Unavailable, |run_id| {
                    RunArtifacts::Directory { run_id }
                }),
            warnings: self.tally.warnings.load(Ordering::Relaxed),
            ops_found: None,
            finished: true,
        };
        if let Some(dir) = &self.dir {
            // Overwrite the finished:false placeholder written by start
            write_summary(dir, &rec);
        }
        if let Some(root) = &self.root {
            append_index(root, &rec);
        }
        Some(rec)
    }
}

/// Placeholder summary written at the start of a run: the plan size is known, the results are empty, `finished:false`.
pub(super) fn pending_record(
    record_id: &str,
    subject: &RunSubject,
    kind: RunKind,
    ts_ms: i64,
    run_id: &str,
    planned: u64,
) -> RunRecord {
    RunRecord {
        schema: RUN_RECORD_SCHEMA,
        record_id: record_id.to_owned(),
        ts_ms,
        subject: subject.clone(),
        kind,
        done: 0,
        skipped: planned,
        errors: 0,
        bytes: 0,
        elapsed_ms: 0,
        cancelled: false,
        artifacts: RunArtifacts::Directory {
            run_id: run_id.to_owned(),
        },
        warnings: 0,
        ops_found: None,
        finished: false,
    }
}

fn write_summary(dir: &LocalDirectory, rec: &RunRecord) {
    match serde_json::to_string_pretty(rec) {
        Ok(t) => {
            let result = (|| -> std::io::Result<()> {
                let name = EntryName::try_from(SUMMARY_FILE)
                    .expect("the summary artifact name is a valid entry");
                let mut staged = dir.create_staged(name)?;
                staged.write_all(t.as_bytes())?;
                staged.seal(true)?;
                staged.commit()
            })();
            if let Err(e) = result {
                eprintln!("runlog: cannot write the run summary: {e}");
            }
        }
        Err(e) => eprintln!("runlog: summary serialization failed: {e}"),
    }
}

fn write_plan(dir: &LocalDirectory, ops: &[Op]) {
    let write = || -> std::io::Result<()> {
        let name = EntryName::try_from(PLAN_FILE).expect("the plan artifact name is a valid entry");
        let f = dir.create_new_file(&name)?;
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

fn append_index(root: &LocalRoot, rec: &RunRecord) {
    let append = || -> std::io::Result<()> {
        validate_record(rec)?;
        with_index_lock(root, |root| {
            let relative = index_relative_path();
            let mut file = root.open_append(&relative)?;
            writeln!(file, "{}", serde_json::to_string(rec)?)?;
            file.sync_all()?;
            root.sync_parent(&relative)
        })
    };
    if let Err(e) = append() {
        eprintln!("runlog: appending to the index failed: {e}");
    }
}

/// The trace a compare-class run leaves: append one index line, **no directory**.
///
/// A watch round every 30s = 2880 a day, and creating a directory each time would flood the log disk;
/// the single line "when we compared and how many differences we found" is worth keeping on its own.
pub fn compare_summary(
    subject: RunSubject,
    kind: RunKind,
    ts_ms: i64,
    ops_found: u64,
    elapsed_ms: u64,
    cancelled: bool,
) {
    let settings = crate::store::settings::load();
    if !settings.logs_compare() {
        return;
    }
    let root_path = settings.resolved_log_dir();
    let record = RunRecord {
        schema: RUN_RECORD_SCHEMA,
        record_id: match random_record_id() {
            Ok(record_id) => record_id,
            Err(error) => {
                eprintln!("runlog: {error}");
                return;
            }
        },
        ts_ms,
        subject,
        kind,
        done: 0,
        skipped: 0,
        errors: 0,
        bytes: 0,
        elapsed_ms,
        cancelled,
        artifacts: RunArtifacts::SummaryOnly,
        warnings: 0,
        ops_found: Some(ops_found),
        finished: true,
    };
    match LocalRoot::create(root_path) {
        Ok(root) => append_index(&root, &record),
        Err(error) => eprintln!("runlog: opening the log root failed: {error}"),
    }
}
