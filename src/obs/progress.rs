//! v0.9 M1: the pipeline-wide progress/cancel/pause substrate.
//!
//! Design notes (see the plans/ffs-ui plan §M1-API; behavior parameters match FFS 14.10 progress_indicator.cpp):
//! - The engine guarantees only: monotone counters, a timestamp on every event, cooperative
//!   checkpoints. Rate (4s window) / ETA (60s window) / percentage
//!   (bytesDone+itemsDone)/(bytesTotal+itemsTotal) are all UI-side arithmetic over the event stream.
//! - Counters update at every file and chunk boundary, while progress snapshots are sampled once
//!   per phase every 100ms. Terminal and total events remain exact and unthrottled.
//! - Cancel rides `io::ErrorKind::Interrupted` — it reuses the io::Result already threaded end to end, zero new error types.
//! - Pause = a 100ms nap spin: **the stack frame stays alive ⇒ the RootLock heartbeat thread keeps
//!   beating**, so the far machine never judges our lock abandoned (lock.rs 12s criterion). That is
//!   the hard reason for not "returning suspended".
//! - Relation to the parallel line P2-6 (scan_with_progress/ScanProgress): this module is a superset;
//!   the blanket closure impl lets `Fn(ProgressEvent)` serve as a sink directly, and the scan side bridges the old callback shape.

use crate::model::event::{LogLevel, Phase, PhaseStatus, ProgressEvent};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;


pub trait ProgressSink: Send + Sync {
    fn emit(&self, ev: ProgressEvent);

    fn wants_progress(&self) -> bool {
        true
    }
}

pub struct NullSink;
impl ProgressSink for NullSink {
    fn emit(&self, _ev: ProgressEvent) {}

    fn wants_progress(&self) -> bool {
        false
    }
}

/// Any `Fn(ProgressEvent)+Send+Sync` closure is a sink — zero-cost compatibility with the closure call shape of the parallel line P2-6
impl<F: Fn(ProgressEvent) + Send + Sync> ProgressSink for F {
    fn emit(&self, ev: ProgressEvent) {
        self(ev)
    }
}

//
// The registry stores `Arc<dyn ProgressSink>`, and `ProgressSink` is this module's trait —
// so it belongs here. It used to live in logging.rs, which forced `RunCtx::null()` to reach back for
// `use crate::obs::progress::current()` while logging did `use crate::obs::progress::{...}`: two mutually
// dependent modules, and the event vocabulary could never compile alone. After the move logging points only downward.

type Slot = std::sync::RwLock<Option<Arc<dyn ProgressSink>>>;
static CURRENT: std::sync::OnceLock<Slot> = std::sync::OnceLock::new();

fn slot() -> &'static Slot {
    CURRENT.get_or_init(|| std::sync::RwLock::new(None))
}

/// Install the "current run" sink; when the guard lands the sink is removed and the previous one restored.
///
/// **Must be RAII**: leaking the guard cross-contaminates the next run's log directory.
/// The desktop has `RunState.active` single-run mutual exclusion and the CLI runs `run --all`
/// sequentially, so a process-wide single slot is safe in itself.
#[must_use = "the sink is removed the moment the guard lands — bind it to the run's lifetime"]
pub struct SinkGuard {
    prev: Option<Arc<dyn ProgressSink>>,
}

/// The current sink, if any. `runlog::Recorder` uses it to thread the **existing** sink into its own
/// MultiSink — capturing to file during a run is **layered on top of**, not a replacement for, what
/// is already there: the StderrSink the CLI installs at process start must keep talking during apply.
pub fn current() -> Option<Arc<dyn ProgressSink>> {
    slot().read().unwrap_or_else(|e| e.into_inner()).clone()
}

pub fn install(sink: Arc<dyn ProgressSink>) -> SinkGuard {
    let mut g = slot().write().unwrap_or_else(|e| e.into_inner());
    let prev = g.take();
    *g = Some(sink);
    SinkGuard { prev }
}

impl Drop for SinkGuard {
    fn drop(&mut self) {
        let mut g = slot().write().unwrap_or_else(|e| e.into_inner());
        *g = self.prev.take();
    }
}

/// Cooperative run control. Tauri/CLI hold the Arc; engine loops respond at checkpoints.
#[derive(Default)]
pub struct RunCtl {
    pub cancel: AtomicBool,
    pub paused: AtomicBool,
    paused_since_ms: AtomicU64,
    paused_total_ms: AtomicU64,
    /// With N worker threads blocked at once, Paused/Resumed are each emitted exactly once (CAS dedup)
    pause_announced: AtomicBool,
}

impl RunCtl {
    pub fn new() -> Arc<RunCtl> {
        Arc::new(RunCtl::default())
    }
    pub fn request_cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }
    pub fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::SeqCst);
    }
    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
    pub fn paused_total_ms(&self) -> u64 {
        self.paused_total_ms.load(Ordering::SeqCst)
    }
}

pub fn cancelled_err() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::Interrupted, "cancelled by user")
}

pub fn is_cancelled(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::Interrupted
}

/// Everything an engine function needs to carry around. Cloning is cheap (two Arcs).
#[derive(Clone)]
pub struct RunCtx {
    pub ctl: Arc<RunCtl>,
    pub sink: Arc<dyn ProgressSink>,
}

impl RunCtx {
    /// No-UI case: no cancel, no pause — every thin shim over an old signature uses this.
    ///
    /// Events go into the void, but **diagnostics do not**: the sink picks up the process's ambient
    /// sink (the StderrSink the CLI installs at startup) and only falls back to NullSink when none is
    /// installed. "No interface" does not mean "no need to talk" — this used to hard-code NullSink,
    /// which is exactly how the mount-point warning during a CLI compare got lost.
    pub fn null() -> RunCtx {
        RunCtx { ctl: RunCtl::new(), sink: current().unwrap_or_else(|| Arc::new(NullSink)) }
    }
    pub fn new(ctl: Arc<RunCtl>, sink: Arc<dyn ProgressSink>) -> RunCtx {
        RunCtx { ctl, sink }
    }

    /// Emit one line of pipeline narration. Use this wherever ctx is in hand; where it is not
    /// (trash / version / lock), the `logging::log_*!` macros reach the same bus via the process registry.
    pub fn log(&self, level: LogLevel, scope: &str, message: impl Into<String>) {
        self.sink.emit(ProgressEvent::Log {
            ts_ms: crate::foundation::time::now_ms(),
            level,
            scope: scope.to_string(),
            message: message.into(),
        });
    }

    /// Cooperation point: cancel → Err(Interrupted); pause → a 100ms nap loop (Paused/Resumed emitted once each, CAS-deduped).
    /// PhaseProgress::checkpoint delegates here; the remote pipeline's between-stage cooperation points (no counter context) use it directly.
    pub fn checkpoint(&self) -> std::io::Result<()> {
        let ctl = &self.ctl;
        if ctl.cancel.load(Ordering::Relaxed) {
            return Err(cancelled_err());
        }
        if ctl.paused.load(Ordering::Relaxed) {
            if !ctl.pause_announced.swap(true, Ordering::SeqCst) {
                ctl.paused_since_ms.store(crate::foundation::time::now_ms(), Ordering::SeqCst);
                self.sink.emit(ProgressEvent::Paused { ts_ms: crate::foundation::time::now_ms() });
            }
            while ctl.paused.load(Ordering::Relaxed) {
                if ctl.cancel.load(Ordering::Relaxed) {
                    return Err(cancelled_err());
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            if ctl.pause_announced.swap(false, Ordering::SeqCst) {
                let since = ctl.paused_since_ms.swap(0, Ordering::SeqCst);
                if since > 0 {
                    let dur = crate::foundation::time::now_ms().saturating_sub(since);
                    ctl.paused_total_ms.fetch_add(dur, Ordering::SeqCst);
                }
                self.sink.emit(ProgressEvent::Resumed {
                    ts_ms: crate::foundation::time::now_ms(),
                    paused_ms: ctl.paused_total_ms.load(Ordering::SeqCst),
                });
            }
        }
        Ok(())
    }
}

/// Result of an apply-class run (the old tuple interface goes through into_tuple)
#[derive(Clone, Copy, Debug, Default)]
pub struct ApplyOutcome {
    pub done: u64,
    pub skipped: u64,
    pub errors: u64,
    pub bytes_copied: u64,
    pub cancelled: bool,
}

impl ApplyOutcome {
    pub fn into_tuple(self) -> (u64, u64, u64) {
        (self.done, self.skipped, self.errors)
    }
}

/// Single-phase counters plus emitter. Worker threads only borrow &self (everything inside is atomic).
pub struct PhaseProgress<'a> {
    ctx: &'a RunCtx,
    phase: Phase,
    ended: bool,
    items_done: AtomicU64,
    items_total: AtomicU64,
    bytes_done: AtomicU64,
    bytes_total: AtomicU64,
    last_progress_ms: AtomicU64,
}

const PROGRESS_INTERVAL_MS: u64 = 100;

impl<'a> PhaseProgress<'a> {
    pub fn begin(ctx: &'a RunCtx, phase: Phase, label: Option<String>, items_total: u64, bytes_total: u64) -> Self {
        ctx.sink.emit(ProgressEvent::PhaseStart {
            phase,
            ts_ms: crate::foundation::time::now_ms(),
            label,
            items_total,
            bytes_total,
        });
        PhaseProgress {
            ctx,
            phase,
            ended: false,
            items_done: AtomicU64::new(0),
            items_total: AtomicU64::new(items_total),
            bytes_done: AtomicU64::new(0),
            bytes_total: AtomicU64::new(bytes_total),
            last_progress_ms: AtomicU64::new(0),
        }
    }

    /// Shift gears atomically within a phase: scan discovery counts found files, then hashing
    /// starts a new processed-file epoch with exact totals.
    pub fn restart_items_with_totals(&self, items: u64, bytes: u64) {
        self.items_done.store(0, Ordering::Relaxed);
        self.items_total.store(items, Ordering::Relaxed);
        self.bytes_total.store(bytes, Ordering::Relaxed);
        self.emit_totals(true);
    }

    pub fn set_totals(&self, items: u64, bytes: u64) {
        self.items_total.store(items, Ordering::Relaxed);
        self.bytes_total.store(bytes, Ordering::Relaxed);
        self.emit_totals(false);
    }

    fn snapshot(&self, current: &str, ts_ms: u64) -> ProgressEvent {
        ProgressEvent::Progress {
            phase: self.phase,
            ts_ms,
            items_done: self.items_done.load(Ordering::Relaxed),
            items_total: self.items_total.load(Ordering::Relaxed),
            bytes_done: self.bytes_done.load(Ordering::Relaxed),
            bytes_total: self.bytes_total.load(Ordering::Relaxed),
            current_path: current.to_string(),
        }
    }

    fn emit_progress(&self, current: &str) {
        if !self.ctx.sink.wants_progress() {
            return;
        }
        let now = crate::foundation::time::now_ms();
        let mut last = self.last_progress_ms.load(Ordering::Relaxed);
        loop {
            if last != 0 && now.saturating_sub(last) < PROGRESS_INTERVAL_MS {
                return;
            }
            match self.last_progress_ms.compare_exchange_weak(last, now, Ordering::Relaxed, Ordering::Relaxed) {
                Ok(_) => {
                    self.ctx.sink.emit(self.snapshot(current, now));
                    return;
                }
                Err(observed) => last = observed,
            }
        }
    }

    pub fn item_done(&self, current: &str) {
        self.items_done.fetch_add(1, Ordering::Relaxed);
        self.emit_progress(current);
    }

    pub fn add_bytes(&self, n: u64, current: &str) {
        self.bytes_done.fetch_add(n, Ordering::Relaxed);
        self.emit_progress(current);
    }

    /// Add work discovered only while applying (for example an external-volume trash copy).
    /// The Totals snapshot carries the current done counters, so changing the denominator is
    /// atomic for consumers and never fabricates a counter reset.
    pub fn add_total_bytes(&self, n: u64) {
        if n == 0 {
            return;
        }
        self.bytes_total.fetch_add(n, Ordering::Relaxed);
        self.emit_totals(false);
    }

    /// Replace one operation's planned byte contribution with the source size observed at apply
    /// time. A source may have changed since compare, and missing plan sizes contribute zero.
    pub fn revise_total_bytes(&self, planned: u64, actual: u64) {
        if planned == actual {
            return;
        }
        if actual > planned {
            self.bytes_total.fetch_add(actual - planned, Ordering::Relaxed);
        } else {
            let decrease = planned - actual;
            let _ = self.bytes_total.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |total| {
                Some(total.saturating_sub(decrease))
            });
        }
        self.emit_totals(false);
    }

    /// Mark an opaque successful stage complete when its worker only returns aggregate totals
    /// (peer pack/apply). Ordinary file/chunk loops must keep using item_done/add_bytes.
    pub fn complete(&self, current: &str) {
        self.items_done.store(self.items_total.load(Ordering::Relaxed), Ordering::Relaxed);
        self.bytes_done.store(self.bytes_total.load(Ordering::Relaxed), Ordering::Relaxed);
        let now = crate::foundation::time::now_ms();
        if self.ctx.sink.wants_progress() {
            self.ctx.sink.emit(self.snapshot(current, now));
        }
    }

    pub fn error(&self, path: &str, action: &str, side: &str, message: &str) {
        self.ctx.sink.emit(ProgressEvent::Error {
            phase: self.phase,
            ts_ms: crate::foundation::time::now_ms(),
            path: path.to_string(),
            action: action.to_string(),
            side: side.to_string(),
            message: message.to_string(),
        });
    }

    /// Cooperation point: cancel → Err(Interrupted); pause → a 100ms nap loop (Paused/Resumed emitted once each, CAS-deduped).
    /// Drop it between every walk iteration, every file about to be hashed, every 1MiB copy chunk.
    pub fn checkpoint(&self) -> std::io::Result<()> {
        self.ctx.checkpoint()
    }

    /// (items_done, items_total, bytes_done, bytes_total)
    pub fn counts(&self) -> (u64, u64, u64, u64) {
        (
            self.items_done.load(Ordering::Relaxed),
            self.items_total.load(Ordering::Relaxed),
            self.bytes_done.load(Ordering::Relaxed),
            self.bytes_total.load(Ordering::Relaxed),
        )
    }

    fn emit_end(&self, status: PhaseStatus) {
        let (items_done, items_total, bytes_done, bytes_total) = self.counts();
        self.ctx.sink.emit(ProgressEvent::PhaseEnd {
            phase: self.phase,
            status,
            ts_ms: crate::foundation::time::now_ms(),
            items_done,
            items_total,
            bytes_done,
            bytes_total,
        });
    }

    /// Emit one authoritative, unthrottled boundary. Consuming self makes it impossible for a call
    /// site to announce the same phase end twice; Drop covers early `?` exits as failed/cancelled.
    pub fn finish(mut self) -> std::io::Result<()> {
        self.ended = true;
        let cancelled = self.ctx.ctl.cancelled();
        let status = if cancelled { PhaseStatus::Cancelled } else { PhaseStatus::Completed };
        self.emit_end(status);
        if cancelled { Err(cancelled_err()) } else { Ok(()) }
    }

    /// End a phase that returned a handled failure rather than unwinding through `?`.
    pub fn fail(mut self) {
        self.ended = true;
        self.emit_end(PhaseStatus::Failed);
    }

    fn emit_totals(&self, reset: bool) {
        self.ctx.sink.emit(ProgressEvent::Totals {
            phase: self.phase,
            reset,
            ts_ms: crate::foundation::time::now_ms(),
            items_done: self.items_done.load(Ordering::Relaxed),
            items_total: self.items_total.load(Ordering::Relaxed),
            bytes_done: self.bytes_done.load(Ordering::Relaxed),
            bytes_total: self.bytes_total.load(Ordering::Relaxed),
        });
    }
}

impl Drop for PhaseProgress<'_> {
    fn drop(&mut self) {
        if !self.ended {
            let status = if self.ctx.ctl.cancelled() { PhaseStatus::Cancelled } else { PhaseStatus::Failed };
            self.emit_end(status);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn collecting_ctx() -> (RunCtx, Arc<Mutex<Vec<ProgressEvent>>>) {
        let store: Arc<Mutex<Vec<ProgressEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let s2 = store.clone();
        let sink = move |ev: ProgressEvent| {
            s2.lock().unwrap().push(ev);
        };
        (RunCtx::new(RunCtl::new(), Arc::new(sink)), store)
    }

    #[test]
    fn cancel_makes_checkpoint_interrupt() {
        let (ctx, _) = collecting_ctx();
        let prog = PhaseProgress::begin(&ctx, Phase::Apply, None, 10, 100);
        assert!(prog.checkpoint().is_ok());
        ctx.ctl.request_cancel();
        let err = prog.checkpoint().unwrap_err();
        assert!(is_cancelled(&err));
    }

    #[test]
    fn pause_blocks_and_accumulates() {
        let (ctx, store) = collecting_ctx();
        ctx.ctl.set_paused(true);
        let ctx2 = ctx.clone();
        let counter = Arc::new(AtomicU64::new(0));
        let c2 = counter.clone();
        let h = std::thread::spawn(move || {
            let prog = PhaseProgress::begin(&ctx2, Phase::Apply, None, 0, 0);
            prog.checkpoint().unwrap();
            c2.store(1, Ordering::SeqCst);
        });
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert_eq!(counter.load(Ordering::SeqCst), 0, "checkpoint must block while paused");
        ctx.ctl.set_paused(false);
        h.join().unwrap();
        assert_eq!(counter.load(Ordering::SeqCst), 1);
        assert!(ctx.ctl.paused_total_ms() >= 200, "paused_ms should accumulate, got {}", ctx.ctl.paused_total_ms());
        let evs = store.lock().unwrap();
        let paused = evs.iter().filter(|e| matches!(e, ProgressEvent::Paused { .. })).count();
        let resumed = evs.iter().filter(|e| matches!(e, ProgressEvent::Resumed { .. })).count();
        assert_eq!((paused, resumed), (1, 1));
    }

    #[test]
    fn concurrent_pause_announces_once() {
        let (ctx, store) = collecting_ctx();
        ctx.ctl.set_paused(true);
        let mut handles = Vec::new();
        for _ in 0..4 {
            let ctx2 = ctx.clone();
            handles.push(std::thread::spawn(move || {
                let prog = PhaseProgress::begin(&ctx2, Phase::Apply, None, 0, 0);
                prog.checkpoint().unwrap();
            }));
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
        ctx.ctl.set_paused(false);
        for h in handles {
            h.join().unwrap();
        }
        let evs = store.lock().unwrap();
        let paused = evs.iter().filter(|e| matches!(e, ProgressEvent::Paused { .. })).count();
        let resumed = evs.iter().filter(|e| matches!(e, ProgressEvent::Resumed { .. })).count();
        assert_eq!((paused, resumed), (1, 1), "4 blocked threads must announce exactly one pair");
    }

    #[test]
    fn counters_and_events_flow() {
        let (ctx, store) = collecting_ctx();
        let prog = PhaseProgress::begin(&ctx, Phase::ScanSource, Some("D:\\root".into()), 0, 0);
        prog.set_totals(2, 300);
        prog.item_done("a.txt");
        prog.add_bytes(100, "a.txt");
        prog.item_done("b.txt");
        prog.add_bytes(200, "b.txt");
        prog.error("b.txt", "hash", "source", "boom");
        assert_eq!(prog.counts(), (2, 2, 300, 300));
        prog.finish().unwrap();
        let evs = store.lock().unwrap();
        assert!(matches!(evs[0], ProgressEvent::PhaseStart { phase: Phase::ScanSource, .. }));
        assert!(matches!(evs[1], ProgressEvent::Totals {
            reset: false,
            items_done: 0,
            items_total: 2,
            bytes_done: 0,
            bytes_total: 300,
            ..
        }));
        assert_eq!(evs.iter().filter(|e| matches!(e, ProgressEvent::Progress { .. })).count(), 1);
        assert!(evs.iter().any(|e| matches!(e, ProgressEvent::Error { .. })));
        assert!(matches!(evs.last(), Some(ProgressEvent::PhaseEnd {
            phase: Phase::ScanSource,
            status: PhaseStatus::Completed,
            items_done: 2,
            items_total: 2,
            bytes_done: 300,
            bytes_total: 300,
            ..
        })));
        let json = serde_json::to_string(&evs[1]).unwrap();
        assert!(json.contains("\"kind\":\"totals\"") && json.contains("\"scan-source\""), "serde shape: {json}");
    }

    #[test]
    fn an_unfinished_phase_emits_a_failed_boundary_on_drop() {
        let (ctx, store) = collecting_ctx();
        {
            let prog = PhaseProgress::begin(&ctx, Phase::ScanTarget, None, 10, 100);
            prog.item_done("one");
        }
        assert!(matches!(store.lock().unwrap().last(), Some(ProgressEvent::PhaseEnd {
            phase: Phase::ScanTarget,
            status: PhaseStatus::Failed,
            items_done: 1,
            ..
        })));
    }

    #[test]
    fn a_cancelled_finish_propagates_interruption() {
        let (ctx, store) = collecting_ctx();
        let prog = PhaseProgress::begin(&ctx, Phase::ScanSource, None, 1, 0);
        ctx.ctl.request_cancel();

        let err = prog.finish().unwrap_err();
        assert!(is_cancelled(&err));
        assert!(matches!(store.lock().unwrap().last(), Some(ProgressEvent::PhaseEnd {
            phase: Phase::ScanSource,
            status: PhaseStatus::Cancelled,
            ..
        })));
    }

    #[test]
    fn a_sink_that_does_not_consume_progress_still_receives_exact_boundaries() {
        struct BoundarySink(Arc<Mutex<Vec<ProgressEvent>>>);
        impl ProgressSink for BoundarySink {
            fn emit(&self, ev: ProgressEvent) {
                self.0.lock().unwrap().push(ev);
            }

            fn wants_progress(&self) -> bool {
                false
            }
        }

        let events = Arc::new(Mutex::new(Vec::new()));
        let ctx = RunCtx::new(RunCtl::new(), Arc::new(BoundarySink(events.clone())));
        let progress = PhaseProgress::begin(&ctx, Phase::ScanTarget, None, 2, 300);
        progress.item_done("a.txt");
        progress.add_bytes(100, "a.txt");
        progress.item_done("b.txt");
        progress.add_bytes(200, "b.txt");
        progress.finish().unwrap();

        let events = events.lock().unwrap();
        assert!(!events.iter().any(|event| matches!(event, ProgressEvent::Progress { .. })));
        assert!(matches!(events.last(), Some(ProgressEvent::PhaseEnd {
            items_done: 2,
            bytes_done: 300,
            status: PhaseStatus::Completed,
            ..
        })));
    }
}
