//! v0.9 M1: the pipeline-wide progress/cancel/pause substrate.
//!
//! Design notes (see the plans/ffs-ui plan §M1-API; behavior parameters match FFS 14.10 progress_indicator.cpp):
//! - The engine guarantees only: monotone counters, a timestamp on every event, cooperative
//!   checkpoints. Rate (4s window) / ETA (60s window) / percentage
//!   (bytesDone+itemsDone)/(bytesTotal+itemsTotal) are all UI-side arithmetic over the event stream.
//! - Throttling belongs to the sink (the Tauri-side Progress class: ≥100ms per event); the engine
//!   emits freely at file and 1MiB chunk boundaries, costing ≈two atomic adds under NullSink.
//! - Cancel rides `io::ErrorKind::Interrupted` — it reuses the io::Result already threaded end to end, zero new error types.
//! - Pause = a 100ms nap spin: **the stack frame stays alive ⇒ the RootLock heartbeat thread keeps
//!   beating**, so the far machine never judges our lock abandoned (lock.rs 12s criterion). That is
//!   the hard reason for not "returning suspended".
//! - Relation to the parallel line P2-6 (scan_with_progress/ScanProgress): this module is a superset;
//!   the blanket closure impl lets `Fn(ProgressEvent)` serve as a sink directly, and the scan side bridges the old callback shape.

use crate::model::event::{LogLevel, Phase, ProgressEvent};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;


pub trait ProgressSink: Send + Sync {
    fn emit(&self, ev: ProgressEvent);
}

pub struct NullSink;
impl ProgressSink for NullSink {
    fn emit(&self, _ev: ProgressEvent) {}
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
    items_done: AtomicU64,
    items_total: AtomicU64,
    bytes_done: AtomicU64,
    bytes_total: AtomicU64,
}

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
            items_done: AtomicU64::new(0),
            items_total: AtomicU64::new(items_total),
            bytes_done: AtomicU64::new(0),
            bytes_total: AtomicU64::new(bytes_total),
        }
    }

    /// Shift gears within a phase (scan: the walk counts "discovered", hashing switches to "processed") — resets the done count
    pub fn restart_items(&self) {
        self.items_done.store(0, Ordering::Relaxed);
    }

    pub fn set_totals(&self, items: u64, bytes: u64) {
        self.items_total.store(items, Ordering::Relaxed);
        self.bytes_total.store(bytes, Ordering::Relaxed);
        self.ctx.sink.emit(ProgressEvent::Totals {
            phase: self.phase,
            ts_ms: crate::foundation::time::now_ms(),
            items_total: items,
            bytes_total: bytes,
        });
    }

    fn snapshot(&self, current: &str) -> ProgressEvent {
        ProgressEvent::Progress {
            phase: self.phase,
            ts_ms: crate::foundation::time::now_ms(),
            items_done: self.items_done.load(Ordering::Relaxed),
            items_total: self.items_total.load(Ordering::Relaxed),
            bytes_done: self.bytes_done.load(Ordering::Relaxed),
            bytes_total: self.bytes_total.load(Ordering::Relaxed),
            current_path: current.to_string(),
        }
    }

    pub fn item_done(&self, current: &str) {
        self.items_done.fetch_add(1, Ordering::Relaxed);
        self.ctx.sink.emit(self.snapshot(current));
    }

    pub fn add_bytes(&self, n: u64, current: &str) {
        self.bytes_done.fetch_add(n, Ordering::Relaxed);
        self.ctx.sink.emit(self.snapshot(current));
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
        let evs = store.lock().unwrap();
        assert!(matches!(evs[0], ProgressEvent::PhaseStart { phase: Phase::ScanSource, .. }));
        assert!(matches!(evs[1], ProgressEvent::Totals { items_total: 2, bytes_total: 300, .. }));
        assert_eq!(evs.iter().filter(|e| matches!(e, ProgressEvent::Progress { .. })).count(), 4);
        assert!(evs.iter().any(|e| matches!(e, ProgressEvent::Error { .. })));
        let json = serde_json::to_string(&evs[1]).unwrap();
        assert!(json.contains("\"kind\":\"totals\"") && json.contains("\"scan-source\""), "serde shape: {json}");
    }
}
