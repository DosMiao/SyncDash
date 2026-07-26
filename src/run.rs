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
    scan::ScanOptions { hash: hash && !job.no_hash, force_rehash, symlinks_direct: job.symlinks == "direct", filter }
}

/// sync 成功后刷新 archive：重扫 source、剔除冲突路径（冲突下次继续报，绝不被静默仲裁）
fn refresh_archive(job: &Job, plan: &Plan) {
    let Some(arch_path) = &job.archive else {
        eprintln!("hint: sync job without `archive` — add one so deletions/moves can be attributed next time");
        return;
    };
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
        refresh_archive(job, plan);
    }
    (done, skipped, errors)
}

/// 本地/挂载盘任务的一条龙（原 CLI run 的主体）。返回 (done, skipped, errors, conflicts)。
pub fn run_local_job(name: &str, job: &Job, do_apply: bool, verbose: bool) -> std::io::Result<(u64, u64, u64, u64)> {
    let plan = compare_job(job)?;
    eprintln!("[{name}] {} op(s), {} conflict(s)", plan.header.op_count, plan.header.conflict_count);
    for op in &plan.ops {
        println!("{}", serde_json::to_string(op)?);
    }
    if !do_apply {
        println!("dry-run (rerun with --apply)");
        return Ok((0, plan.ops.len() as u64, 0, plan.header.conflict_count));
    }
    let ops: Vec<Op> = plan
        .ops
        .iter()
        .filter(|o| !matches!(o.action, Action::Conflict | Action::Note))
        .cloned()
        .collect();
    let (done, skipped, errors) = apply_job(job, &plan, &ops, None, verbose);
    Ok((done, skipped, errors, plan.header.conflict_count))
}

/// 远程管线（v0.6 ssh 一条龙）：ssh 探测 → 远端本地扫描（stdout 收表）→ 本地扫描 → 比对
/// → target 侧打包 ssh 送达 apply-pack → source 侧经挂载路径直落 → sync 成功后刷新 archive。
pub fn run_remote_job(name: &str, job: &Job, do_apply: bool, verbose: bool) -> std::io::Result<(u64, u64, u64, u64)> {
    use crate::compare::Side;
    let host = job
        .remote_host
        .as_deref()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "remote_host not set"))?;
    let exe = job.remote_exe.as_deref().unwrap_or("syncdash");
    let rroot = job
        .remote_root
        .as_deref()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "remote_root required for remote jobs"))?;

    // 1) 探测：可达性 + schema 一致性
    let probe = crate::remote::ssh_capture(host, &format!("{exe} probe"))?;
    let pv: serde_json::Value = serde_json::from_slice(&probe)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("bad probe output: {e}")))?;
    if pv["schema"].as_u64() != Some(crate::table::SCHEMA as u64) {
        eprintln!("[{name}] warning: remote schema {} != local {} — rebuild the remote binary", pv["schema"], crate::table::SCHEMA);
    }
    eprintln!("[{name}] remote {}: {} {}", host, pv["os"].as_str().unwrap_or("?"), pv["arch"].as_str().unwrap_or("?"));

    // 2) 远端扫描（在远端自己的盘上哈希——比经 UNC 拉数据快得多）
    let mut scan_cmd = format!("{exe} scan {}", crate::remote::shq(rroot));
    match job.rigor.as_str() {
        "quick" => scan_cmd.push_str(" --no-hash"),
        "paranoid" => scan_cmd.push_str(" --force-rehash"),
        _ => {}
    }
    for ex in &job.exclude {
        scan_cmd.push_str(&format!(" --exclude {}", crate::remote::shq(ex)));
    }
    if job.symlinks == "direct" {
        scan_cmd.push_str(" --symlinks-direct");
    }
    let table_bytes = crate::remote::ssh_capture(host, &scan_cmd)?;
    let t = Snapshot::from_reader(std::io::BufReader::new(&table_bytes[..]))?;

    // 3) 本地扫描 + 比对
    let s = scan::scan(&job.source, &scan_opts(job))?;
    let archive = match (&job.archive, job.mode.as_str()) {
        (Some(p), "sync") if p.is_file() => Some(Snapshot::load(p)?),
        _ => None,
    };
    let copts = compare::CompareOptions { case_insensitive: !job.case_sensitive };
    let plan = compare::compare(&s, &t, &job.mode, archive.as_ref(), false, &copts);
    eprintln!("[{name}] {} op(s), {} conflict(s)  (remote pipeline via ssh:{host})", plan.header.op_count, plan.header.conflict_count);
    for op in &plan.ops {
        println!("{}", serde_json::to_string(op)?);
    }
    if !do_apply {
        println!("dry-run (rerun with --apply)");
        return Ok((0, plan.ops.len() as u64, 0, plan.header.conflict_count));
    }

    let mut done = 0u64;
    let mut skipped = 0u64;
    let mut errors = 0u64;

    // 4) target 侧：打包 → ssh 送包 → 远端 apply-pack（远端自带锁/回收/校验）
    let has_target_ops = plan.ops.iter().any(|o| o.side == Side::Target && !matches!(o.action, Action::Conflict | Action::Note));
    if has_target_ops {
        let tmp = std::env::temp_dir().join(format!("syncdash-remote-{}.tar", crate::table::now_ms()));
        let sum = crate::pack::pack(&plan, &job.source, &tmp)?;
        let rpkg = format!("/tmp/syncdash-{}.tar", crate::table::now_ms());
        let ship = crate::remote::ssh_send_file(host, &tmp, &rpkg);
        let _ = std::fs::remove_file(&tmp);
        ship?;
        let vflag = if verbose { " -v" } else { "" };
        let ok = crate::remote::ssh_run(host, &format!("{exe} apply-pack {} --apply{vflag}; rc=$?; rm -f {}; exit $rc", crate::remote::shq(&rpkg), crate::remote::shq(&rpkg)))?;
        if ok {
            done += sum.ops;
        } else {
            errors += 1;
            eprintln!("[{name}] remote apply-pack reported failure");
        }
    }

    // 5) source 侧（sync 的回拉方向）：经挂载路径直读远端内容；不可达则跳过并说明
    let src_ops: Vec<Op> = plan
        .ops
        .iter()
        .filter(|o| o.side == Side::Source && !matches!(o.action, Action::Conflict | Action::Note))
        .cloned()
        .collect();
    if !src_ops.is_empty() {
        if job.target.is_dir() {
            let (d2, s2, e2) = crate::apply::apply(
                &src_ops,
                &job.source,
                &job.target,
                &crate::apply::ApplyOptions { dry_run: false, trash: None, verbose, verify: job.rigor == "paranoid" },
            );
            done += d2;
            skipped += s2;
            errors += e2;
        } else {
            skipped += src_ops.len() as u64;
            eprintln!(
                "[{name}] {} source-side op(s) skipped: mounted target '{}' not accessible (pull direction needs the SMB mount)",
                src_ops.len(),
                job.target.display()
            );
        }
    }

    if errors == 0 && job.mode == "sync" {
        refresh_archive(job, &plan);
    }
    Ok((done, skipped, errors, plan.header.conflict_count))
}
