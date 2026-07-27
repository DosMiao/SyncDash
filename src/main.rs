
use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;
use std::sync::Arc;
use syncdash::{apply, compare, config, filter, pack, run, scan, table, territory};

#[derive(Parser)]
#[command(name = "syncdash", version, about = "Table-driven multi-node file sync (scan -> compare -> apply)")]
struct Cli {
    /// 不带子命令（如双击 exe）时直接打开 GUI
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn FreeConsole() -> i32;
}

/// 双击启动时甩掉随附的控制台窗口
fn detach_console() {
    #[cfg(windows)]
    unsafe {
        FreeConsole();
    }
}

#[derive(Clone, ValueEnum)]
enum Mode {
    Mirror,
    Sync,
    Enrich,
}

#[derive(Subcommand)]
enum Cmd {
    /// 打印本机环境信息（远端探测用：ssh 对面跑这个）
    Probe,
    /// 列出任务配置（%APPDATA%\syncdash\jobs\*.toml）
    Jobs,
    /// 跑任务：扫双侧 → 比对 →（--apply 时）执行 + 刷新 archive。job 配置了 remote_host 则走 ssh 远程管线
    Run {
        /// 任务名（jobs 目录里的文件名）或 toml 路径；省略时配合 --all / --prefix
        job: Option<String>,
        /// 跑全部任务
        #[arg(long)]
        all: bool,
        /// 只跑名字以此开头的任务（如 cs-）
        #[arg(long)]
        prefix: Option<String>,
        #[arg(long)]
        apply: bool,
        /// 放行"计划体检"（删除占比过高）。标记缺失/空间不足依然拦截
        #[arg(long = "i-know")]
        i_know: bool,
        #[arg(short, long)]
        verbose: bool,
        /// M6 值守：循环 比对→（有差异且自动档时）执行→睡眠。Ctrl-C 退出
        #[arg(long)]
        watch: bool,
        /// watch 间隔秒（缺省用任务的 watch_interval_secs，其次 30）
        #[arg(long)]
        interval: Option<u64>,
        /// watch 发现差异时自动执行（等同任务里的 watch_auto_apply = true）
        #[arg(long = "auto-apply")]
        auto_apply: bool,
    },
    /// 打开图形界面（Tauri 桌面版；egui 旧界面已于 v0.9 退役）
    Gui,
    /// 扫描目录，产出快照表（JSONL，默认 stdout —— ssh 管道友好）
    Scan {
        root: PathBuf,
        #[arg(long)]
        out: Option<PathBuf>,
        /// 跳过内容 hash（快，但比对退化为 size+mtime，且无法做移动检测）
        #[arg(long)]
        no_hash: bool,
        /// 严谨级预设：quick（0 读）| fast（采样+缓存）| standard（每轮实采样每个文件，默认）
        /// | paranoid（每轮全读每个字节）
        #[arg(long, default_value = "standard")]
        rigor: String,
        /// 明细覆盖：内容证据 none | sampled | full（覆盖预设）
        #[arg(long)]
        evidence: Option<String>,
        /// 明细覆盖：哈希缓存 on | off（覆盖预设）
        #[arg(long)]
        cache: Option<String>,
        /// [兼容旧参] 无视缓存全部重读
        #[arg(long)]
        force_rehash: bool,
        /// [兼容旧参] 抽样+缓存（≈ --rigor fast）
        #[arg(long)]
        fast: bool,
        /// 记录 symlink 本身（指向字符串）；默认忽略 symlink
        #[arg(long)]
        symlinks_direct: bool,
        /// 系统垃圾排除预设：auto（Win+Mac，默认）| windows | mac | off
        #[arg(long, default_value = "auto")]
        os_excludes: String,
        /// 排除开发产物（.git/node_modules/target…）。默认关——.git 也是正常文件
        #[arg(long)]
        dev_excludes: bool,
        /// 追加排除（FFS 过滤器语法，如 */big_temp/ 或 */*.log，可多次）
        #[arg(long)]
        exclude: Vec<String>,
        /// 往 stderr 打哈希进度（百分比 + MiB/s）
        #[arg(long)]
        progress: bool,
    },
    /// 比对两张快照表，产出行动计划（JSONL）
    Compare {
        #[arg(long)]
        source: PathBuf,
        #[arg(long)]
        target: PathBuf,
        #[arg(long, value_enum, default_value = "mirror")]
        mode: Mode,
        /// 上次同步存档（sync 模式的增删归因依据，Unison 思路）
        #[arg(long)]
        archive: Option<PathBuf>,
        /// sync 无 archive 时，差异按"新者胜"解决（默认只报冲突）
        #[arg(long)]
        resolve_newer: bool,
        /// 大小写敏感匹配（默认不敏感——NTFS/APFS 默认行为）
        #[arg(long)]
        case_sensitive: bool,
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// 列出 .ffs-sync 标记的同步领地（CodeSync 生态）
    Territories {
        root: PathBuf,
    },
    /// 按 .ffs-sync 标记为每个领地生成 cs-<slug>.toml 任务（syncdash 版 CodeSync 生成器）
    GenJobs {
        root: PathBuf,
        #[arg(long)]
        target_root: PathBuf,
        #[arg(long, default_value = "sync")]
        mode: String,
        #[arg(long, default_value = "standard")]
        rigor: String,
        /// 生成远程管线任务：ssh 主机别名（如 mac）
        #[arg(long)]
        remote_host: Option<String>,
        /// 远端根路径前缀（远端本地路径，如 /Users/xxx/Code）
        #[arg(long)]
        remote_root_base: Option<String>,
        /// 远端 syncdash 路径（默认当它在 PATH 里）
        #[arg(long)]
        remote_exe: Option<String>,
    },
    /// 从 stdin 收一个文件写到 path（远程送包用：ssh 对面跑这个，双平台二进制级可靠）
    Recv {
        path: PathBuf,
    },
    /// 输出指定文件的 FastCDC 分块表（增量传输用；每文件一行 JSON）
    Chunks {
        #[arg(long)]
        root: PathBuf,
        /// 相对路径，可多次
        #[arg(long = "file")]
        files: Vec<String>,
    },
    /// 打包计划中 target 侧的操作为 tar 包（payload+计划+双 hash 清单），供对端 apply-pack
    Pack {
        plan: PathBuf,
        #[arg(long)]
        out: PathBuf,
        /// 覆盖计划头里的 source root
        #[arg(long)]
        source_root: Option<PathBuf>,
    },
    /// 在目标机执行包：验计划 hash → 逐文件验 hash → 执行（锁+回收+复制后校验）。默认 dry-run
    ApplyPack {
        pkg: PathBuf,
        /// 覆盖计划头里的 target root
        #[arg(long)]
        target_root: Option<PathBuf>,
        #[arg(long)]
        apply: bool,
        /// 成功后删除包文件（远程管线的清场步骤，免 shell 方言差异）
        #[arg(long)]
        remove_pkg: bool,
        /// 被删/被覆盖文件存进 target 的 .version_syncDash/（而非本机 trash）
        #[arg(long)]
        versioning: bool,
        #[arg(short, long)]
        verbose: bool,
    },
    /// 列出某 root 的版本历史（.version_syncDash）；--prune N 只保留最新 N 个
    Versions {
        root: PathBuf,
        #[arg(long)]
        prune: Option<usize>,
    },
    /// 从版本历史找回文件（默认 dry-run；--file 可多次，不给 = 整个版本）
    Restore {
        root: PathBuf,
        #[arg(long)]
        version: String,
        #[arg(long = "file")]
        files: Vec<String>,
        #[arg(long)]
        apply: bool,
    },
    /// 在 root 写下 `.syncdash-root` 挂载点标记（配 job 的 require_marker，防共享盘没挂上）
    Mark {
        root: PathBuf,
        /// 记进标记文件的任务名（仅供人看）
        #[arg(long, default_value = "")]
        job: String,
        #[arg(long, default_value = "")]
        note: String,
    },
    /// 运行历史（M4）：每次 apply 的结果一览；--prune-days N 清理旧记录
    History {
        /// 只看这个任务（缺省 = 全部）
        job: Option<String>,
        /// 最多显示多少条
        #[arg(long, default_value_t = 30)]
        limit: usize,
        /// 清理 N 天前的记录与明细文件
        #[arg(long)]
        prune_days: Option<u64>,
    },
    /// 本机回收目录：查看 / 找回 / 清理
    Trash {
        #[command(subcommand)]
        cmd: TrashCmd,
    },
    /// 执行计划。默认 dry-run，--apply 才动手
    Apply {
        plan: PathBuf,
        #[arg(long)]
        apply: bool,
        #[arg(long)]
        source_root: Option<PathBuf>,
        #[arg(long)]
        target_root: Option<PathBuf>,
        #[arg(long)]
        trash: Option<PathBuf>,
        /// 复制后重读校验 blake3（paranoid）
        #[arg(long)]
        verify: bool,
        /// 被删/被覆盖文件存进各 root 的 .version_syncDash/（而非本机 trash）
        #[arg(long)]
        versioning: bool,
        /// 大文件按 FastCDC 块增量写入（多读一遍目标换少写很多字节；SMB 上传划算）
        #[arg(long)]
        delta: bool,
        /// 临时文件 rename 前不 fsync（快，但断电可能丢最后的写入）
        #[arg(long)]
        no_fsync: bool,
        #[arg(short, long)]
        verbose: bool,
    },
}

#[derive(Subcommand)]
enum TrashCmd {
    /// 列出全部回收批次（时间、文件数、体积）
    Runs,
    /// 跨全部批次查找某个路径的历史版本（子串匹配，新的在前）
    Find { pattern: String },
    /// 找回文件到指定 root。默认 dry-run
    Restore {
        pattern: String,
        #[arg(long)]
        into: PathBuf,
        /// 指定批次 id（默认取最新一版）
        #[arg(long)]
        run: Option<String>,
        #[arg(long)]
        apply: bool,
    },
    /// 按保留策略清理。默认 dry-run
    Prune {
        /// 超过这么多天的批次一律删（0 = 关）
        #[arg(long, default_value = "30")]
        keep_days: i64,
        /// 总体积上限 GiB（0 = 关）
        #[arg(long, default_value = "10")]
        max_gib: u64,
        /// 关闭 staggered 稀释（近期密、远期疏）
        #[arg(long)]
        no_staggered: bool,
        #[arg(long)]
        apply: bool,
    },
}

fn write_out<F: Fn(&mut dyn std::io::Write) -> std::io::Result<()>>(out: &Option<PathBuf>, f: F) -> std::io::Result<()> {
    match out {
        Some(p) => {
            let file = std::fs::File::create(p)?;
            let mut w = std::io::BufWriter::new(file);
            f(&mut w)
        }
        None => {
            let stdout = std::io::stdout();
            let mut w = std::io::BufWriter::new(stdout.lock());
            f(&mut w)
        }
    }
}

fn main() {
    syncdash::scan::init_worker_pool();
    // CLI 有控制台：把库内诊断按原文接回 stderr——改造前的终端体验逐字不变。
    // 必须在任何库调用之前装，且 guard 活到进程结束（`_g` 不能写成 `_`：
    // `let _ = ...` 会当场 drop，sink 立刻被摘掉）。
    let cfg = syncdash::settings::load();
    let _g = cfg.mirror_stderr.then(|| {
        syncdash::logging::install(Arc::new(syncdash::logging::StderrSink { min_level: cfg.level }))
    });
    let cli = Cli::parse();
    let code = match run_cli(cli) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            2
        }
    };
    std::process::exit(code);
}

/// v0.9 M5：egui 退役后，GUI = Tauri 桌面版。找同目录的 syncdash-desktop 启动；
/// 找不到就说清楚去哪拿，而不是无声退出。
fn launch_desktop() -> std::io::Result<i32> {
    let exe_name = if cfg!(windows) { "syncdash-desktop.exe" } else { "syncdash-desktop" };
    let cand = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join(exe_name)));
    match cand {
        Some(p) if p.exists() => {
            std::process::Command::new(p).spawn()?;
            Ok(0)
        }
        _ => {
            eprintln!("desktop app not found next to this binary ({exe_name}).");
            eprintln!("build it with: cargo build -p syncdash-desktop --release");
            eprintln!("(CLI subcommands all still work — run `syncdash --help`)");
            Ok(2)
        }
    }
}

fn run_cli(cli: Cli) -> std::io::Result<i32> {
    let cmd = match cli.cmd {
        Some(c) => c,
        None => {
            // 双击 exe：无参数 → 启动 Tauri 桌面版（egui 旧界面已退役）
            detach_console();
            return launch_desktop();
        }
    };
    match cmd {
        Cmd::Probe => {
            let info = serde_json::json!({
                "app": "syncdash",
                "version": env!("CARGO_PKG_VERSION"),
                "schema": table::SCHEMA,
                "os": std::env::consts::OS,
                "arch": std::env::consts::ARCH,
                "host": table::host_name(),
                "exe": std::env::current_exe().ok().map(|p| p.to_string_lossy().into_owned()),
                "jobs_dir": config::jobs_dir().to_string_lossy().into_owned(),
            });
            println!("{}", serde_json::to_string_pretty(&info)?);
            Ok(0)
        }
        Cmd::Jobs => {
            let jobs = config::load_all();
            if jobs.is_empty() {
                println!("no jobs in {}\n\nsample job file:\n{}", config::jobs_dir().display(), config::SAMPLE);
            } else {
                for (name, j) in jobs {
                    println!("{:<24} {:<7} {}  ->  {}", name, j.mode, j.source.display(), j.target.display());
                }
            }
            Ok(0)
        }
        Cmd::Run { job, all, prefix, apply: do_apply, i_know, verbose, watch, interval, auto_apply } => {
            let list: Vec<(String, config::Job)> = if all || prefix.is_some() {
                config::load_all()
                    .into_iter()
                    .filter(|(n, _)| prefix.as_deref().map(|p| n.starts_with(p)).unwrap_or(true))
                    .collect()
            } else if let Some(j) = job {
                vec![config::load(&j)?]
            } else {
                eprintln!("error: give a job name, or use --all / --prefix <p>");
                return Ok(2);
            };
            if list.is_empty() {
                eprintln!("no matching jobs");
                return Ok(2);
            }
            // M6 值守：hash 缓存让未变的树每拍只付 walk 成本；RootLock 防两端同时动手
            if watch {
                let iv = interval
                    .or_else(|| list.iter().filter_map(|(_, j)| j.watch_interval_secs).min())
                    .unwrap_or(30)
                    .max(1);
                eprintln!("watch: {} job(s), every {iv}s — Ctrl-C to stop", list.len());
                loop {
                    for (name, j) in &list {
                        let auto = auto_apply || j.watch_auto_apply;
                        let res = if j.remote_host.is_some() {
                            run::run_remote_job(name, j, auto, verbose, i_know)
                        } else {
                            run::run_local_job(name, j, auto, verbose, i_know)
                        };
                        match res {
                            Ok((d, _s, e, c)) if d + e + c > 0 => {
                                eprintln!("[{name}] watch: {d} done, {e} error(s), {c} conflict(s)");
                            }
                            Ok(_) => {}
                            Err(err) => eprintln!("[{name}] watch error: {err}"),
                        }
                    }
                    std::thread::sleep(std::time::Duration::from_secs(iv));
                }
            }
            let many = list.len() > 1;
            let mut tot = (0u64, 0u64, 0u64, 0u64);
            for (name, j) in &list {
                let res = if j.remote_host.is_some() {
                    run::run_remote_job(name, j, do_apply, verbose, i_know)
                } else {
                    run::run_local_job(name, j, do_apply, verbose, i_know)
                };
                match res {
                    Ok((d, s, e, c)) => {
                        if do_apply {
                            println!("[{name}] applied: {d} done, {s} skipped, {e} error(s), {c} conflict(s)");
                        }
                        tot.0 += d;
                        tot.1 += s;
                        tot.2 += e;
                        tot.3 += c;
                    }
                    Err(err) => {
                        eprintln!("[{name}] FAILED: {err}");
                        tot.2 += 1;
                    }
                }
            }
            if many {
                println!(
                    "== total: {} job(s), {} done, {} skipped/pending, {} error(s), {} conflict(s)",
                    list.len(),
                    tot.0,
                    tot.1,
                    tot.2,
                    tot.3
                );
            }
            Ok(if tot.2 > 0 { 1 } else { 0 })
        }
        Cmd::Gui => launch_desktop(),
        Cmd::Scan { root, out, no_hash, rigor, evidence, cache, force_rehash, fast, symlinks_direct, os_excludes, dev_excludes, exclude, progress } => {
            if !root.is_dir() {
                eprintln!("error: not a directory: {}", root.display());
                return Ok(2);
            }
            // 预设铺底 → 明细覆盖 → 旧旗标兼容覆盖
            let (mut hash, mut sampled, mut use_cache) = match rigor.as_str() {
                "quick" => (false, false, false),
                "fast" => (true, true, true),
                "paranoid" => (true, false, false),
                _ => (true, true, false), // standard / custom
            };
            match evidence.as_deref() {
                Some("none") => { hash = false; sampled = false; }
                Some("full") => { hash = true; sampled = false; }
                Some(_) => { hash = true; sampled = true; }
                None => {}
            }
            match cache.as_deref() {
                Some("on") => use_cache = true,
                Some("off") => use_cache = false,
                _ => {}
            }
            if no_hash { hash = false; }
            if fast { sampled = true; use_cache = true; }
            if force_rehash { use_cache = false; }
            let sopt = scan::ScanOptions { hash, sampled, use_cache, symlinks_direct, filter: filter::PathFilter::build_full_opt(&[], &exclude, &[], &os_excludes, dev_excludes) };
            let bar = |p: scan::ScanProgress| {
                let pct = if p.bytes_total > 0 { p.bytes_done * 100 / p.bytes_total } else { 100 };
                eprint!("\r{} {:>3}%  {}/{}  {:.1} MiB/s   ", p.phase, pct,
                    syncdash::preflight::human_bytes(p.bytes_done),
                    syncdash::preflight::human_bytes(p.bytes_total), p.mib_per_s);
            };
            let snap = if progress {
                let r = scan::scan_with_progress(&root, &sopt, Some(&bar))?;
                eprintln!();
                r
            } else {
                scan::scan(&root, &sopt)?
            };
            eprintln!(
                "scanned {} entries in {} ms ({})",
                snap.header.entry_count, snap.header.duration_ms, snap.header.root
            );
            write_out(&out, |w| snap.write_to(w))?;
            Ok(0)
        }
        Cmd::Compare { source, target, mode, archive, resolve_newer, case_sensitive, out } => {
            let s = table::Snapshot::load(&source)?;
            let t = table::Snapshot::load(&target)?;
            let a = match &archive {
                Some(p) => Some(table::Snapshot::load(p)?),
                None => None,
            };
            let mode_str = match mode {
                Mode::Mirror => "mirror",
                Mode::Sync => "sync",
                Mode::Enrich => "enrich",
            };
            let plan = compare::compare(&s, &t, mode_str, a.as_ref(), resolve_newer, &compare::CompareOptions { case_insensitive: !case_sensitive, ..Default::default() });
            eprintln!(
                "plan: {} op(s), {} conflict(s)  [{} -> {}]",
                plan.header.op_count, plan.header.conflict_count, plan.header.source_root, plan.header.target_root
            );
            write_out(&out, |w| plan.write_to(w))?;
            Ok(if plan.header.conflict_count > 0 { 1 } else { 0 })
        }
        Cmd::Territories { root } => {
            let ts = territory::find_territories(&root);
            if ts.is_empty() {
                println!("no .ffs-sync territories under {}", root.display());
            } else {
                for t in &ts {
                    println!("{t}");
                }
                eprintln!("{} territor(ies)", ts.len());
            }
            Ok(0)
        }
        Cmd::GenJobs { root, target_root, mode, rigor, remote_host, remote_root_base, remote_exe } => {
            let remote = remote_host.map(|h| territory::RemoteGen {
                host: h,
                root_base: remote_root_base.unwrap_or_default(),
                exe: remote_exe,
            });
            let outs = territory::gen_jobs(&root, &target_root, &mode, &rigor, remote.as_ref())?;
            for o in &outs {
                println!("{:<44} <- {}", o.name, o.territory);
            }
            println!("{} job(s) written to {}", outs.len(), config::jobs_dir().display());
            Ok(0)
        }
        Cmd::Pack { plan, out, source_root } => {
            let p = compare::Plan::load(&plan)?;
            let sr = source_root.unwrap_or_else(|| PathBuf::from(&p.header.source_root));
            let s = pack::pack(&p, &sr, &out, None)?;
            println!("packed: {} op(s), {} payload file(s), {} bytes -> {}", s.ops, s.files, s.bytes, out.display());
            Ok(0)
        }
        Cmd::Recv { path } => {
            if let Some(p) = path.parent() {
                std::fs::create_dir_all(p)?;
            }
            let mut f = std::fs::File::create(&path)?;
            let n = std::io::copy(&mut std::io::stdin().lock(), &mut f)?;
            eprintln!("received {n} bytes -> {}", path.display());
            Ok(0)
        }
        Cmd::Chunks { root, files } => {
            let stdout = std::io::stdout();
            let mut w = std::io::BufWriter::new(stdout.lock());
            for rel in &files {
                match syncdash::chunk::chunk_file(&root, rel) {
                    Ok(fc) => {
                        use std::io::Write;
                        writeln!(w, "{}", serde_json::to_string(&fc)?)?;
                    }
                    Err(e) => eprintln!("warning: chunking {rel} failed: {e}"),
                }
            }
            Ok(0)
        }
        Cmd::Versions { root, prune } => {
            if let Some(keep) = prune {
                let dropped = syncdash::version::prune(&root, keep)?;
                println!("pruned {} version(s), kept newest {keep}", dropped.len());
            }
            let list = syncdash::version::list(&root)?;
            if list.is_empty() {
                println!("no versions under {}", root.join(syncdash::version::STORE_DIR).display());
            } else {
                for v in &list {
                    println!("{}  {}  ops={} preserved={} bytes={}", v.id, v.host, v.ops, v.preserved, v.bytes);
                }
            }
            Ok(0)
        }
        Cmd::Restore { root, version, files, apply: do_apply } => {
            let (restored, skipped, errors) = syncdash::version::restore(&root, &version, &files, !do_apply)?;
            println!(
                "{}: {restored} restored, {skipped} skipped, {errors} error(s)",
                if do_apply { "restore" } else { "dry-run (rerun with --apply)" }
            );
            Ok(if errors > 0 { 1 } else { 0 })
        }
        Cmd::ApplyPack { pkg, target_root, apply: do_apply, remove_pkg, versioning, verbose } => {
            let (done, skipped, errors) = pack::apply_pack(&pkg, target_root.as_deref(), do_apply, verbose, versioning)?;
            println!(
                "{}: {done} done, {skipped} skipped, {errors} error(s)",
                if do_apply { "applied" } else { "dry-run" }
            );
            if remove_pkg && errors == 0 && do_apply {
                let _ = std::fs::remove_file(&pkg);
            }
            Ok(if errors > 0 { 1 } else { 0 })
        }
        Cmd::Mark { root, job, note } => {
            let (path, created) = syncdash::preflight::write_marker(&root, &job, &note)?;
            if created {
                println!("marked: {}", path.display());
            } else {
                let m = syncdash::preflight::read_marker(&root);
                println!(
                    "already marked: {}{}",
                    path.display(),
                    m.map(|m| format!("  (job '{}', by {} )", m.job, m.host)).unwrap_or_default()
                );
            }
            println!("set `require_marker = true` in the job to have syncdash refuse to run without it");
            Ok(0)
        }
        Cmd::History { job, limit, prune_days } => {
            if let Some(days) = prune_days {
                // 0 = 不叠总量闸门：`--prune-days N` 的语义就是"只按天"
                let n = syncdash::runlog::prune(days, 0);
                println!("pruned {n} run(s) older than {days} day(s)");
            }
            let rows = syncdash::runlog::history(job.as_deref(), limit);
            if rows.is_empty() {
                println!("no runs recorded yet (runs are logged when a job actually applies)");
                return Ok(0);
            }
            let now = syncdash::table::now_ms() as i64;
            for r in rows {
                let age_min = (now - r.ts_ms).max(0) / 60_000;
                let age = if age_min < 60 {
                    format!("{age_min}m ago")
                } else if age_min < 48 * 60 {
                    format!("{}h ago", age_min / 60)
                } else {
                    format!("{}d ago", age_min / 60 / 24)
                };
                println!(
                    "{:>9}  {:<20} {:<12} {:>5} done {:>4} skip {:>3} err  {:>10}  {:>7.1}s{}",
                    age,
                    r.job,
                    r.kind,
                    r.done,
                    r.skipped,
                    r.errors,
                    syncdash::preflight::human_bytes(r.bytes),
                    r.elapsed_ms as f64 / 1000.0,
                    if r.cancelled { "  [cancelled]" } else { "" },
                );
            }
            Ok(0)
        }
        Cmd::Trash { cmd } => {
            use syncdash::preflight::human_bytes;
            match cmd {
                TrashCmd::Runs => {
                    let runs = syncdash::trash::list_runs();
                    if runs.is_empty() {
                        println!("no trash runs under {}", syncdash::trash::trash_root().display());
                    }
                    let mut total = 0u64;
                    for r in &runs {
                        println!("{:<16} {:>7} files  {:>10}", r.id, r.files, human_bytes(r.bytes));
                        total += r.bytes;
                    }
                    if !runs.is_empty() {
                        println!("== {} run(s), {} total", runs.len(), human_bytes(total));
                    }
                    Ok(0)
                }
                TrashCmd::Find { pattern } => {
                    let hits = syncdash::trash::find(&pattern);
                    for h in &hits {
                        println!("{:<16} {:>10}  {}", h.run_id, human_bytes(h.size), h.rel);
                    }
                    println!("{} version(s)", hits.len());
                    Ok(0)
                }
                TrashCmd::Restore { pattern, into, run, apply: do_apply } => {
                    let (r, s, e) = syncdash::trash::restore(&pattern, run.as_deref(), &into, !do_apply)?;
                    println!(
                        "{}: {r} restored, {s} skipped, {e} error(s)",
                        if do_apply { "restore" } else { "dry-run (rerun with --apply)" }
                    );
                    Ok(if e > 0 { 1 } else { 0 })
                }
                TrashCmd::Prune { keep_days, max_gib, no_staggered, apply: do_apply } => {
                    let ret = syncdash::trash::Retention {
                        keep_days,
                        max_bytes: max_gib * 1024 * 1024 * 1024,
                        staggered: !no_staggered,
                    };
                    let (n, freed) = syncdash::trash::prune(&ret, !do_apply)?;
                    println!(
                        "{}: {n} run(s), {} freed",
                        if do_apply { "pruned" } else { "dry-run (rerun with --apply)" },
                        human_bytes(freed)
                    );
                    Ok(0)
                }
            }
        }
        Cmd::Apply { plan, apply: do_apply, source_root, target_root, trash, verify, versioning, delta, no_fsync, verbose } => {
            let p = compare::Plan::load(&plan)?;
            let sr = source_root.unwrap_or_else(|| PathBuf::from(&p.header.source_root));
            let tr = target_root.unwrap_or_else(|| PathBuf::from(&p.header.target_root));
            for (name, r) in [("source", &sr), ("target", &tr)] {
                if !r.is_dir() {
                    eprintln!("error: {name} root not accessible locally: {} (remote package mode lands in v0.4)", r.display());
                    return Ok(2);
                }
            }
            let (done, skipped, errors) = apply::apply(&p.ops, &sr, &tr, &apply::ApplyOptions { dry_run: !do_apply, trash, verbose, verify, versioning, delta, fsync: !no_fsync, ..Default::default() });
            println!(
                "{}: {done} done, {skipped} {}, {errors} error(s)",
                if do_apply { "applied" } else { "dry-run" },
                if do_apply { "skipped" } else { "pending (rerun with --apply)" },
            );
            Ok(if errors > 0 { 1 } else { 0 })
        }
    }
}
