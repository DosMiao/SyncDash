mod gui;

use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;
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
        #[arg(short, long)]
        verbose: bool,
    },
    /// 打开图形界面（参考 FFS：Compare → 勾选 → Synchronize）
    Gui {
        /// 启动时选中的任务名
        job: Option<String>,
    },
    /// 扫描目录，产出快照表（JSONL，默认 stdout —— ssh 管道友好）
    Scan {
        root: PathBuf,
        #[arg(long)]
        out: Option<PathBuf>,
        /// 跳过内容 hash（快，但比对退化为 size+mtime，且无法做移动检测）
        #[arg(long)]
        no_hash: bool,
        /// 无视 hash 缓存，全部重新 hash（paranoid）
        #[arg(long)]
        force_rehash: bool,
        /// 记录 symlink 本身（指向字符串）；默认忽略 symlink
        #[arg(long)]
        symlinks_direct: bool,
        /// 追加排除（FFS 过滤器语法，如 */big_temp/ 或 */*.log，可多次）
        #[arg(long)]
        exclude: Vec<String>,
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
        #[arg(short, long)]
        verbose: bool,
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
        #[arg(short, long)]
        verbose: bool,
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

fn run_cli(cli: Cli) -> std::io::Result<i32> {
    let cmd = match cli.cmd {
        Some(c) => c,
        None => {
            // 双击 exe：无参数 → 直接进 GUI
            detach_console();
            gui::run_gui(None).map_err(|e| std::io::Error::other(e.to_string()))?;
            return Ok(0);
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
        Cmd::Run { job, all, prefix, apply: do_apply, verbose } => {
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
            let many = list.len() > 1;
            let mut tot = (0u64, 0u64, 0u64, 0u64);
            for (name, j) in &list {
                let res = if j.remote_host.is_some() {
                    run::run_remote_job(name, j, do_apply, verbose)
                } else {
                    run::run_local_job(name, j, do_apply, verbose)
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
        Cmd::Gui { job } => {
            gui::run_gui(job).map_err(|e| std::io::Error::other(e.to_string()))?;
            Ok(0)
        }
        Cmd::Scan { root, out, no_hash, force_rehash, symlinks_direct, exclude } => {
            if !root.is_dir() {
                eprintln!("error: not a directory: {}", root.display());
                return Ok(2);
            }
            let snap = scan::scan(&root, &scan::ScanOptions { hash: !no_hash, force_rehash, symlinks_direct, filter: filter::PathFilter::build(&[], &exclude) })?;
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
            let plan = compare::compare(&s, &t, mode_str, a.as_ref(), resolve_newer, &compare::CompareOptions { case_insensitive: !case_sensitive });
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
            let s = pack::pack(&p, &sr, &out)?;
            println!("packed: {} op(s), {} payload file(s), {} bytes -> {}", s.ops, s.files, s.bytes, out.display());
            Ok(0)
        }
        Cmd::ApplyPack { pkg, target_root, apply: do_apply, verbose } => {
            let (done, skipped, errors) = pack::apply_pack(&pkg, target_root.as_deref(), do_apply, verbose)?;
            println!(
                "{}: {done} done, {skipped} skipped, {errors} error(s)",
                if do_apply { "applied" } else { "dry-run" }
            );
            Ok(if errors > 0 { 1 } else { 0 })
        }
        Cmd::Apply { plan, apply: do_apply, source_root, target_root, trash, verify, verbose } => {
            let p = compare::Plan::load(&plan)?;
            let sr = source_root.unwrap_or_else(|| PathBuf::from(&p.header.source_root));
            let tr = target_root.unwrap_or_else(|| PathBuf::from(&p.header.target_root));
            for (name, r) in [("source", &sr), ("target", &tr)] {
                if !r.is_dir() {
                    eprintln!("error: {name} root not accessible locally: {} (remote package mode lands in v0.4)", r.display());
                    return Ok(2);
                }
            }
            let (done, skipped, errors) = apply::apply(&p.ops, &sr, &tr, &apply::ApplyOptions { dry_run: !do_apply, trash, verbose, verify });
            println!(
                "{}: {done} done, {skipped} {}, {errors} error(s)",
                if do_apply { "applied" } else { "dry-run" },
                if do_apply { "skipped" } else { "pending (rerun with --apply)" },
            );
            Ok(if errors > 0 { 1 } else { 0 })
        }
    }
}
