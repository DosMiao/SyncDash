//! v0.9 M1：全管线统一的 进度/取消/暂停 底座。
//!
//! 设计要点（详见 plans/ffs-ui 计划 §M1-API；行为参数对齐 FFS 14.10 progress_indicator.cpp）：
//! - 引擎只保证：单调计数器、每个事件带时间戳、协作检查点。速率(4s 窗)/ETA(60s 窗)/
//!   百分比 (bytesDone+itemsDone)/(bytesTotal+itemsTotal) 全是 UI 侧对事件流的算术。
//! - 节流归 sink（Tauri 侧 Progress 类 ≥100ms/条）；引擎在文件边界/1MiB 块边界自由发射，
//!   NullSink 情况下代价≈两次原子加。
//! - 取消走 `io::ErrorKind::Interrupted`——复用全链路既有 io::Result，零新错误类型。
//! - 暂停 = 100ms 小睡自旋：**栈帧存活 ⇒ RootLock 心跳线程继续跳**，对面机器不会把
//!   我们的锁判成遗弃（lock.rs 12s 判据）。这是不用"挂起返回"的硬理由。
//! - 与并行线 P2-6（scan_with_progress/ScanProgress）的关系：本模块是其超集；
//!   闭包 blanket impl 让 `Fn(ProgressEvent)` 直接当 sink 用，旧回调形态由 scan 侧桥接。

use serde::Serialize;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Phase {
    ScanSource,
    ScanTarget,
    Compare,
    Apply,
    Pack,
    Ship,
    Verify,
    /// apply 成功后的 archive 重扫——今天完全不可见的长阶段
    Refresh,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProgressEvent {
    /// 进入阶段。totals 为 0 = 尚未知。label = 人类语境（root 路径、ssh:host…）
    PhaseStart {
        phase: Phase,
        ts_ms: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        items_total: u64,
        bytes_total: u64,
    },
    /// 阶段中期精化总量（scan：walk 结束、哈希开始前）
    Totals { phase: Phase, ts_ms: u64, items_total: u64, bytes_total: u64 },
    /// 计数快照。文件边界/块边界都会发；sink 负责节流
    Progress {
        phase: Phase,
        ts_ms: u64,
        items_done: u64,
        items_total: u64,
        bytes_done: u64,
        bytes_total: u64,
        current_path: String,
    },
    /// 一条失败记录（错误绝不中断执行——FFS 累积语义；windowed 桌面构建靠它首次看见错误）
    Error {
        phase: Phase,
        ts_ms: u64,
        path: String,
        action: String,
        side: String,
        message: String,
    },
    Paused { ts_ms: u64 },
    Resumed { ts_ms: u64, paused_ms: u64 },
    /// apply 类运行的终态摘要
    Summary {
        ts_ms: u64,
        done: u64,
        skipped: u64,
        errors: u64,
        bytes_done: u64,
        elapsed_ms: u64,
        paused_ms: u64,
        cancelled: bool,
    },
}

pub trait ProgressSink: Send + Sync {
    fn emit(&self, ev: ProgressEvent);
}

pub struct NullSink;
impl ProgressSink for NullSink {
    fn emit(&self, _ev: ProgressEvent) {}
}

/// 任何 `Fn(ProgressEvent)+Send+Sync` 闭包都是 sink——并行线 P2-6 的闭包调用形态零成本兼容
impl<F: Fn(ProgressEvent) + Send + Sync> ProgressSink for F {
    fn emit(&self, ev: ProgressEvent) {
        self(ev)
    }
}

/// 协作式运行控制。Tauri/CLI 持有 Arc，引擎循环在检查点响应。
#[derive(Default)]
pub struct RunCtl {
    pub cancel: AtomicBool,
    pub paused: AtomicBool,
    paused_since_ms: AtomicU64,
    paused_total_ms: AtomicU64,
    /// N 个工作线程同时阻塞时，Paused/Resumed 只各发一次（CAS 去重）
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

/// 引擎函数需要的全部随身物。克隆廉价（两个 Arc）。
#[derive(Clone)]
pub struct RunCtx {
    pub ctl: Arc<RunCtl>,
    pub sink: Arc<dyn ProgressSink>,
}

impl RunCtx {
    /// 无 UI 场景：不取消、不暂停、事件进黑洞——旧签名薄壳全用它
    pub fn null() -> RunCtx {
        RunCtx { ctl: RunCtl::new(), sink: Arc::new(NullSink) }
    }
    pub fn new(ctl: Arc<RunCtl>, sink: Arc<dyn ProgressSink>) -> RunCtx {
        RunCtx { ctl, sink }
    }

    /// 协作点：取消 → Err(Interrupted)；暂停 → 100ms 小睡循环（Paused/Resumed CAS 去重发射）。
    /// PhaseProgress::checkpoint 委托到这里；远程管线的级间协作点（无计数器语境）直接用它。
    pub fn checkpoint(&self) -> std::io::Result<()> {
        let ctl = &self.ctl;
        if ctl.cancel.load(Ordering::Relaxed) {
            return Err(cancelled_err());
        }
        if ctl.paused.load(Ordering::Relaxed) {
            if !ctl.pause_announced.swap(true, Ordering::SeqCst) {
                ctl.paused_since_ms.store(crate::table::now_ms(), Ordering::SeqCst);
                self.sink.emit(ProgressEvent::Paused { ts_ms: crate::table::now_ms() });
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
                    let dur = crate::table::now_ms().saturating_sub(since);
                    ctl.paused_total_ms.fetch_add(dur, Ordering::SeqCst);
                }
                self.sink.emit(ProgressEvent::Resumed {
                    ts_ms: crate::table::now_ms(),
                    paused_ms: ctl.paused_total_ms.load(Ordering::SeqCst),
                });
            }
        }
        Ok(())
    }
}

/// apply 类运行的结果（旧元组接口走 into_tuple）
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

/// 单阶段计数器＋发射器。工作线程只借 &self（内部全原子）。
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
            ts_ms: crate::table::now_ms(),
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

    /// 相位内换挡（scan：walk 计数的是"发现"，哈希期改计"处理完"）——清零已完成条数
    pub fn restart_items(&self) {
        self.items_done.store(0, Ordering::Relaxed);
    }

    pub fn set_totals(&self, items: u64, bytes: u64) {
        self.items_total.store(items, Ordering::Relaxed);
        self.bytes_total.store(bytes, Ordering::Relaxed);
        self.ctx.sink.emit(ProgressEvent::Totals {
            phase: self.phase,
            ts_ms: crate::table::now_ms(),
            items_total: items,
            bytes_total: bytes,
        });
    }

    fn snapshot(&self, current: &str) -> ProgressEvent {
        ProgressEvent::Progress {
            phase: self.phase,
            ts_ms: crate::table::now_ms(),
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
            ts_ms: crate::table::now_ms(),
            path: path.to_string(),
            action: action.to_string(),
            side: side.to_string(),
            message: message.to_string(),
        });
    }

    /// 协作点：取消 → Err(Interrupted)；暂停 → 100ms 小睡循环（Paused/Resumed CAS 去重发射）。
    /// 放进每个 walk 迭代、每个待哈希文件、每个 1MiB 复制块之间。
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
