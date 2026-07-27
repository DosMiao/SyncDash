//! 任务流水线：scan 双侧 → compare（sync 自动带 archive）→ apply → 成功后刷新 archive。
//! CLI 的 `run` 与 GUI 共用这一份逻辑。

use crate::compare::{Action, Op, Plan};
use crate::config::Job;
use crate::table::Snapshot;
use crate::{apply, compare, scan};
use std::path::Path;

/// 严谨级 → 扫描参数。单调阶梯：每一级 = **本轮实际读得更多**，
/// "一致 ✓"的含义逐级从"实测元数据"升到"实测每个字节"。
/// quick    读 0 字节（size+mtime）
/// fast     只实读变化面的采样窗（缓存加速未变面）
/// standard 本轮实读每个文件的采样窗（不用缓存——不是记忆，是本轮测量）
/// paranoid 本轮实读每个文件的全部字节
pub fn scan_opts(job: &Job) -> scan::ScanOptions {
    let filter = crate::filter::PathFilter::build_full_opt(&job.include, &job.exclude, &job.deletable, &job.os_excludes, job.dev_excludes);
    let r = job.rigor_resolved();
    scan::ScanOptions { hash: r.hash, sampled: r.sampled, use_cache: r.use_cache, symlinks_direct: job.symlinks == "direct", filter }
}

/// sync 成功后刷新 archive：重扫 source、剔除冲突路径（冲突下次继续报，绝不被静默仲裁）。
/// v0.9 M1：Refresh 阶段可见化——archive 重扫是个今天完全隐形的长阶段，挂上事件流与取消。
/// 被取消只意味着冲突下轮重报，安全。
fn refresh_archive_with(job: &Job, plan: &Plan, ctx: &crate::progress::RunCtx) {
    let Some(arch_path) = &job.archive else {
        ctx.log(
            crate::progress::LogLevel::Warn,
            "run",
            "hint: sync job without `archive` — add one so deletions/moves can be attributed next time",
        );
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
                ctx.log(
                    crate::progress::LogLevel::Info,
                    "run",
                    format!("archive refreshed: {}", arch_path.display()),
                );
            }
        }
    }
}

pub fn compare_job(job: &Job) -> std::io::Result<Plan> {
    compare_job_with(job, &crate::progress::RunCtx::null())
}

/// 比对的完整产出：计划 + 两侧快照。
/// 桌面壳要靠快照算"证据层"（双侧 size/mtime、相同项），CLI 只要 plan。
pub struct CompareOutcome {
    pub plan: Plan,
    pub source: Snapshot,
    pub target: Snapshot,
}

/// v0.9 M1：带事件流/取消的比对一条龙。src-tauri 里那份为插事件而内联复制的
/// 第二套管线由本函数取代（撤销双份漂移）。
pub fn compare_job_with(job: &Job, ctx: &crate::progress::RunCtx) -> std::io::Result<Plan> {
    compare_job_detailed(job, ctx).map(|o| o.plan)
}

/// 同一条管线，额外把两侧快照交出来（丢掉它们等于逼界面再扫一遍）
pub fn compare_job_detailed(job: &Job, ctx: &crate::progress::RunCtx) -> std::io::Result<CompareOutcome> {
    use crate::progress::Phase;
    let opt = scan_opts(job);
    // P0-2：root 可达性 + 挂载点标记。共享盘没挂上时 target 常常是个空目录，
    // 照常比对会产出"把对面删光"或"全量重传"的计划。
    let mut v = crate::preflight::Verdict { blockers: Vec::new(), warnings: Vec::new() };
    crate::preflight::check_root("source", &job.source, job.require_marker, &mut v);
    crate::preflight::check_root("target", &job.target, job.require_marker, &mut v);
    for w in &v.warnings {
        // v0.10：Log{Warn} 取代 Error{action:"warning"} 那个 hack——
        // 有了真正的等级，警告不必再伪装成"不算数的错误"
        ctx.log(crate::progress::LogLevel::Warn, "compare", format!("warning: {w}"));
    }
    if !v.ok() {
        return Err(std::io::Error::new(std::io::ErrorKind::NotFound, v.blockers.join("; ")));
    }
    // 双侧并行扫描：source 与 target 几乎总在不同的盘/链路上（本地盘 vs SMB、
    // OneDrive vs 外置盘），串行等于白白排队——并行后墙钟≈慢的一侧。
    // 两侧各自的 PhaseStart 会同时出现，进度面板双行同时跳。
    let (s, t) = std::thread::scope(|sc| {
        let hs = sc.spawn(|| scan::scan_ctx(&job.source, &opt, ctx, Phase::ScanSource));
        let ht = sc.spawn(|| scan::scan_ctx(&job.target, &opt, ctx, Phase::ScanTarget));
        (hs.join().unwrap(), ht.join().unwrap())
    });
    let (s, t) = (s?, t?);
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
    let plan = compare::compare(&s, &t, &job.mode, archive.as_ref(), false, &copts);
    // 分歧升级：抽样证据档里"摘要相等但 mtime 差 >2s"的文件不许直接判相等（旋钮可关）
    let rr = job.rigor_resolved();
    let plan = if rr.sampled && rr.escalate { escalate_sampled_disagreements(job, plan, &s, &t, ctx) } else { plan };
    Ok(CompareOutcome { plan, source: s, target: t })
}

/// 分歧升级规则：两份信号打架（抽样摘要说"相同"、mtime 说"动过"）时不静默采信任何一方，
/// 把该文件**双侧升级为全量哈希**重新裁决。这把 fast/standard 的盲区从
/// "采样窗外的一切改动"压缩到"采样窗外＋保时间戳"（≈timestomp 场景）。
/// 升级集天然很小（正常树上接近零），逐个全量读的成本可忽略。仅本地管线（远端侧无从廉价重读）。
fn escalate_sampled_disagreements(
    job: &Job,
    mut plan: Plan,
    s: &Snapshot,
    t: &Snapshot,
    ctx: &crate::progress::RunCtx,
) -> Plan {
    use crate::compare::Side;
    use crate::progress::LogLevel;
    use crate::table::EntryKind;
    use rayon::prelude::*;
    const SLACK_MS: i64 = 2000;

    fn full_hash(p: &Path) -> std::io::Result<String> {
        let mut h = blake3::Hasher::new();
        h.update_mmap(p)?;
        Ok(h.finalize().to_hex().to_string())
    }
    fn native(root: &Path, rel: &str) -> std::path::PathBuf {
        let r = if cfg!(windows) { rel.replace('/', "\\") } else { rel.to_string() };
        root.join(r)
    }

    let tmap: std::collections::HashMap<&str, &crate::table::Entry> = t
        .entries
        .iter()
        .filter(|e| e.kind == EntryKind::File)
        .map(|e| (e.path.as_str(), e))
        .collect();
    let suspects: Vec<(&crate::table::Entry, &crate::table::Entry)> = s
        .entries
        .iter()
        .filter(|e| e.kind == EntryKind::File)
        .filter_map(|se| tmap.get(se.path.as_str()).map(|te| (se, *te)))
        .filter(|(se, te)| match (&se.hash, &te.hash) {
            (Some(a), Some(b)) => a.starts_with('~') && a == b && (se.mtime_ms - te.mtime_ms).abs() > SLACK_MS,
            _ => false,
        })
        .collect();
    if suspects.is_empty() {
        return plan;
    }
    ctx.log(LogLevel::Info, "compare", format!("分歧升级：{} 个文件摘要相等但 mtime 差 >2s，双侧全量重验", suspects.len()));
    let extra: Vec<Op> = suspects
        .par_iter()
        .filter_map(|(se, te)| {
            let hs = full_hash(&native(&job.source, &se.path)).ok()?;
            let ht = full_hash(&native(&job.target, &te.path)).ok()?;
            if hs == ht {
                return None; // 摘要没撒谎：只是 mtime 漂了
            }
            let reason = "escalated: sampled digests equal, mtime differs, full hashes differ";
            match job.mode.as_str() {
                // mirror：source 无条件赢
                "mirror" => Some(Op { side: Side::Target, action: Action::Update, path: se.path.clone(), from: None, size: Some(se.size), mtime_ms: Some(se.mtime_ms), hash: Some(hs), link: None, mode: None, reason: reason.into() }),
                // sync：双边内容不同且无归因 → 如实报冲突，人来裁
                "sync" => Some(Op { side: Side::Target, action: Action::Conflict, path: se.path.clone(), from: None, size: Some(se.size), mtime_ms: Some(se.mtime_ms), hash: None, link: None, mode: None, reason: reason.into() }),
                // enrich：source 严格较新才更新
                _ => {
                    if se.mtime_ms > te.mtime_ms + SLACK_MS {
                        Some(Op { side: Side::Target, action: Action::Update, path: se.path.clone(), from: None, size: Some(se.size), mtime_ms: Some(se.mtime_ms), hash: Some(hs), link: None, mode: None, reason: reason.into() })
                    } else {
                        None
                    }
                }
            }
        })
        .collect();
    if !extra.is_empty() {
        ctx.log(LogLevel::Warn, "compare", format!("分歧升级坐实 {} 个文件内容不同（抽样窗外的改动），已补进计划", extra.len()));
        let new_conflicts = extra.iter().filter(|o| o.action == Action::Conflict).count() as u64;
        plan.header.op_count += extra.len() as u64;
        plan.header.conflict_count += new_conflicts;
        plan.ops.extend(extra);
    }
    plan
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
    crate::log_info!("run", "[{name}] {} op(s), {} conflict(s)", plan.header.op_count, plan.header.conflict_count);
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
    // M4：CLI 的 apply 也留运行日志（desktop 在壳层各自记录）
    let t0 = std::time::Instant::now();
    let rec = crate::runlog::Recorder::start(name, "apply", &crate::progress::RunCtx::null(), &ops);
    let out = apply_job_guarded_with(job, &plan, &ops, None, verbose, acknowledged, &rec.ctx);
    rec.finish(&out, t0.elapsed().as_millis() as u64);
    Ok((out.done, out.skipped, out.errors, plan.header.conflict_count))
}

/// 远程管线（v0.6 ssh 一条龙）：ssh 探测 → 远端本地扫描（stdout 收表）→ 本地扫描 → 比对
/// → target 侧打包 ssh 送达 apply-pack → source 侧经挂载路径直落 → sync 成功后刷新 archive。
pub fn run_remote_job(name: &str, job: &Job, do_apply: bool, verbose: bool, acknowledged: bool) -> std::io::Result<(u64, u64, u64, u64)> {
    run_remote_job_with(name, job, do_apply, verbose, acknowledged, &crate::progress::RunCtx::null())
}

/// v0.9 M1/M3：远程管线 = 比对段 + 执行段（desktop 分两次 IPC 各自调用；CLI 在这里一条龙）。
/// 级边界 PhaseStart、级间协作点、终态 Summary；ssh 传输内部的逐字节计数与
/// kill-on-cancel 是明确后补（M1 步骤 8）。
pub fn run_remote_job_with(
    name: &str,
    job: &Job,
    do_apply: bool,
    verbose: bool,
    acknowledged: bool,
    ctx: &crate::progress::RunCtx,
) -> std::io::Result<(u64, u64, u64, u64)> {
    let plan = match compare_remote_job_with(name, job, ctx) {
        Ok(p) => p,
        Err(e) => {
            // 比对段取消：终态必须可见（desktop 靠 Summary 收窗）
            if crate::progress::is_cancelled(&e) {
                emit_cancel_summary(ctx, std::time::Instant::now());
            }
            return Err(e);
        }
    };
    ctx.log(
        crate::progress::LogLevel::Info,
        "run",
        format!(
            "[{name}] {} op(s), {} conflict(s)  (remote pipeline via ssh)",
            plan.header.op_count, plan.header.conflict_count
        ),
    );
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
    let t0 = std::time::Instant::now();
    let rec = crate::runlog::Recorder::start(name, "remote-apply", ctx, &ops);
    let out = apply_remote_job_with(name, job, &plan, &ops, verbose, acknowledged, &rec.ctx)?;
    rec.finish(&out, t0.elapsed().as_millis() as u64);
    Ok((out.done, out.skipped, out.errors, plan.header.conflict_count))
}

fn emit_cancel_summary(ctx: &crate::progress::RunCtx, t0: std::time::Instant) {
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

/// 远端连接参数（probe 的产物）。desktop 的比对/执行是两次独立 IPC，
/// 中间不保存连接——执行段重新 probe（一次 ssh 往返，顺带就是可达性预检）。
pub struct RemoteLink {
    pub host: String,
    pub exe: String,
    pub rroot: String,
    pub shell: crate::remote::RemoteShell,
}

/// 阶段 1：探测可达性 + schema 一致性 + 远端 OS（决定 shell 方言）。
///
/// 收 `ctx` 是为了 schema 不匹配那条警告：它必须在**比对期**就到达界面。
/// 走宏经全局注册表的话，compare 期间没装 sink（只有 apply 才起 Recorder），
/// 这条恰恰会退回 stderr——在 windowed 桌面构建里等于没说。
pub fn probe_remote(name: &str, job: &Job, ctx: &crate::progress::RunCtx) -> std::io::Result<RemoteLink> {
    let host = job
        .remote_host
        .as_deref()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "remote_host not set"))?;
    let exe = job.remote_exe.as_deref().unwrap_or("syncdash");
    let rroot = job
        .remote_root
        .as_deref()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "remote_root required for remote jobs"))?;
    let probe = crate::remote::ssh_capture(host, &format!("{exe} probe"))?;
    let pv: serde_json::Value = serde_json::from_slice(&probe)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("bad probe output: {e}")))?;
    if pv["schema"].as_u64() != Some(crate::table::SCHEMA as u64) {
        ctx.log(
            crate::progress::LogLevel::Warn,
            "remote",
            format!(
                "[{name}] warning: remote schema {} != local {} — rebuild the remote binary",
                pv["schema"],
                crate::table::SCHEMA
            ),
        );
    }
    let remote_os = pv["os"].as_str().unwrap_or("").to_string();
    ctx.log(
        crate::progress::LogLevel::Info,
        "remote",
        format!("[{name}] remote {}: {} {}", host, remote_os, pv["arch"].as_str().unwrap_or("?")),
    );
    Ok(RemoteLink {
        host: host.to_string(),
        exe: exe.to_string(),
        rroot: rroot.to_string(),
        shell: crate::remote::RemoteShell::from_os(&remote_os),
    })
}

/// v0.9 M3：远程任务的**比对段**——desktop `compare_job` 对 remote 任务直接走这里，
/// 不再静默落进本地管线（那会经 UNC 拉数据重哈希，慢一个数量级还语义错位）。
pub fn compare_remote_job_with(name: &str, job: &Job, ctx: &crate::progress::RunCtx) -> std::io::Result<Plan> {
    compare_remote_job_detailed(name, job, ctx).map(|o| o.plan)
}

/// 远程管线的同款细节版：远端快照是经 ssh 拉回来的完整表，
/// 证据层（双侧 size/mtime、相同项）在这里和本地任务一样能算。
pub fn compare_remote_job_detailed(
    name: &str,
    job: &Job,
    ctx: &crate::progress::RunCtx,
) -> std::io::Result<CompareOutcome> {
    use crate::progress::{Phase, PhaseProgress};
    let link = probe_remote(name, job, ctx)?;

    // 2) 远端扫描（在远端自己的盘上哈希——比经 UNC 拉数据快得多）
    // 远端按**解析后**的旋钮显式传参（预设名不够——明细可能被覆盖）
    let rr = job.rigor_resolved();
    let mut scan_args: Vec<String> = vec![
        "scan".into(),
        link.rroot.clone(),
        "--evidence".into(),
        (if !rr.hash { "none" } else if rr.sampled { "sampled" } else { "full" }).into(),
        "--cache".into(),
        (if rr.use_cache { "on" } else { "off" }).into(),
    ];
    if job.dev_excludes {
        scan_args.push("--dev-excludes".into());
    }
    if job.os_excludes != "auto" {
        scan_args.push("--os-excludes".into());
        scan_args.push(job.os_excludes.clone());
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
    let _pp_rs = PhaseProgress::begin(ctx, Phase::ScanTarget, Some(format!("ssh:{} {}", link.host, link.rroot)), 0, 0);
    let table_bytes = crate::remote::ssh_capture(&link.host, &crate::remote::remote_cmd(link.shell, &link.exe, &scan_args))?;
    let t = Snapshot::from_reader(std::io::BufReader::new(&table_bytes[..]))?;

    // 3) 本地扫描 + 比对（本地 source 侧同样过挂载点闸门）
    let mut v = crate::preflight::Verdict { blockers: Vec::new(), warnings: Vec::new() };
    crate::preflight::check_root("source", &job.source, job.require_marker, &mut v);
    for w in &v.warnings {
        ctx.log(crate::progress::LogLevel::Warn, "compare", format!("[{name}] warning: {w}"));
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
    Ok(CompareOutcome { plan, source: s, target: t })
}

/// 远程任务的计划体检（desktop 确认单用）：只有删除占比闸门——
/// 磁盘空间与 marker 在远端机器上，本地查不到也不该假装查了。
pub fn preflight_remote_job(job: &Job, plan: &Plan, ops: &[Op], acknowledged: bool) -> crate::preflight::Verdict {
    let g = job.guards(acknowledged);
    let st = crate::preflight::stat_plan(ops);
    let mut gv = crate::preflight::Verdict { blockers: Vec::new(), warnings: Vec::new() };
    crate::preflight::check_delete_ratio("target", &st.target, plan.header.target_entries, &g, &mut gv);
    crate::preflight::check_delete_ratio("source", &st.source, plan.header.source_entries, &g, &mut gv);
    gv
}

/// v0.9 M3：远程任务的**执行段**——`ops` 是用户在差异表定稿的子集（翻向/勾选已生效）。
/// 重新 probe → 体检 → 打包选中项 → ssh 送包 → 远端 apply-pack → source 回拉 → 刷新 → Summary。
pub fn apply_remote_job_with(
    name: &str,
    job: &Job,
    plan: &Plan,
    ops: &[Op],
    verbose: bool,
    acknowledged: bool,
    ctx: &crate::progress::RunCtx,
) -> std::io::Result<crate::progress::ApplyOutcome> {
    let t0 = std::time::Instant::now();
    let r = apply_remote_inner(name, job, plan, ops, verbose, acknowledged, ctx, t0);
    if let Err(e) = &r {
        if crate::progress::is_cancelled(e) {
            emit_cancel_summary(ctx, t0);
        }
    }
    r
}

#[allow(clippy::too_many_arguments)]
fn apply_remote_inner(
    name: &str,
    job: &Job,
    plan_full: &Plan,
    sel_ops: &[Op],
    verbose: bool,
    acknowledged: bool,
    ctx: &crate::progress::RunCtx,
    t0: std::time::Instant,
) -> std::io::Result<crate::progress::ApplyOutcome> {
    use crate::compare::Side;
    use crate::progress::{ApplyOutcome, Phase, PhaseProgress, ProgressEvent};

    // 计划体检：远端磁盘空间查不到，但"删掉对面一大半"这类事故本地就能拦
    let gv = preflight_remote_job(job, plan_full, sel_ops, acknowledged);
    if !gv.report(name) {
        for b in &gv.blockers {
            ctx.sink.emit(ProgressEvent::Error {
                phase: Phase::Apply,
                ts_ms: crate::table::now_ms(),
                path: String::new(),
                action: "preflight".into(),
                side: "target".into(),
                message: b.clone(),
            });
        }
        return Ok(ApplyOutcome { done: 0, skipped: sel_ops.len() as u64, errors: 1, bytes_copied: 0, cancelled: false });
    }

    let link = probe_remote(name, job, ctx)?;
    let (host, exe, rroot, shell) = (link.host.as_str(), link.exe.as_str(), link.rroot.as_str(), link.shell);
    // 打包/回拉都只看定稿子集；full plan 只用于 archive 刷新（冲突路径剔除要看全量）
    let plan = Plan { header: plan_full.header.clone(), ops: sel_ops.to_vec() };

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
                    ctx.log(
                        crate::progress::LogLevel::Info,
                        "delta",
                        format!("[{name}] delta: got chunk tables for {} large file(s)", m.len()),
                    );
                    Some(m)
                }
                Err(e) => {
                    ctx.log(
                        crate::progress::LogLevel::Warn,
                        "delta",
                        format!("[{name}] delta disabled (chunk request failed: {e})"),
                    );
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
            ctx.log(
                crate::progress::LogLevel::Info,
                "pack",
                format!("[{name}] packed {} B, delta saved {} B", sum.bytes, sum.delta_saved),
            );
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
            ctx.log(crate::progress::LogLevel::Error, "remote", format!("[{name}] remote apply-pack reported failure"));
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
            ctx.log(
                crate::progress::LogLevel::Warn,
                "remote",
                format!(
                    "[{name}] {} source-side op(s) skipped: mounted target '{}' not accessible (pull direction needs the SMB mount)",
                    src_ops.len(),
                    job.target.display()
                ),
            );
        }
    }

    if errors == 0 && !ctx.ctl.cancelled() && job.mode == "sync" {
        refresh_archive_with(job, plan_full, ctx);
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
    Ok(ApplyOutcome {
        done,
        skipped,
        errors,
        bytes_copied: bytes_done_total,
        cancelled: ctx.ctl.cancelled(),
    })
}
