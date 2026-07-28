//! v0.10: centralised logging.
//!
//! **Why not pull in tracing/log**: the project already has one event bus running through the whole
//! pipeline (`progress::ProgressSink`), `RunCtx` already reaches every engine function, and
//! `runlog::ErrCollector` has already demonstrated the sink-decorator pattern. Stacking another
//! facade on top would only give one thing two outlets.
//! This module does exactly one thing: it merges the stray `eprintln!`s into that existing bus.
//!
//! **Why macros plus a process-wide registry**: the call sites in `trash.rs` / `version.rs` / `lock.rs`
//! have no `RunCtx`, and changing signatures would ripple through dozens of functions. The registry
//! puts them on the bus with zero signature churn; where ctx is in hand the code still calls `ctx.log()` directly.
//!
//! **With no sink installed, print verbatim to stderr** — this is what makes "CLI output is byte-for-byte
//! identical before and after the rework" hold, so the correctness of the swap (P3) does not depend on
//! whether the desktop shell or the CLI installed a sink.

use crate::foundation::names::{APP_LOG_FILE, RUNLOG_ERRORS_FILE, RUNLOG_ITEMS_FILE, RUNLOG_RUN_FILE};
use crate::model::event::{LogLevel, ProgressEvent};
use crate::obs::progress::{current, ProgressSink};
use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex};

/// Where the macros land. With nobody holding the sink it goes verbatim to stderr — the pre-rework behavior.
pub fn emit(level: LogLevel, scope: &str, message: String) {
    match current() {
        Some(s) => s.emit(ProgressEvent::Log {
            ts_ms: crate::foundation::time::now_ms(),
            level,
            scope: scope.to_string(),
            message,
        }),
        None => eprintln!("{message}"),
    }
}

/// `log_info!("run", "remote {host}: {os}")` — the arguments match `format!`.
/// Messages keep the prefix the call site already had (`[job] warning: …`), so the swap does not change one byte of CLI output.
#[macro_export]
macro_rules! log_info {
    ($scope:expr, $($arg:tt)*) => {
        $crate::obs::logging::emit($crate::model::event::LogLevel::Info, $scope, format!($($arg)*))
    };
}

#[macro_export]
macro_rules! log_warn {
    ($scope:expr, $($arg:tt)*) => {
        $crate::obs::logging::emit($crate::model::event::LogLevel::Warn, $scope, format!($($arg)*))
    };
}

#[macro_export]
macro_rules! log_error {
    ($scope:expr, $($arg:tt)*) => {
        $crate::obs::logging::emit($crate::model::event::LogLevel::Error, $scope, format!($($arg)*))
    };
}


/// The CLI's old behavior: `Log` goes verbatim to stderr.
///
/// Only `Log` — after the swap every `Error` event also carries a `Log` alongside it (`apply::record`
/// emits both), and printing both would double the output.
pub struct StderrSink {
    pub min_level: LogLevel,
}

impl ProgressSink for StderrSink {
    fn emit(&self, ev: ProgressEvent) {
        if let ProgressEvent::Log { level, message, .. } = &ev {
            if *level >= self.min_level {
                eprintln!("{message}");
            }
        }
    }
}

/// Fan one event out to several sinks (desktop: TauriSink + FileSink).
pub struct MultiSink(Vec<Arc<dyn ProgressSink>>);

impl MultiSink {
    pub fn new(sinks: Vec<Arc<dyn ProgressSink>>) -> MultiSink {
        MultiSink(sinks)
    }
}

impl ProgressSink for MultiSink {
    fn emit(&self, ev: ProgressEvent) {
        for s in &self.0 {
            s.emit(ev.clone());
        }
    }
}

/// Append writer that flushes every N lines.
///
/// A bare BufWriter loses the whole tail on Ctrl-C (the process is killed, Drop never runs); flushing
/// every line is too expensive — items.jsonl routinely runs to tens of thousands of lines. The
/// compromise: flush once `FLUSH_EVERY` lines have piled up, and force a flush on errors and phase boundaries.
struct Appender {
    w: std::io::BufWriter<std::fs::File>,
    since_flush: u32,
}

const FLUSH_EVERY: u32 = 64;

impl Appender {
    fn create(path: &Path) -> Option<Appender> {
        match std::fs::File::create(path) {
            Ok(f) => Some(Appender { w: std::io::BufWriter::new(f), since_flush: 0 }),
            Err(e) => {
                eprintln!("logging: cannot create {}: {e}", path.display());
                None
            }
        }
    }

    fn line(&mut self, s: &str, force_flush: bool) {
        if writeln!(self.w, "{s}").is_err() {
            return;
        }
        self.since_flush += 1;
        if force_flush || self.since_flush >= FLUSH_EVERY {
            let _ = self.w.flush();
            self.since_flush = 0;
        }
    }

    fn flush(&mut self) {
        let _ = self.w.flush();
        self.since_flush = 0;
    }
}

/// Three-way persistence for one run: run.jsonl (narration) / errors.jsonl (error detail) / items.jsonl (execution detail).
///
/// All IO is best-effort — following `runlog`'s standing rule: **logging must never fail a sync**.
/// Whichever file cannot be opened is simply the one that goes missing.
pub struct FileSink {
    run: Mutex<Option<Appender>>,
    errors: Mutex<Option<Appender>>,
    items: Mutex<Option<Appender>>,
    min_level: LogLevel,
}

impl FileSink {
    pub fn open(dir: &Path, min_level: LogLevel) -> FileSink {
        FileSink {
            run: Mutex::new(Appender::create(&dir.join(RUNLOG_RUN_FILE))),
            errors: Mutex::new(Appender::create(&dir.join(RUNLOG_ERRORS_FILE))),
            items: Mutex::new(Appender::create(&dir.join(RUNLOG_ITEMS_FILE))),
            min_level,
        }
    }

    fn put(slot: &Mutex<Option<Appender>>, line: &str, flush: bool) {
        if let Ok(mut g) = slot.lock() {
            if let Some(a) = g.as_mut() {
                a.line(line, flush);
            }
        }
    }

    pub fn flush_all(&self) {
        for s in [&self.run, &self.errors, &self.items] {
            if let Ok(mut g) = s.lock() {
                if let Some(a) = g.as_mut() {
                    a.flush();
                }
            }
        }
    }
}

impl ProgressSink for FileSink {
    fn emit(&self, ev: ProgressEvent) {
        // Progress is far too dense (one per file / per 1MiB chunk); persisting it would only drown the
        // narration. "Where are we" is for the UI, "what was done" is what gets archived — the latter is in ItemResult.
        if matches!(ev, ProgressEvent::Progress { .. }) {
            return;
        }
        if let ProgressEvent::Log { level, .. } = &ev {
            if *level < self.min_level {
                return;
            }
        }
        let Ok(line) = serde_json::to_string(&ev) else {
            return;
        };
        match &ev {
            // The execution detail goes only to items.jsonl: tens of thousands of lines mixed into run.jsonl would make the narration unreadable
            ProgressEvent::ItemResult { .. } => Self::put(&self.items, &line, false),
            ProgressEvent::Error { .. } => {
                Self::put(&self.run, &line, true);
                Self::put(&self.errors, &line, true);
            }
            ProgressEvent::Log { level, .. } => {
                let warn_up = *level >= LogLevel::Warn;
                Self::put(&self.run, &line, warn_up);
                if warn_up {
                    Self::put(&self.errors, &line, true);
                }
            }
            // Phase boundaries and terminal states flush everything while we are here — so Ctrl-C does not take the whole tail with it
            ProgressEvent::PhaseStart { .. } | ProgressEvent::Summary { .. } => {
                Self::put(&self.run, &line, true);
                self.flush_all();
            }
            _ => Self::put(&self.run, &line, false),
        }
    }
}

impl Drop for FileSink {
    fn drop(&mut self) {
        self.flush_all();
    }
}

/// App-level rolling log (events outside a run: startup, settings errors, prune, migration).
/// Single-file append, flushed per line — these events are sparse and all happen at moments where something may be about to go wrong.
pub struct AppLogSink {
    w: Mutex<Option<std::fs::File>>,
    min_level: LogLevel,
}

impl AppLogSink {
    pub fn open(dir: &Path, min_level: LogLevel) -> AppLogSink {
        let f = std::fs::OpenOptions::new().create(true).append(true).open(dir.join(APP_LOG_FILE)).ok();
        AppLogSink { w: Mutex::new(f), min_level }
    }
}

impl ProgressSink for AppLogSink {
    fn emit(&self, ev: ProgressEvent) {
        let ProgressEvent::Log { level, .. } = &ev else {
            return;
        };
        if *level < self.min_level {
            return;
        }
        let Ok(line) = serde_json::to_string(&ev) else {
            return;
        };
        if let Ok(mut g) = self.w.lock() {
            if let Some(f) = g.as_mut() {
                let _ = writeln!(f, "{line}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::event::ItemOutcome;

    /// Installing a sink uses a process-wide single slot, so install/uninstall tests must run serially
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn ev_log(level: LogLevel, msg: &str) -> ProgressEvent {
        ProgressEvent::Log { ts_ms: 1, level, scope: "t".into(), message: msg.into() }
    }

    fn collecting() -> (Arc<dyn ProgressSink>, Arc<Mutex<Vec<ProgressEvent>>>) {
        let store: Arc<Mutex<Vec<ProgressEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let s2 = store.clone();
        let sink = move |ev: ProgressEvent| s2.lock().unwrap().push(ev);
        (Arc::new(sink) as Arc<dyn ProgressSink>, store)
    }

    #[test]
    fn guard_restores_previous_sink() {
        let _l = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (a, sa) = collecting();
        let (b, sb) = collecting();
        {
            let _ga = crate::obs::progress::install(a);
            emit(LogLevel::Info, "t", "first".into());
            {
                let _gb = crate::obs::progress::install(b);
                emit(LogLevel::Info, "t", "inner".into());
            }
            // Once the inner guard lands it must restore a — otherwise the next run's log cross-contaminates the directory
            emit(LogLevel::Info, "t", "after".into());
        }
        assert_eq!(sa.lock().unwrap().len(), 2, "the outer sink should receive first + after");
        assert_eq!(sb.lock().unwrap().len(), 1, "the inner sink receives only inner");
        // Once every guard has landed we are back to "nobody holding the sink"
        assert!(current().is_none());
    }

    #[test]
    fn file_sink_routes_three_ways() {
        let dir = std::env::temp_dir().join(format!("syncdash-logtest-{}", crate::foundation::time::now_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        {
            let fs_sink = FileSink::open(&dir, LogLevel::Info);
            fs_sink.emit(ev_log(LogLevel::Info, "narrative"));
            fs_sink.emit(ev_log(LogLevel::Warn, "careful"));
            fs_sink.emit(ProgressEvent::Error {
                phase: crate::model::event::Phase::Apply,
                ts_ms: 1,
                path: "a.bin".into(),
                action: "Update".into(),
                side: "target".into(),
                message: "denied".into(),
            });
            fs_sink.emit(ProgressEvent::ItemResult {
                ts_ms: 1,
                path: "a.bin".into(),
                action: "Update".into(),
                side: "target".into(),
                outcome: ItemOutcome::Failed,
                bytes: 0,
                ms: 3,
            });
            // Progress is not persisted (too dense)
            fs_sink.emit(ProgressEvent::Progress {
                phase: crate::model::event::Phase::Apply,
                ts_ms: 1,
                items_done: 1,
                items_total: 2,
                bytes_done: 1,
                bytes_total: 2,
                current_path: "a.bin".into(),
            });
        }
        let read = |n: &str| std::fs::read_to_string(dir.join(n)).unwrap();
        let run = read(RUNLOG_RUN_FILE);
        let errors = read(RUNLOG_ERRORS_FILE);
        let items = read(RUNLOG_ITEMS_FILE);
        assert_eq!(run.lines().count(), 3, "run.jsonl = info + warn + error, no item/progress:\n{run}");
        assert!(!run.contains("\"progress\""), "Progress must not be persisted");
        assert_eq!(errors.lines().count(), 2, "errors.jsonl = warn + error:\n{errors}");
        assert!(!errors.contains("narrative"), "info does not go into the error detail");
        assert_eq!(items.lines().count(), 1, "items.jsonl takes only ItemResult:\n{items}");
        assert!(items.contains("\"failed\""));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_sink_respects_min_level() {
        let dir = std::env::temp_dir().join(format!("syncdash-logtest-lv-{}", crate::foundation::time::now_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        {
            let fs_sink = FileSink::open(&dir, LogLevel::Warn);
            fs_sink.emit(ev_log(LogLevel::Info, "chatter"));
            fs_sink.emit(ev_log(LogLevel::Error, "boom"));
        }
        let run = std::fs::read_to_string(dir.join(RUNLOG_RUN_FILE)).unwrap();
        assert_eq!(run.lines().count(), 1, "at level=warn, info is blocked:\n{run}");
        assert!(run.contains("boom"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn multi_sink_fans_out() {
        let (a, sa) = collecting();
        let (b, sb) = collecting();
        let m = MultiSink::new(vec![a, b]);
        m.emit(ev_log(LogLevel::Info, "x"));
        assert_eq!(sa.lock().unwrap().len(), 1);
        assert_eq!(sb.lock().unwrap().len(), 1);
    }

    #[test]
    fn level_ordering_is_info_warn_error() {
        assert!(LogLevel::Info < LogLevel::Warn && LogLevel::Warn < LogLevel::Error);
    }
}
