
use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;
use std::sync::Arc;
use syncdash::job::{self, territory};
use syncdash::model::table;
use syncdash::pipeline::{apply, compare, filter, scan};
use syncdash::run;
use syncdash::transfer::pack;

#[derive(Parser)]
#[command(name = "syncdash", version, about = "Table-driven multi-node file sync (scan -> compare -> apply)")]
struct Cli {
    /// With no subcommand (e.g. double-clicking the exe), open the GUI directly
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn FreeConsole() -> i32;
}

/// Drop the console window that tags along when launched by double-click
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
    /// Print this machine's environment info (used for remote probing: this is what runs over ssh on the far side)
    Probe,
    /// List job configs (%APPDATA%\syncdash\jobs\*.toml)
    Jobs,
    /// Run a job: scan both sides → compare → (with --apply) execute + refresh the archive. A job with remote_host set takes the ssh remote pipeline
    Run {
        /// Job name (a filename in the jobs directory) or a toml path; omit it and use --all / --prefix
        job: Option<String>,
        /// Run every job
        #[arg(long)]
        all: bool,
        /// Run only jobs whose name starts with this (e.g. cs-)
        #[arg(long)]
        prefix: Option<String>,
        #[arg(long)]
        apply: bool,
        /// Allow the "plan health check" through (deletion share too high). A missing marker / insufficient space still blocks
        #[arg(long = "i-know")]
        i_know: bool,
        #[arg(short, long)]
        verbose: bool,
        /// M6 watch: loop compare → (on differences, in auto mode) apply → sleep. Ctrl-C to stop
        #[arg(long)]
        watch: bool,
        /// Watch interval in seconds (defaults to the job's watch_interval_secs, then 30)
        #[arg(long)]
        interval: Option<u64>,
        /// Apply automatically when watch finds differences (same as watch_auto_apply = true in the job)
        #[arg(long = "auto-apply")]
        auto_apply: bool,
    },
    /// Open the graphical interface (the Tauri desktop app; the old egui UI was retired in v0.9)
    Gui,
    /// Scan a directory and produce a snapshot table (JSONL, stdout by default — friendly to ssh pipes)
    Scan {
        root: PathBuf,
        #[arg(long)]
        out: Option<PathBuf>,
        /// Skip content hashing (fast, but comparison degrades to size+mtime and move detection becomes impossible)
        #[arg(long)]
        no_hash: bool,
        /// Rigor preset: quick (0 reads) | fast (sampling + cache) | standard (really samples every file each round, the default)
        /// | paranoid (reads every byte each round)
        #[arg(long, default_value = "standard")]
        rigor: String,
        /// Detail override: content evidence none | sampled | full (overrides the preset)
        #[arg(long)]
        evidence: Option<String>,
        /// Detail override: hash cache on | off (overrides the preset)
        #[arg(long)]
        cache: Option<String>,
        /// [legacy flag] Ignore the cache and re-read everything
        #[arg(long)]
        force_rehash: bool,
        /// [legacy flag] Sampling + cache (≈ --rigor fast)
        #[arg(long)]
        fast: bool,
        /// Record the symlink itself (its target string); symlinks are ignored by default
        #[arg(long)]
        symlinks_direct: bool,
        /// OS-junk exclude preset: auto (Win+Mac, the default) | windows | mac | off
        #[arg(long, default_value = "auto")]
        os_excludes: String,
        /// Exclude dev artifacts (.git/node_modules/target…). Off by default — .git is a normal tree too
        #[arg(long)]
        dev_excludes: bool,
        /// Extra excludes (FFS filter syntax, e.g. */big_temp/ or */*.log; repeatable)
        #[arg(long)]
        exclude: Vec<String>,
        /// Print hashing progress to stderr (percentage + MiB/s)
        #[arg(long)]
        progress: bool,
    },
    /// Compare two snapshot tables and produce an action plan (JSONL)
    Compare {
        #[arg(long)]
        source: PathBuf,
        #[arg(long)]
        target: PathBuf,
        #[arg(long, value_enum, default_value = "mirror")]
        mode: Mode,
        /// The last sync's archive (what sync mode attributes adds and deletes against; the Unison approach)
        #[arg(long)]
        archive: Option<PathBuf>,
        /// With no archive in sync mode, resolve differences by "newer wins" (by default it only reports conflicts)
        #[arg(long)]
        resolve_newer: bool,
        /// Case-sensitive matching (insensitive by default — the NTFS/APFS behavior)
        #[arg(long)]
        case_sensitive: bool,
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// List the sync territories marked by .ffs-sync (the CodeSync ecosystem)
    Territories {
        root: PathBuf,
    },
    /// Generate a cs-<slug>.toml job for every .ffs-sync-marked territory (syncdash's take on the CodeSync generator)
    GenJobs {
        root: PathBuf,
        #[arg(long)]
        target_root: PathBuf,
        #[arg(long, default_value = "sync")]
        mode: String,
        #[arg(long, default_value = "standard")]
        rigor: String,
        /// Generate remote-pipeline jobs: the ssh host alias (e.g. mac)
        #[arg(long)]
        remote_host: Option<String>,
        /// Remote root path prefix (the remote's own local path, e.g. /Users/xxx/Code)
        #[arg(long)]
        remote_root_base: Option<String>,
        /// Path to the remote syncdash (defaults to assuming it is on PATH)
        #[arg(long)]
        remote_exe: Option<String>,
    },
    /// Receive a file on stdin and write it to path (used to ship remote packages: this runs over ssh on the far side, binary-safe on both platforms)
    Recv {
        path: PathBuf,
    },
    /// Emit the FastCDC chunk table for the given files (used for delta transfer; one JSON line per file)
    Chunks {
        #[arg(long)]
        root: PathBuf,
        /// Relative path; repeatable
        #[arg(long = "file")]
        files: Vec<String>,
    },
    /// Pack the plan's target-side operations into a tar (payload + plan + a two-hash manifest) for the far end's apply-pack
    Pack {
        plan: PathBuf,
        #[arg(long)]
        out: PathBuf,
        /// Override the source root in the plan header
        #[arg(long)]
        source_root: Option<PathBuf>,
    },
    /// Execute a package on the target machine: verify the plan hash → verify each file's hash → execute (lock + trash + verify after copy). dry-run by default
    ApplyPack {
        pkg: PathBuf,
        /// Override the target root in the plan header
        #[arg(long)]
        target_root: Option<PathBuf>,
        #[arg(long)]
        apply: bool,
        /// Delete the package file on success (the remote pipeline's cleanup step, free of shell-dialect differences)
        #[arg(long)]
        remove_pkg: bool,
        /// Put deleted/overwritten files in target's .version_syncDash/ (instead of the local trash)
        #[arg(long)]
        versioning: bool,
        #[arg(short, long)]
        verbose: bool,
    },
    /// List a root's version history (.version_syncDash); --prune N keeps only the newest N
    Versions {
        root: PathBuf,
        #[arg(long)]
        prune: Option<usize>,
    },
    /// Recover files from the version history (dry-run by default; --file is repeatable, omit it for the whole version)
    Restore {
        root: PathBuf,
        #[arg(long)]
        version: String,
        #[arg(long = "file")]
        files: Vec<String>,
        #[arg(long)]
        apply: bool,
    },
    /// Write the `.syncdash-root` mount-point marker in root (pairs with the job's require_marker to guard against an unmounted share)
    Mark {
        root: PathBuf,
        /// Job name recorded in the marker file (for humans only)
        #[arg(long, default_value = "")]
        job: String,
        #[arg(long, default_value = "")]
        note: String,
    },
    /// Run history (M4): an overview of every apply's outcome; --prune-days N prunes old records
    History {
        /// Only this job (omit for all)
        job: Option<String>,
        /// Maximum number of rows to show
        #[arg(long, default_value_t = 30)]
        limit: usize,
        /// Prune records and detail files older than N days
        #[arg(long)]
        prune_days: Option<u64>,
    },
    /// Central logs (v0.10): list runs / view one run's three manifests / prune / show where the directory is
    Logs {
        #[command(subcommand)]
        cmd: LogsCmd,
    },
    /// The local trash: view / recover / prune
    Trash {
        #[command(subcommand)]
        cmd: TrashCmd,
    },
    /// Execute a plan. dry-run by default; only --apply touches anything
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
        /// Re-read and verify blake3 after copying (paranoid)
        #[arg(long)]
        verify: bool,
        /// Put deleted/overwritten files in each root's .version_syncDash/ (instead of the local trash)
        #[arg(long)]
        versioning: bool,
        /// Write large files delta-wise in FastCDC chunks (one extra read of the target buys a lot fewer written bytes; pays off for SMB uploads)
        #[arg(long)]
        delta: bool,
        /// Do not fsync the temp file before renaming (fast, but a power cut may lose the last writes)
        #[arg(long)]
        no_fsync: bool,
        #[arg(short, long)]
        verbose: bool,
    },
}

#[derive(Subcommand)]
enum LogsCmd {
    /// List runs (newest → oldest). **Including interrupted runs** — the ones missing from the index, with only a directory left
    List {
        /// Only this job (omit for all)
        job: Option<String>,
        #[arg(long, default_value_t = 30)]
        limit: usize,
    },
    /// View one run's artifacts. Shows the event stream by default
    Show {
        /// Run id (= the directory name, the column in `syncdash logs list`)
        run_id: String,
        /// The error manifest
        #[arg(long)]
        errors: bool,
        /// The execution manifest (one line per op, its actual outcome)
        #[arg(long)]
        items: bool,
        /// The plan manifest (what this run **intended** to do — compare it with the execution manifest to see what never got its turn)
        #[arg(long)]
        plan: bool,
        #[arg(long, default_value_t = 5000)]
        limit: usize,
    },
    /// Prune by the retention policy (defaults come from keep_days / max_total_mb in settings)
    Prune {
        #[arg(long)]
        keep_days: Option<u64>,
        #[arg(long)]
        max_total_mb: Option<u64>,
    },
    /// Print the log directory's location
    Dir,
}

#[derive(Subcommand)]
enum TrashCmd {
    /// List every trash batch (time, file count, size)
    Runs,
    /// Search all batches for a path's historical versions (substring match, newest first)
    Find { pattern: String },
    /// Recover files into the given root. dry-run by default
    Restore {
        pattern: String,
        #[arg(long)]
        into: PathBuf,
        /// A specific batch id (defaults to the newest version)
        #[arg(long)]
        run: Option<String>,
        #[arg(long)]
        apply: bool,
    },
    /// Prune by the retention policy. dry-run by default
    Prune {
        /// Delete every batch older than this many days (0 = off)
        #[arg(long, default_value = "30")]
        keep_days: i64,
        /// Total size cap in GiB (0 = off)
        #[arg(long, default_value = "10")]
        max_gib: u64,
        /// Turn off staggered thinning (dense recently, sparse further back)
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

/// v0.10: the CLI facade for central logging. Every read goes through runlog's public API,
/// which is the layer holding the path-escape guard (`artifact_lines` accepts only bare-filename-shaped run_ids).
fn run_logs(cmd: LogsCmd) -> std::io::Result<i32> {
    use syncdash::obs::runlog;
    match cmd {
        LogsCmd::List { job, limit } => {
            // history_merged rather than history: an interrupted run has only a directory and no index line,
            // and that is precisely the run that most needs to be seen
            let rows = runlog::history_merged(job.as_deref(), limit);
            if rows.is_empty() {
                println!("no runs recorded yet (runs are logged when a job actually applies)");
                return Ok(0);
            }
            let now = syncdash::foundation::time::now_ms() as i64;
            for r in &rows {
                let age_min = (now - r.ts_ms).max(0) / 60_000;
                let age = if age_min < 60 {
                    format!("{age_min}m ago")
                } else if age_min < 48 * 60 {
                    format!("{}h ago", age_min / 60)
                } else {
                    format!("{}d ago", age_min / 60 / 24)
                };
                // compare rows have no directory; "-" holds the slot so the column stays visually aligned
                let state = if !r.finished {
                    "  [INTERRUPTED]"
                } else if r.cancelled {
                    "  [cancelled]"
                } else {
                    ""
                };
                let what = match r.ops_found {
                    Some(n) => format!("{n:>5} found"),
                    None => format!("{:>5} done ", r.done),
                };
                println!(
                    "{:>9}  {:<28} {:<16} {:<12} {what} {:>3} err {:>3} warn  {:>10}  {:>7.1}s{state}",
                    age,
                    r.run_id.as_deref().unwrap_or("-"),
                    r.job,
                    r.kind,
                    r.errors,
                    r.warnings,
                    syncdash::foundation::fmt::human_bytes(r.bytes),
                    r.elapsed_ms as f64 / 1000.0,
                );
            }
            println!("\n{} run(s) · logs at {}", rows.len(), runlog::logs_dir().display());
            Ok(0)
        }
        LogsCmd::Show { run_id, errors, items, plan, limit } => {
            let which = if errors {
                "errors"
            } else if items {
                "items"
            } else if plan {
                "plan"
            } else {
                "run"
            };
            let lines = runlog::artifact_lines(&run_id, which, limit);
            if lines.is_empty() {
                eprintln!("no {which} lines for run '{run_id}' (wrong id, or that artifact is empty)");
                return Ok(1);
            }
            for l in lines {
                println!("{l}");
            }
            Ok(0)
        }
        LogsCmd::Prune { keep_days, max_total_mb } => {
            let cfg = syncdash::store::settings::load();
            let days = keep_days.unwrap_or(cfg.keep_days);
            let cap = max_total_mb.unwrap_or(cfg.max_total_mb);
            let n = runlog::prune(days, cap);
            println!("pruned {n} run(s)  (keep_days={days}, max_total_mb={cap})");
            Ok(0)
        }
        LogsCmd::Dir => {
            println!("{}", runlog::logs_dir().display());
            println!("settings: {}", syncdash::store::settings::settings_path().display());
            Ok(0)
        }
    }
}

fn main() {
    syncdash::pipeline::scan::init_worker_pool();
    // The CLI has a console: pipe the library's diagnostics back to stderr verbatim — the pre-refactor terminal experience, word for word.
    // It must be installed before any library call, and the guard must live to process exit (`_g` cannot be
    // written `_`: `let _ = ...` drops on the spot and the sink is pulled straight back out).
    let cfg = syncdash::store::settings::load();
    let _g = cfg.mirror_stderr.then(|| {
        syncdash::obs::progress::install(Arc::new(syncdash::obs::logging::StderrSink { min_level: cfg.level }))
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

/// v0.9 M5: with egui retired, GUI = the Tauri desktop app. Look for syncdash-desktop next to this binary and launch it;
/// if it isn't there, say plainly where to get it instead of exiting silently.
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
            // Double-clicked exe: no arguments → launch the Tauri desktop app (the old egui UI is retired)
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
                "jobs_dir": syncdash::foundation::dirs::jobs_dir().to_string_lossy().into_owned(),
            });
            println!("{}", serde_json::to_string_pretty(&info)?);
            Ok(0)
        }
        Cmd::Jobs => {
            let jobs = job::load_all();
            if jobs.is_empty() {
                println!("no jobs in {}\n\nsample job file:\n{}", syncdash::foundation::dirs::jobs_dir().display(), job::SAMPLE);
            } else {
                for (name, j) in jobs {
                    println!("{:<24} {:<7} {}  ->  {}", name, j.mode, j.source.display(), j.target.display());
                }
            }
            Ok(0)
        }
        Cmd::Run { job, all, prefix, apply: do_apply, i_know, verbose, watch, interval, auto_apply } => {
            let list: Vec<(String, job::Job)> = if all || prefix.is_some() {
                job::load_all()
                    .into_iter()
                    .filter(|(n, _)| prefix.as_deref().map(|p| n.starts_with(p)).unwrap_or(true))
                    .collect()
            } else if let Some(j) = job {
                vec![job::load(&j)?]
            } else {
                eprintln!("error: give a job name, or use --all / --prefix <p>");
                return Ok(2);
            };
            if list.is_empty() {
                eprintln!("no matching jobs");
                return Ok(2);
            }
            // M6 watch: the hash cache means an unchanged tree only pays the walk each tick; RootLock stops both ends acting at once
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
            // preset lays the base → detail overrides → legacy-flag compatibility overrides
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
                    syncdash::foundation::fmt::human_bytes(p.bytes_done),
                    syncdash::foundation::fmt::human_bytes(p.bytes_total), p.mib_per_s);
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
            println!("{} job(s) written to {}", outs.len(), syncdash::foundation::dirs::jobs_dir().display());
            Ok(0)
        }
        Cmd::Pack { plan, out, source_root } => {
            let p = syncdash::model::plan::Plan::load(&plan)?;
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
                match syncdash::model::chunk::chunk_file(&root, rel) {
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
                let dropped = syncdash::store::version::prune(&root, keep)?;
                println!("pruned {} version(s), kept newest {keep}", dropped.len());
            }
            let list = syncdash::store::version::list(&root)?;
            if list.is_empty() {
                println!("no versions under {}", root.join(syncdash::foundation::names::VERSION_STORE_DIR).display());
            } else {
                for v in &list {
                    println!("{}  {}  ops={} preserved={} bytes={}", v.id, v.host, v.ops, v.preserved, v.bytes);
                }
            }
            Ok(0)
        }
        Cmd::Restore { root, version, files, apply: do_apply } => {
            let (restored, skipped, errors) = syncdash::store::version::restore(&root, &version, &files, !do_apply)?;
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
            let (path, created) = syncdash::pipeline::guard::write_marker(&root, &job, &note)?;
            if created {
                println!("marked: {}", path.display());
            } else {
                let m = syncdash::pipeline::guard::read_marker(&root);
                println!(
                    "already marked: {}{}",
                    path.display(),
                    m.map(|m| format!("  (job '{}', by {} )", m.job, m.host)).unwrap_or_default()
                );
            }
            println!("set `require_marker = true` in the job to have syncdash refuse to run without it");
            Ok(0)
        }
        Cmd::Logs { cmd } => run_logs(cmd),
        Cmd::History { job, limit, prune_days } => {
            if let Some(days) = prune_days {
                // 0 = don't stack the total-size gate: the meaning of `--prune-days N` is exactly "by days only"
                let n = syncdash::obs::runlog::prune(days, 0);
                println!("pruned {n} run(s) older than {days} day(s)");
            }
            let rows = syncdash::obs::runlog::history(job.as_deref(), limit);
            if rows.is_empty() {
                println!("no runs recorded yet (runs are logged when a job actually applies)");
                return Ok(0);
            }
            let now = syncdash::foundation::time::now_ms() as i64;
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
                    syncdash::foundation::fmt::human_bytes(r.bytes),
                    r.elapsed_ms as f64 / 1000.0,
                    if r.cancelled { "  [cancelled]" } else { "" },
                );
            }
            Ok(0)
        }
        Cmd::Trash { cmd } => {
            use syncdash::foundation::fmt::human_bytes;
            match cmd {
                TrashCmd::Runs => {
                    let runs = syncdash::store::trash::list_runs();
                    if runs.is_empty() {
                        println!("no trash runs under {}", syncdash::store::trash::trash_root().display());
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
                    let hits = syncdash::store::trash::find(&pattern);
                    for h in &hits {
                        println!("{:<16} {:>10}  {}", h.run_id, human_bytes(h.size), h.rel);
                    }
                    println!("{} version(s)", hits.len());
                    Ok(0)
                }
                TrashCmd::Restore { pattern, into, run, apply: do_apply } => {
                    let (r, s, e) = syncdash::store::trash::restore(&pattern, run.as_deref(), &into, !do_apply)?;
                    println!(
                        "{}: {r} restored, {s} skipped, {e} error(s)",
                        if do_apply { "restore" } else { "dry-run (rerun with --apply)" }
                    );
                    Ok(if e > 0 { 1 } else { 0 })
                }
                TrashCmd::Prune { keep_days, max_gib, no_staggered, apply: do_apply } => {
                    let ret = syncdash::store::trash::Retention {
                        keep_days,
                        max_bytes: max_gib * 1024 * 1024 * 1024,
                        staggered: !no_staggered,
                    };
                    let (n, freed) = syncdash::store::trash::prune(&ret, !do_apply)?;
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
            let p = syncdash::model::plan::Plan::load(&plan)?;
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
