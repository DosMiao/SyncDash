//! 任务流水线：scan 双侧 → compare（sync 自动带 archive）→ apply → 成功后刷新 archive。
//! CLI 的 `run` 与 GUI 共用这一份逻辑。

use crate::compare::{Action, Op, Plan};
use crate::config::Job;
use crate::table::Snapshot;
use crate::{apply, compare, scan};
use std::path::Path;

/// 严谨级 → 扫描参数：quick（不 hash）| standard（hash+缓存）| paranoid（全量重 hash）
pub fn scan_opts(job: &Job) -> scan::ScanOptions {
    let filter = crate::filter::PathFilter::build_full(&job.include, &job.exclude, &job.deletable);
    let (hash, force_rehash) = match job.rigor.as_str() {
        "quick" => (false, false),
        "paranoid" => (true, true),
        _ => (true, false),
    };
    scan::ScanOptions { hash: hash && !job.no_hash, force_rehash, symlinks_direct: job.symlinks == "direct", filter }
}

/// sync 成功后刷新 archive：重扫 source、剔除冲突路径（冲突下次继续报，绝不被静默仲裁）。
/// v0.9 M1：Refresh 阶段可见化——archive 重扫是个今天完全隐形的长阶段，挂上事件流与取消。
/// 被取消只意味着冲突下轮重报，安全。
fn refresh_archive_with(job: &Job, plan: &Plan, ctx: &crate::progress::RunCtx) {
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
    // 上一代 archive：新表的每条把旧 hash 推进 prev 链，让"落后一代"能与
    // "并发修改"区分开（P1-3，见 compare::generation_of）
    let previous = if arch_path.is_file() { Snapshot::load(arch_path).ok() } else { None };
    if let Ok(mut snap) = scan::scan_ctx(&job.source, &opt, ctx, crate::progress::Phase::Refresh) {
        snap.header.kind = "archive".into();
        snap.entries.retain(|e| !conflicted.contains(e.path.as_str()));
        if let Some(prev) = &previous {
            crate::table::roll_generations(&mut snap.entries, &prev.entries);
        }
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
    compare_job_with(job, &crate::progress::RunCtx::null())
}

/// v0.9 M1：带事件流/取消的比对一条龙。src-tauri 里那份为插事件而内联复制的
/// 第二套管线由本函数取代（撤销双份漂移）。
pub fn compare_job_with(job: &Job, ctx: &crate::progress::RunCtx) -> std::io::Result<Plan> {
    use crate::progress::Phase;
    let opt = scan_opts(job);
    // P0-2：root 可达性 + 挂载点标记。共享盘没挂上时 target 常常是个空目录，
    // 照常比对会产出"把对面删光"或"全量重传"的计划。
    let mut v = crate::preflight::Verdict { blockers: Vec::new(), warnings: Vec::new() };
    crate::preflight::check_root("source", &job.source, job.require_marker, &mut v);
    crate::preflight::check_root("target", &job.target, job.require_marker, &mut v);
    for w in &v.warnings {
        eprintln!("warning: {w}");
        // windowed 桌面构建里 stderr 会丢——警告也走事件流（action 区分，不算 errors 计数）
        ctx.sink.emit(crate::progress::ProgressEvent::Error {
            phase: Phase::ScanSource,
            ts_ms: crate::table::now_ms(),
            path: String::new(),
            action: "warning".into(),
            side: "source".into(),
            message: w.clone(),
        });
    }
    if !v.ok() {
        return Err(std::io::Error::new(std::io::ErrorKind::NotFound, v.blockers.join("; ")));
    }
    let s = scan::scan_ctx(&job.source, &opt, ctx, Phase::ScanSource)?;
    let t = scan::scan_ctx(&job.target, &opt, ctx, Phase::ScanTarget)?;
    let archive = match (&job.archive, job.mode.as_str()) {
        (Some(p), "sync") if p.is_file() => Some(Snapshot::load(p)?),
        _ => None,
    };
    // compare 本身是亚秒级 CPU 活：只报阶段边界，不做内部计数
    let _pp = crate::progress::PhaseProgress::begin(
        ctx,
        Phase::Compare,
        Some(format!("{} × {} 条目", s.header.entry_count, t.header.entry_count)),
        0,
        0,
    );
    let copts = job.compare_opts();
    Ok(compare::compare(&s, &t, &job.mode, archive.as_ref(), false, &copts))
}

/// 执行选中的 ops；全部成功且是 sync 模式时刷新 archive（冲突路径从存档剔除，下次继续报冲突）。
pub fn apply_job(job: &Job, plan: &Plan, ops: &[Op], trash: Option<std::path::PathBuf>, verbose: bool) -> (u64, u64, u64) {
    apply_job_guarded(job, plan, ops, trash, verbose, false)
}

/// 只跑闸门不执行——GUI 在弹确认单之前调它，把拒绝理由完整展示给人看，
/// 而不是让理由只出现在没人看的 stderr 上。
pub fn preflight_job(job: &Job, plan: &Plan, ops: &[Op], acknowledged: bool) -> crate::preflight::Verdict {
    crate::preflight::run_all(
        ops,
        Path::new(&plan.header.source_root),
        Path::new(&plan.header.target_root),
        plan.header.source_entries,
        plan.header.target_entries,
        &job.guards(acknowledged),
    )
}

/// `acknowledged` = 用户显式 --i-know，只放行"计划体检"类闸门；
/// 标记缺失与磁盘空间不足始终拦截（那是环境问题，不是判断问题）。
pub fn apply_job_guarded(
    job: &Job,
    plan: &Plan,
    ops: &[Op],
    trash: Option<std::path::PathBuf>,
    verbose: bool,
    acknowledged: bool,
) -> (u64, u64, u64) {
    apply_job_guarded_with(job, plan, ops, trash, verbose, acknowledged, &crate::progress::RunCtx::null()).into_tuple()
}

/// v0.9 M1：带事件流的执行编排——Apply 阶段（apply_with 自报总量与逐字节进度）→
/// Refresh 阶段 → Summary 终态。
pub fn apply_job_guarded_with(
    job: &Job,
    plan: &Plan,
    ops: &[Op],
    trash: Option<std::path::PathBuf>,
    verbose: bool,
    acknowledged: bool,
    ctx: &crate::progress::RunCtx,
) -> crate::progress::ApplyOutcome {
    use crate::progress::{ApplyOutcome, Phase, ProgressEvent};
    let t0 = std::time::Instant::now();
    let src_root = Path::new(&plan.header.source_root);
    let tgt_root = Path::new(&plan.header.target_root);
    let verdict = crate::preflight::run_all(
        ops,
        src_root,
        tgt_root,
        plan.header.source_entries,
        plan.header.target_entries,
        &job.guards(acknowledged),
    );
    if !verdict.report("preflight") {
        for b in &verdict.blockers {
            ctx.sink.emit(ProgressEvent::Error {
                phase: Phase::Apply,
                ts_ms: crate::table::now_ms(),
                path: String::new(),
                action: "preflight".into(),
                side: "target".into(),
                message: b.clone(),
            });
        }
        return ApplyOutcome { done: 0, skipped: ops.len() as u64, errors: 1, bytes_copied: 0, cancelled: false };
    }
    let ap = apply::apply_with(ops, src_root, tgt_root, &job.apply_opts(trash, verbose), ctx);
    // 取消的运行不做 archive 刷新：用户要的是"立刻停"，且冲突下轮重报本来就安全
    if ap.errors == 0 && !ap.cancelled && job.mode == "sync" {
        refresh_archive_with(job, plan, ctx);
    }
    let out = ApplyOutcome { cancelled: ctx.ctl.cancelled(), ..ap };
    ctx.sink.emit(ProgressEvent::Summary {
        ts_ms: crate::table::now_ms(),
        done: out.done,
        skipped: out.skipped,
        errors: out.errors,
        bytes_done: out.bytes_copied,
        elapsed_ms: t0.elapsed().as_millis() as u64,
        paused_ms: ctx.ctl.paused_total_ms(),
        cancelled: out.cancelled,
    });
    out
}

/// 本地/挂载盘任务的一条龙（原 CLI run 的主体）。返回 (done, skipped, errors, conflicts)。
pub fn run_local_job(name: &str, job: &Job, do_apply: bool, verbose: bool, acknowledged: bool) -> std::io::Result<(u64, u64, u64, u64)> {
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
    let (done, skipped, errors) = apply_job_guarded(job, &plan, &ops, None, verbose, acknowledged);
    Ok((done, skipped, errors, plan.header.conflict_count))
}

/// 远程管线（v0.6 ssh 一条龙）：ssh 探测 → 远端本地扫描（stdout 收表）→ 本地扫描 → 比对
/// → target 侧打包 ssh 送达 apply-pack → source 侧经挂载路径直落 → sync 成功后刷新 archive。
pub fn run_remote_job(name: &str, job: &Job, do_apply: bool, verbose: bool, acknowledged: bool) -> std::io::Result<(u64, u64, u64, u64)> {
    run_remote_job_with(name, job, do_apply, verbose, acknowledged, &crate::progress::RunCtx::null())
}

/// v0.9 M1：远程管线的级边界事件化——每级 PhaseStart、级间协作点（取消/暂停响应）、终态 Summary。
/// ssh 传输内部的逐字节计数与 kill-on-cancel 是明确后补（M1 步骤 8）；本层保证的是：
/// 桌面能看见管线走到哪一级、能在级间取消、Summary 数字如实。
pub fn run_remote_job_with(
    name: &str,
    job: &Job,
    do_apply: bool,
    verbose: bool,
    acknowledged: bool,
    ctx: &crate::progress::RunCtx,
) -> std::io::Result<(u64, u64, u64, u64)> {
    let t0 = std::time::Instant::now();
    let r = run_remote_job_inner(name, job, do_apply, verbose, acknowledged, ctx, t0);
    if let Err(e) = &r {
        // 级间取消：计数尚无意义，但终态必须可见（desktop 靠 Summary 收窗）
        if crate::progress::is_cancelled(e) {
            ctx.sink.emit(crate::progress::ProgressEvent::Summary {
                ts_ms: crate::table::now_ms(),
                done: 0,
                skipped: 0,
                errors: 0,
                bytes_done: 0,
                elapsed_ms: t0.elapsed().as_millis() as u64,
                paused_ms: ctx.ctl.paused_total_ms(),
                cancelled: true,
            });
        }
    }
    r
}

#[allow(clippy::too_many_arguments)]
fn run_remote_job_inner(
    name: &str,
    job: &Job,
    do_apply: bool,
    verbose: bool,
    acknowledged: bool,
    ctx: &crate::progress::RunCtx,
    t0: std::time::Instant,
) -> std::io::Result<(u64, u64, u64, u64)> {
    use crate::compare::Side;
    use crate::progress::{Phase, PhaseProgress, ProgressEvent};
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
    let remote_os = pv["os"].as_str().unwrap_or("").to_string();
    let shell = crate::remote::RemoteShell::from_os(&remote_os);
    eprintln!("[{name}] remote {}: {} {}", host, remote_os, pv["arch"].as_str().unwrap_or("?"));

    // 2) 远端扫描（在远端自己的盘上哈希——比经 UNC 拉数据快得多）
    let mut scan_args: Vec<String> = vec!["scan".into(), rroot.to_string()];
    match job.rigor.as_str() {
        "quick" => scan_args.push("--no-hash".into()),
        "paranoid" => scan_args.push("--force-rehash".into()),
        _ => {}
    }
    for ex in &job.exclude {
        scan_args.push("--exclude".into());
        scan_args.push(ex.clone());
    }
    if job.symlinks == "direct" {
        scan_args.push("--symlinks-direct".into());
    }
    ctx.checkpoint()?;
    // 远端在自己盘上扫描，本地只能看到"进行中"——总量归零、label 说明白在等谁
    let _pp_rs = PhaseProgress::begin(ctx, Phase::ScanTarget, Some(format!("ssh:{host} {rroot}")), 0, 0);
    let table_bytes = crate::remote::ssh_capture(host, &crate::remote::remote_cmd(shell, exe, &scan_args))?;
    let t = Snapshot::from_reader(std::io::BufReader::new(&table_bytes[..]))?;

    // 3) 本地扫描 + 比对（本地 source 侧同样过挂载点闸门）
    let mut v = crate::preflight::Verdict { blockers: Vec::new(), warnings: Vec::new() };
    crate::preflight::check_root("source", &job.source, job.require_marker, &mut v);
    for w in &v.warnings {
        eprintln!("[{name}] warning: {w}");
    }
    if !v.ok() {
        return Err(std::io::Error::new(std::io::ErrorKind::NotFound, v.blockers.join("; ")));
    }
    let s = scan::scan_ctx(&job.source, &scan_opts(job), ctx, Phase::ScanSource)?;
    let archive = match (&job.archive, job.mode.as_str()) {
        (Some(p), "sync") if p.is_file() => Some(Snapshot::load(p)?),
        _ => None,
    };
    let _pp_cmp = PhaseProgress::begin(
        ctx,
        Phase::Compare,
        Some(format!("{} × {} 条目", s.header.entry_count, t.header.entry_count)),
        0,
        0,
    );
    let copts = job.compare_opts();
    let plan = compare::compare(&s, &t, &job.mode, archive.as_ref(), false, &copts);
    eprintln!("[{name}] {} op(s), {} conflict(s)  (remote pipeline via ssh:{host})", plan.header.op_count, plan.header.conflict_count);
    for op in &plan.ops {
        println!("{}", serde_json::to_string(op)?);
    }
    if !do_apply {
        println!("dry-run (rerun with --apply)");
        return Ok((0, plan.ops.len() as u64, 0, plan.header.conflict_count));
    }

    // 计划体检：远端磁盘空间查不到，但"删掉对面一大半"这类事故本地就能拦
    let g = job.guards(acknowledged);
    let st = crate::preflight::stat_plan(&plan.ops);
    let mut gv = crate::preflight::Verdict { blockers: Vec::new(), warnings: Vec::new() };
    crate::preflight::check_delete_ratio("target", &st.target, plan.header.target_entries, &g, &mut gv);
    crate::preflight::check_delete_ratio("source", &st.source, plan.header.source_entries, &g, &mut gv);
    if !gv.report(name) {
        return Ok((0, plan.ops.len() as u64, 1, plan.header.conflict_count));
    }

    let mut done = 0u64;
    let mut skipped = 0u64;
    let mut errors = 0u64;
    let mut bytes_done_total = 0u64;

    // 4) target 侧：（大更新先要远端块表走 FastCDC 增量）打包 → ssh 送包 → 远端 apply-pack
    let has_target_ops = plan.ops.iter().any(|o| o.side == Side::Target && !matches!(o.action, Action::Conflict | Action::Note));
    if has_target_ops {
        let delta_rels: Vec<String> = plan
            .ops
            .iter()
            .filter(|o| {
                o.side == Side::Target
                    && o.action == Action::Update
                    && o.link.is_none()
                    && o.size.map(|s| s >= crate::chunk::DELTA_MIN_SIZE).unwrap_or(false)
            })
            .map(|o| o.path.clone())
            .collect();
        let remote_chunks = if delta_rels.is_empty() {
            None
        } else {
            let mut args: Vec<String> = vec!["chunks".into(), "--root".into(), rroot.to_string()];
            for r in &delta_rels {
                args.push("--file".into());
                args.push(r.clone());
            }
            match crate::remote::ssh_capture(host, &crate::remote::remote_cmd(shell, exe, &args)) {
                Ok(bytes) => {
                    let mut m = std::collections::HashMap::new();
                    for line in String::from_utf8_lossy(&bytes).lines() {
                        let line = line.trim();
                        if line.is_empty() {
                            continue;
                        }
                        if let Ok(fc) = serde_json::from_str::<crate::chunk::FileChunks>(line) {
                            m.insert(fc.rel.clone(), fc);
                        }
                    }
                    eprintln!("[{name}] delta: got chunk tables for {} large file(s)", m.len());
                    Some(m)
                }
                Err(e) => {
                    eprintln!("[{name}] delta disabled (chunk request failed: {e})");
                    None
                }
            }
        };
        ctx.checkpoint()?;
        let pp_pack = PhaseProgress::begin(ctx, Phase::Pack, Some("打包 target 侧内容".into()), 0, 0);
        let tmp = std::env::temp_dir().join(format!("syncdash-remote-{}.tar", crate::table::now_ms()));
        let sum = crate::pack::pack(&plan, &job.source, &tmp, remote_chunks.as_ref())?;
        pp_pack.set_totals(sum.ops, sum.bytes);
        if sum.delta_saved > 0 {
            eprintln!("[{name}] packed {} B, delta saved {} B", sum.bytes, sum.delta_saved);
        }
        let rpkg = if shell == crate::remote::RemoteShell::PowerShell {
            format!("syncdash-{}.tar", crate::table::now_ms()) // 相对路径 → 远端家目录
        } else {
            format!("/tmp/syncdash-{}.tar", crate::table::now_ms())
        };
        ctx.checkpoint()?;
        let tar_len = std::fs::metadata(&tmp).map(|m| m.len()).unwrap_or(0);
        let pp_ship = PhaseProgress::begin(ctx, Phase::Ship, Some(format!("→ ssh:{host}")), 1, tar_len);
        let recv_cmd = crate::remote::remote_cmd(shell, exe, &["recv".into(), rpkg.clone()]);
        let ship = crate::remote::ssh_run_with_stdin(host, &recv_cmd, &tmp);
        let _ = std::fs::remove_file(&tmp);
        ship?;
        pp_ship.add_bytes(tar_len, &rpkg);
        pp_ship.item_done(&rpkg);
        bytes_done_total += sum.bytes;

        ctx.checkpoint()?;
        let _pp_ra = PhaseProgress::begin(ctx, Phase::Apply, Some(format!("ssh:{host} apply-pack")), sum.ops, 0);
        let mut ap_args: Vec<String> = vec!["apply-pack".into(), rpkg.clone(), "--apply".into(), "--remove-pkg".into()];
        if job.versioning {
            ap_args.push("--versioning".into());
        }
        if verbose {
            ap_args.push("-v".into());
        }
        let ok = crate::remote::ssh_run(host, &crate::remote::remote_cmd(shell, exe, &ap_args))?;
        if ok {
            done += sum.ops;
        } else {
            errors += 1;
            eprintln!("[{name}] remote apply-pack reported failure");
            ctx.sink.emit(ProgressEvent::Error {
                phase: Phase::Apply,
                ts_ms: crate::table::now_ms(),
                path: rpkg.clone(),
                action: "apply-pack".into(),
                side: "target".into(),
                message: "remote apply-pack reported failure".into(),
            });
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
            let out = crate::apply::apply_with(
                &src_ops,
                &job.source,
                &job.target,
                &job.apply_opts(None, verbose),
                ctx,
            );
            done += out.done;
            skipped += out.skipped;
            errors += out.errors;
            bytes_done_total += out.bytes_copied;
        } else {
            skipped += src_ops.len() as u64;
            eprintln!(
                "[{name}] {} source-side op(s) skipped: mounted target '{}' not accessible (pull direction needs the SMB mount)",
                src_ops.len(),
                job.target.display()
            );
        }
    }

    if errors == 0 && !ctx.ctl.cancelled() && job.mode == "sync" {
        refresh_archive_with(job, &plan, ctx);
    }
    ctx.sink.emit(ProgressEvent::Summary {
        ts_ms: crate::table::now_ms(),
        done,
        skipped,
        errors,
        bytes_done: bytes_done_total,
        elapsed_ms: t0.elapsed().as_millis() as u64,
        paused_ms: ctx.ctl.paused_total_ms(),
        cancelled: ctx.ctl.cancelled(),
    });
    Ok((done, skipped, errors, plan.header.conflict_count))
}
