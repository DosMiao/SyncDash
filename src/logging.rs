//! v0.10：集中日志。
//!
//! **为什么不引 tracing/log**：项目里已经有一条贯穿全管线的事件总线
//! （`progress::ProgressSink`），`RunCtx` 已经传到每个引擎函数，`runlog::ErrCollector`
//! 也已经示范过 sink 装饰器写法。再叠一套 facade 只会让同一件事有两个出口。
//! 本模块做的事只有一件：把旁路的 `eprintln!` 并进那条已有的总线。
//!
//! **为什么需要宏 + 进程级注册表**：`trash.rs` / `version.rs` / `lock.rs` 里的调用点
//! 拿不到 `RunCtx`，改签名会波及几十个函数。注册表让它们零签名改动进总线；
//! 拿得到 ctx 的地方仍然直接 `ctx.log()`，不绕这一圈。
//!
//! **没装 sink 时按原文打 stderr** —— 这样"改造前后 CLI 输出逐字节相同"成立，
//! 换线（P3）的正确性不依赖桌面壳或 CLI 有没有把 sink 装上。

use crate::progress::{current, LogLevel, ProgressEvent, ProgressSink};
use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex};

/// 宏的落点。没人接管时按原文进 stderr——即改造前的行为。
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

/// `log_info!("run", "远端 {host}: {os}")` —— 参数与 `format!` 一致。
/// 消息里保留调用点原有的前缀（`[job] warning: …`），换线才不改变 CLI 输出的一个字节。
#[macro_export]
macro_rules! log_info {
    ($scope:expr, $($arg:tt)*) => {
        $crate::logging::emit($crate::progress::LogLevel::Info, $scope, format!($($arg)*))
    };
}

#[macro_export]
macro_rules! log_warn {
    ($scope:expr, $($arg:tt)*) => {
        $crate::logging::emit($crate::progress::LogLevel::Warn, $scope, format!($($arg)*))
    };
}

#[macro_export]
macro_rules! log_error {
    ($scope:expr, $($arg:tt)*) => {
        $crate::logging::emit($crate::progress::LogLevel::Error, $scope, format!($($arg)*))
    };
}


/// CLI 的老行为：`Log` 按原文进 stderr。
///
/// 只认 `Log`——`Error` 事件在换线后各自也会带一条 `Log`（`apply::record` 里
/// 两者都发），两边都打会让输出翻倍。
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

/// 把一个事件分发给多个 sink（桌面：TauriSink + FileSink）。
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

/// 每 N 行 flush 一次的追加写。
///
/// BufWriter 直接用会让 Ctrl-C 丢掉整条尾巴（进程被杀，Drop 不跑）；每行都 flush
/// 又太贵——items.jsonl 动辄上万行。折中：攒够 `FLUSH_EVERY` 行落一次，
/// 报错与阶段边界强制落。
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
                eprintln!("logging: 建不出 {}：{e}", path.display());
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

/// 一次运行的三路落盘：run.jsonl（叙事）/ errors.jsonl（报错清单）/ items.jsonl（执行清单）。
///
/// 所有 IO best-effort——沿用 `runlog` 的老规矩：**日志绝不让同步失败**。
/// 哪一份开不出来就只是那一份缺席。
pub struct FileSink {
    run: Mutex<Option<Appender>>,
    errors: Mutex<Option<Appender>>,
    items: Mutex<Option<Appender>>,
    min_level: LogLevel,
}

impl FileSink {
    pub const RUN: &'static str = "run.jsonl";
    pub const ERRORS: &'static str = "errors.jsonl";
    pub const ITEMS: &'static str = "items.jsonl";

    pub fn open(dir: &Path, min_level: LogLevel) -> FileSink {
        FileSink {
            run: Mutex::new(Appender::create(&dir.join(Self::RUN))),
            errors: Mutex::new(Appender::create(&dir.join(Self::ERRORS))),
            items: Mutex::new(Appender::create(&dir.join(Self::ITEMS))),
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
        // Progress 太密（每文件 / 每 1MiB 块一条），落盘只会把叙事淹掉。
        // "到哪了"是给界面看的，"做了什么"才归档——后者在 ItemResult 里。
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
            // 执行清单只进 items.jsonl：上万行混进 run.jsonl 会让叙事没法读
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
            // 阶段边界与终态顺手全落盘——Ctrl-C 时尾巴才不至于整段丢
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

/// app 级滚动日志（运行之外的事件：启动、设置错误、prune、迁移）。
/// 单文件追加，每条都 flush——这些事件稀疏且都发生在"可能马上就要出事"的时刻。
pub struct AppLogSink {
    w: Mutex<Option<std::fs::File>>,
    min_level: LogLevel,
}

impl AppLogSink {
    pub const FILE: &'static str = "app.jsonl";

    pub fn open(dir: &Path, min_level: LogLevel) -> AppLogSink {
        let f = std::fs::OpenOptions::new().create(true).append(true).open(dir.join(Self::FILE)).ok();
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
    use crate::progress::ItemOutcome;

    /// 装 sink 用的是进程级单槽，装卸类测试必须串行跑
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
            let _ga = crate::progress::install(a);
            emit(LogLevel::Info, "t", "first".into());
            {
                let _gb = crate::progress::install(b);
                emit(LogLevel::Info, "t", "inner".into());
            }
            // 内层 guard 落地后必须还原到 a——否则下一次运行的日志会串目录
            emit(LogLevel::Info, "t", "after".into());
        }
        assert_eq!(sa.lock().unwrap().len(), 2, "外层 sink 应收到 first + after");
        assert_eq!(sb.lock().unwrap().len(), 1, "内层 sink 只收到 inner");
        // guard 全落地后回到"无人接管"
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
                phase: crate::progress::Phase::Apply,
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
            // Progress 不落盘（太密）
            fs_sink.emit(ProgressEvent::Progress {
                phase: crate::progress::Phase::Apply,
                ts_ms: 1,
                items_done: 1,
                items_total: 2,
                bytes_done: 1,
                bytes_total: 2,
                current_path: "a.bin".into(),
            });
        }
        let read = |n: &str| std::fs::read_to_string(dir.join(n)).unwrap();
        let run = read(FileSink::RUN);
        let errors = read(FileSink::ERRORS);
        let items = read(FileSink::ITEMS);
        assert_eq!(run.lines().count(), 3, "run.jsonl = info + warn + error，不含 item/progress：\n{run}");
        assert!(!run.contains("\"progress\""), "Progress 不该落盘");
        assert_eq!(errors.lines().count(), 2, "errors.jsonl = warn + error：\n{errors}");
        assert!(!errors.contains("narrative"), "info 不进报错清单");
        assert_eq!(items.lines().count(), 1, "items.jsonl 只收 ItemResult：\n{items}");
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
        let run = std::fs::read_to_string(dir.join(FileSink::RUN)).unwrap();
        assert_eq!(run.lines().count(), 1, "level=warn 时 info 被挡掉：\n{run}");
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
