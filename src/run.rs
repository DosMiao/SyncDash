//! 任务流水线：scan 双侧 → compare（sync 自动带 archive）→ apply → 成功后刷新 archive。
//! CLI 的 `run` 与 GUI 共用这一份逻辑。

use crate::compare::{Action, Op, Plan};
use crate::config::Job;
use crate::table::Snapshot;
use crate::{apply, compare, scan};
use std::path::Path;

/// 严谨级 → 扫描参数：quick（不 hash）| standard（hash+缓存）| paranoid（全量重 hash）
pub fn scan_opts(job: &Job) -> scan::ScanOptions {
    let filter = crate::filter::PathFilter::build(&job.include, &job.exclude);
    let (hash, force_rehash) = match job.rigor.as_str() {
        "quick" => (false, false),
        "paranoid" => (true, true),
        _ => (true, false),
    };
    scan::ScanOptions { hash: hash && !job.no_hash, force_rehash, filter }
}

pub fn compare_job(job: &Job) -> std::io::Result<Plan> {
    let opt = scan_opts(job);
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
    let copts = compare::CompareOptions { case_insensitive: !job.case_sensitive };
    Ok(compare::compare(&s, &t, &job.mode, archive.as_ref(), false, &copts))
}

/// 执行选中的 ops；全部成功且是 sync 模式时刷新 archive（冲突路径从存档剔除，下次继续报冲突）。
pub fn apply_job(job: &Job, plan: &Plan, ops: &[Op], trash: Option<std::path::PathBuf>, verbose: bool) -> (u64, u64, u64) {
    let (done, skipped, errors) = apply::apply(
        ops,
        Path::new(&plan.header.source_root),
        Path::new(&plan.header.target_root),
        &apply::ApplyOptions { dry_run: false, trash, verbose, verify: job.rigor == "paranoid" },
    );
    if errors == 0 && job.mode == "sync" {
        if let Some(arch_path) = &job.archive {
            let conflicted: std::collections::HashSet<&str> = plan
                .ops
                .iter()
                .filter(|o| o.action == Action::Conflict)
                .map(|o| o.path.as_str())
                .collect();
            let opt = scan_opts(job);
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
