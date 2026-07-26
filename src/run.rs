//! 任务流水线：scan 双侧 → compare（sync 自动带 archive）→ apply → 成功后刷新 archive。
//! CLI 的 `run` 与 GUI 共用这一份逻辑。

use crate::compare::{Action, Op, Plan};
use crate::config::Job;
use crate::table::Snapshot;
use crate::{apply, compare, scan};
use std::path::Path;

pub fn compare_job(job: &Job) -> std::io::Result<Plan> {
    let opt = scan::ScanOptions { hash: !job.no_hash, extra_excludes: job.exclude.clone() };
    for (label, r) in [("source", &job.source), ("target", &job.target)] {
        if !r.is_dir() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("{label} root not accessible: {}", r.display()),
            ));
        }
    }
    let s = scan::scan(&job.source, &opt)?;
    let t = scan::scan(&job.target, &opt)?;
    let archive = match (&job.archive, job.mode.as_str()) {
        (Some(p), "sync") if p.is_file() => Some(Snapshot::load(p)?),
        _ => None,
    };
    Ok(compare::compare(&s, &t, &job.mode, archive.as_ref(), false))
}

/// 执行选中的 ops；全部成功且是 sync 模式时刷新 archive（冲突路径从存档剔除，下次继续报冲突）。
pub fn apply_job(job: &Job, plan: &Plan, ops: &[Op], trash: Option<std::path::PathBuf>, verbose: bool) -> (u64, u64, u64) {
    let (done, skipped, errors) = apply::apply(
        ops,
        Path::new(&plan.header.source_root),
        Path::new(&plan.header.target_root),
        &apply::ApplyOptions { dry_run: false, trash, verbose },
    );
    if errors == 0 && job.mode == "sync" {
        if let Some(arch_path) = &job.archive {
            let conflicted: std::collections::HashSet<&str> = plan
                .ops
                .iter()
                .filter(|o| o.action == Action::Conflict)
                .map(|o| o.path.as_str())
                .collect();
            let opt = scan::ScanOptions { hash: !job.no_hash, extra_excludes: job.exclude.clone() };
            if let Ok(mut snap) = scan::scan(&job.source, &opt) {
                snap.header.kind = "archive".into();
                snap.entries.retain(|e| !conflicted.contains(e.path.as_str()));
                if let Some(dir) = arch_path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                if let Ok(f) = std::fs::File::create(arch_path) {
                    let mut w = std::io::BufWriter::new(f);
                    if snap.write_to(&mut w).is_ok() {
                        eprintln!("archive refreshed: {}", arch_path.display());
                    }
                }
            }
        } else {
            eprintln!("hint: sync job without `archive` — add one so deletions/moves can be attributed next time");
        }
    }
    (done, skipped, errors)
}
