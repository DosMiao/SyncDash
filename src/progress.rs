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

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

/// 日志等级。与 `Error` 事件的分工：`Error` 是结构化的**单条 op 失败**
/// （带 path/action/side，天然就是报错清单的行），`Log` 是管线叙事
/// （远端探测结果、delta 降级、锁接管…）——库内那些 `eprintln!` 的去处。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "../typescript/core/types/generated/")]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Info,
    Warn,
    Error,
}

/// 一条 op 的实际结局——执行清单（items.jsonl）的核心字段。
/// 今天这个信息只活在 `apply::record` 的四个分支里，出了那个函数就没了。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "../typescript/core/types/generated/")]
#[serde(rename_all = "lowercase")]
pub enum ItemOutcome {
    Ok,
    /// 目录非空所以留着（保护过滤中的文件是对的，但必须留痕）
    Kept,
    /// 用户取消，这条没轮到
    Cancelled,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, ts_rs::TS)]
#[ts(export, export_to = "../typescript/core/types/generated/")]
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

#[derive(Clone, Debug, Serialize, ts_rs::TS)]
#[ts(export, export_to = "../typescript/core/types/generated/")]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProgressEvent {
    /// 进入阶段。totals 为 0 = 尚未知。label = 人类语境（root 路径、ssh:host…）
    PhaseStart {
        phase: Phase,
        #[ts(type = "number")]
        ts_ms: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[ts(type = "number")]
        items_total: u64,
        #[ts(type = "number")]
        bytes_total: u64,
    },
    /// 阶段中期精化总量（scan：walk 结束、哈希开始前）
    Totals {
        phase: Phase,
        #[ts(type = "number")]
        ts_ms: u64,
        #[ts(type = "number")]
        items_total: u64,
        #[ts(type = "number")]
        bytes_total: u64,
    },
    /// 计数快照。文件边界/块边界都会发；sink 负责节流
    Progress {
        phase: Phase,
        #[ts(type = "number")]
        ts_ms: u64,
        #[ts(type = "number")]
        items_done: u64,
        #[ts(type = "number")]
        items_total: u64,
        #[ts(type = "number")]
        bytes_done: u64,
        #[ts(type = "number")]
        bytes_total: u64,
        current_path: String,
    },
    /// 一条失败记录（错误绝不中断执行——FFS 累积语义；windowed 桌面构建靠它首次看见错误）
    Error {
        phase: Phase,
        #[ts(type = "number")]
        ts_ms: u64,
        path: String,
        action: String,
        side: String,
        message: String,
    },
    /// 管线叙事。`scope` = 模块名（run / pack / lock…），面板按它分组与筛选。
    /// windowed 桌面构建里 stderr 无处可去，这条是那些话唯一的出口。
    Log {
        #[ts(type = "number")]
        ts_ms: u64,
        level: LogLevel,
        scope: String,
        message: String,
    },
    /// 一条 op 的实际结局（执行清单的行）。与 `Progress` 的分工：
    /// Progress 是"到哪了"（会被节流丢帧），ItemResult 是"这条成没成"（一条都不能丢）。
    ItemResult {
        #[ts(type = "number")]
        ts_ms: u64,
        path: String,
        action: String,
        side: String,
        outcome: ItemOutcome,
        #[ts(type = "number")]
        bytes: u64,
        #[ts(type = "number")]
        ms: u64,
    },
    Paused {
        #[ts(type = "number")]
        ts_ms: u64,
    },
    Resumed {
        #[ts(type = "number")]
        ts_ms: u64,
        #[ts(type = "number")]
        paused_ms: u64,
    },
    /// apply 类运行的终态摘要
    Summary {
        #[ts(type = "number")]
        ts_ms: u64,
        #[ts(type = "number")]
        done: u64,
        #[ts(type = "number")]
        skipped: u64,
        #[ts(type = "number")]
        errors: u64,
        #[ts(type = "number")]
        bytes_done: u64,
        #[ts(type = "number")]
        elapsed_ms: u64,
        #[ts(type = "number")]
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

//
// 注册表存的是 `Arc<dyn ProgressSink>`，而 `ProgressSink` 是本模块的 trait——
// 所以它归这里。此前它住在 logging.rs，于是 `RunCtx::null()` 要回头
// `use crate::progress::current()`，而 logging 又 `use crate::progress::{...}`：
// 两个模块互相依赖，事件词汇表永远没法独立编译。挪过来之后 logging 单向向下。

type Slot = std::sync::RwLock<Option<Arc<dyn ProgressSink>>>;
static CURRENT: std::sync::OnceLock<Slot> = std::sync::OnceLock::new();

fn slot() -> &'static Slot {
    CURRENT.get_or_init(|| std::sync::RwLock::new(None))
}

/// 装上"当前运行"的 sink；guard 落地时自动摘除并还原上一个。
///
/// **必须是 RAII**：漏摘会让下一次运行的日志串进上一个运行的目录。
/// 桌面有 `RunState.active` 单运行互斥、CLI `run --all` 顺序执行，
/// 所以进程级单槽本身是安全的。
#[must_use = "guard 一落地 sink 就被摘除——必须绑到运行的生命周期上"]
pub struct SinkGuard {
    prev: Option<Arc<dyn ProgressSink>>,
}

/// 当前接管者（若有）。`runlog::Recorder` 用它把**已有的**去处串进自己的
/// MultiSink——运行期的文件捕获是**叠加**，不是替换：CLI 在进程启动装的
/// StderrSink 必须在 apply 期间继续说话。
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
    /// 无 UI 场景：不取消、不暂停——旧签名薄壳全用它。
    ///
    /// 事件进黑洞，但**诊断不进**：sink 取进程当前的环境去处（CLI 启动装的
    /// StderrSink），没装才退回 NullSink。"没有界面"不等于"不用说话"——
    /// 之前这里硬写 NullSink，CLI 比对期的挂载点警告就是这么丢的。
    pub fn null() -> RunCtx {
        RunCtx { ctl: RunCtl::new(), sink: current().unwrap_or_else(|| Arc::new(NullSink)) }
    }
    pub fn new(ctl: Arc<RunCtl>, sink: Arc<dyn ProgressSink>) -> RunCtx {
        RunCtx { ctl, sink }
    }

    /// 发一条管线叙事。拿得到 ctx 的地方直接用它；拿不到的（trash / version / lock）
    /// 走 `logging::log_*!` 宏经进程级注册表落到同一条总线。
    pub fn log(&self, level: LogLevel, scope: &str, message: impl Into<String>) {
        self.sink.emit(ProgressEvent::Log {
            ts_ms: crate::foundation::time::now_ms(),
            level,
            scope: scope.to_string(),
            message: message.into(),
        });
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

    /// 相位内换挡（scan：walk 计数的是"发现"，哈希期改计"处理完"）——清零已完成条数
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
