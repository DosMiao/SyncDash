//! SyncDash 自己写到磁盘上的所有文件名/目录名。
//!
//! 集中在此的原因是一条跨文件约束：`filter` 必须按名字排除这些东西，而写它们的是
//! atomic / lock / preflight / version / runlog。名字散开时任何一处改名都会让过滤器
//! 静默失效——`.syncdash-root` 被同步到对端后，没挂载的空目录也会长出标记，
//! 挂载点闸门随之整个失效。

/// 同目录临时文件前缀。真名是 `{TEMP_PREFIX}{basename}.{pid}`。
pub const TEMP_PREFIX: &str = ".syncdash.tmp.";

/// 临时文件的存活上限，超过即视为上一次崩溃的残留。
pub const TEMP_LIFETIME_MS: i64 = 24 * 60 * 60 * 1000;


/// 根心跳锁，防双机并发 apply。
pub const LOCK_NAME: &str = ".syncdash.lock";

/// 挂载点标记（对应 syncthing 的 `.stfolder`）。配 `require_marker` 使用。
pub const MARKER_NAME: &str = ".syncdash-root";

/// 版本控制存放目录（root 内）。
pub const VERSION_STORE_DIR: &str = ".version_syncDash";

/// 工具自留目录（缓存等）。
pub const APP_DIR: &str = ".syncdash";


/// 运行一览索引（每行一条 `RunRecord`）。
pub const RUNLOG_INDEX_FILE: &str = "runs.jsonl";

/// 单次运行的汇总。
pub const RUNLOG_SUMMARY_FILE: &str = "summary.json";

/// 单次运行的计划快照。
pub const RUNLOG_PLAN_FILE: &str = "plan.jsonl";

/// 事件流三份产物。
pub const RUNLOG_RUN_FILE: &str = "run.jsonl";
pub const RUNLOG_ERRORS_FILE: &str = "errors.jsonl";
pub const RUNLOG_ITEMS_FILE: &str = "items.jsonl";

/// 进程级应用日志。
pub const APP_LOG_FILE: &str = "app.jsonl";


/// 冲突副本中缀：`report.pdf` → `report.sync-conflict-<ts>-<host>.pdf`。
pub const CONFLICT_INFIX: &str = ".sync-conflict-";


/// 工具自身的元数据。无条件排除，任何档位都不放行。
pub fn self_excludes() -> Vec<String> {
    vec![
        format!("*/{APP_DIR}/"),
        format!("*/{VERSION_STORE_DIR}/"),
        format!("*/{LOCK_NAME}"),
        format!("*/{TEMP_PREFIX}*"),
        format!("*/{MARKER_NAME}"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn self_excludes_cover_every_artifact_we_write() {
        let v = self_excludes();
        // 这几条是「过滤器必须认得自己」的回归闸：任何一条丢了，
        // 工具的元数据就会被当成待同步内容传到对端去。
        assert!(v.iter().any(|s| s.contains(APP_DIR)));
        assert!(v.iter().any(|s| s.contains(VERSION_STORE_DIR)));
        assert!(v.iter().any(|s| s.contains(LOCK_NAME)));
        assert!(v.iter().any(|s| s.contains(TEMP_PREFIX)));
        assert!(v.iter().any(|s| s.contains(MARKER_NAME)));
        assert_eq!(v.len(), 5);
    }

    #[test]
    fn artifact_names_are_distinct() {
        let all = [
            RUNLOG_INDEX_FILE, RUNLOG_SUMMARY_FILE, RUNLOG_PLAN_FILE,
            RUNLOG_RUN_FILE, RUNLOG_ERRORS_FILE, RUNLOG_ITEMS_FILE, APP_LOG_FILE,
        ];
        let mut sorted = all.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), all.len(), "运行目录里的产物名不能撞车");
    }
}
