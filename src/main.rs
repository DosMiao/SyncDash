mod apply;
mod compare;
mod config;
mod filter;
mod gui;
mod lock;
mod run;
mod scan;
mod table;

use clap::{Parser, Subcommand, ValueEnum};
use compare::{Action, Op};
use std::path::PathBuf;

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
    /// 跑一个任务：扫双侧 → 比对 →（--apply 时）执行 + 刷新 archive
    Run {
        /// 任务名（jobs 目录里的文件名）或 toml 路径
        job: String,
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
        #[arg(long)]
        out: Option<PathBuf>,
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
        Cmd::Run { job, apply: do_apply, verbose } => {
            let (name, j) = config::load(&job)?;
            let plan = run::compare_job(&j)?;
            eprintln!("[{name}] {} op(s), {} conflict(s)", plan.header.op_count, plan.header.conflict_count);
            for op in &plan.ops {
                println!("{}", serde_json::to_string(op)?);
            }
            if do_apply {
                let ops: Vec<Op> = plan
                    .ops
                    .iter()
                    .filter(|o| !matches!(o.action, Action::Conflict | Action::Note))
                    .cloned()
                    .collect();
                let (done, skipped, errors) = run::apply_job(&j, &plan, &ops, None, verbose);
                println!("applied: {done} done, {skipped} skipped, {errors} error(s)");
                Ok(if errors > 0 { 1 } else { 0 })
            } else {
                println!("dry-run (rerun with --apply)");
                Ok(if plan.header.conflict_count > 0 { 1 } else { 0 })
            }
        }
        Cmd::Gui { job } => {
            gui::run_gui(job).map_err(|e| std::io::Error::other(e.to_string()))?;
            Ok(0)
        }
        Cmd::Scan { root, out, no_hash, exclude } => {
            if !root.is_dir() {
                eprintln!("error: not a directory: {}", root.display());
                return Ok(2);
            }
            let snap = scan::scan(&root, &scan::ScanOptions { hash: !no_hash, filter: filter::PathFilter::build(&[], &exclude) })?;
            eprintln!(
                "scanned {} entries in {} ms ({})",
                snap.header.entry_count, snap.header.duration_ms, snap.header.root
            );
            write_out(&out, |w| snap.write_to(w))?;
            Ok(0)
        }
        Cmd::Compare { source, target, mode, archive, resolve_newer, out } => {
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
            let plan = compare::compare(&s, &t, mode_str, a.as_ref(), resolve_newer);
            eprintln!(
                "plan: {} op(s), {} conflict(s)  [{} -> {}]",
                plan.header.op_count, plan.header.conflict_count, plan.header.source_root, plan.header.target_root
            );
            write_out(&out, |w| plan.write_to(w))?;
            Ok(if plan.header.conflict_count > 0 { 1 } else { 0 })
        }
        Cmd::Apply { plan, apply: do_apply, source_root, target_root, trash, verbose } => {
            let p = compare::Plan::load(&plan)?;
            let sr = source_root.unwrap_or_else(|| PathBuf::from(&p.header.source_root));
            let tr = target_root.unwrap_or_else(|| PathBuf::from(&p.header.target_root));
            for (name, r) in [("source", &sr), ("target", &tr)] {
                if !r.is_dir() {
                    eprintln!("error: {name} root not accessible locally: {} (remote package mode lands in v0.4)", r.display());
                    return Ok(2);
                }
            }
            let (done, skipped, errors) = apply::apply(&p.ops, &sr, &tr, &apply::ApplyOptions { dry_run: !do_apply, trash, verbose });
            println!(
                "{}: {done} done, {skipped} {}, {errors} error(s)",
                if do_apply { "applied" } else { "dry-run" },
                if do_apply { "skipped" } else { "pending (rerun with --apply)" },
            );
            Ok(if errors > 0 { 1 } else { 0 })
        }
    }
}
