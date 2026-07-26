mod apply;
mod compare;
mod scan;
mod table;

use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "syncdash", version, about = "Table-driven multi-node file sync (scan -> compare -> apply)")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
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
    /// 扫描目录，产出快照表（JSONL，默认 stdout —— ssh 管道友好）
    Scan {
        root: PathBuf,
        #[arg(long)]
        out: Option<PathBuf>,
        /// 跳过内容 hash（快，但比对退化为 size+mtime，且无法做移动检测）
        #[arg(long)]
        no_hash: bool,
        /// 追加排除的目录名（可多次）
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
        /// 覆盖计划头里的 source root（如换了挂载点）
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
    let code = match run(cli) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            2
        }
    };
    std::process::exit(code);
}

fn run(cli: Cli) -> std::io::Result<i32> {
    match cli.cmd {
        Cmd::Probe => {
            let info = serde_json::json!({
                "app": "syncdash",
                "version": env!("CARGO_PKG_VERSION"),
                "schema": table::SCHEMA,
                "os": std::env::consts::OS,
                "arch": std::env::consts::ARCH,
                "host": table::host_name(),
                "exe": std::env::current_exe().ok().map(|p| p.to_string_lossy().into_owned()),
            });
            println!("{}", serde_json::to_string_pretty(&info)?);
            Ok(0)
        }
        Cmd::Scan { root, out, no_hash, exclude } => {
            if !root.is_dir() {
                eprintln!("error: not a directory: {}", root.display());
                return Ok(2);
            }
            let snap = scan::scan(&root, &scan::ScanOptions { hash: !no_hash, extra_excludes: exclude })?;
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
            let (done, skipped, errors) = apply::apply(&p, &sr, &tr, &apply::ApplyOptions { dry_run: !do_apply, trash, verbose });
            println!(
                "{}: {done} done, {skipped} {}, {errors} error(s)",
                if do_apply { "applied" } else { "dry-run" },
                if do_apply { "skipped" } else { "pending (rerun with --apply)" },
            );
            Ok(if errors > 0 { 1 } else { 0 })
        }
    }
}
