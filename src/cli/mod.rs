//! Dispatch: turn parsed arguments into library calls, and render what comes back.
//!
//! Rendering is the shell's job — the library returns data. That split is why the desktop shell
//! can call the same functions and draw its own tables instead of scraping stdout.

pub mod args;
pub mod logs;

use std::path::PathBuf;

use syncdash::job::{self, junk, territory};
use syncdash::model::table;
use syncdash::pipeline::{apply, compare, filter, scan};
use syncdash::run;
use syncdash::transfer::pack;

use args::{Cli, Cmd, CredCmd, Mode, TrashCmd};
use logs::run_logs;

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn FreeConsole() -> i32;
}

/// Drop the console window that tags along when launched by double-click.
///
/// Only reached when the exe is started with no arguments — a double-click, which then hands off
/// to the desktop app. A real command-line invocation keeps its console, because that is where
/// its output goes.
fn detach_console() {
    #[cfg(windows)]
    unsafe {
        FreeConsole();
    }
}


pub(crate) fn write_out<F: Fn(&mut dyn std::io::Write) -> std::io::Result<()>>(out: &Option<PathBuf>, f: F) -> std::io::Result<()> {
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

pub fn run_cli(cli: Cli) -> std::io::Result<i32> {
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
                    println!("{:<24} {:<7} {}  ->  {}", name, j.mode, j.source, j.target);
                }
            }
            Ok(0)
        }
        Cmd::Run { job, all, prefix, apply: do_apply, i_know, accept_caps, verbose, watch, interval, auto_apply } => {
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
                        let res = run::run_job(name, j, auto, verbose, i_know, accept_caps);
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
                let res = run::run_job(name, j, do_apply, verbose, i_know, accept_caps);
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
        Cmd::Junk { patterns } => {
            match patterns {
                Some(ids) => {
                    let ids: Vec<&str> = ids.iter().map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
                    if let Some(bad) = ids.iter().find(|id| junk::junk_preset(id).is_none()) {
                        eprintln!("error: unknown junk preset '{bad}' — run `syncdash junk` for the list");
                        return Ok(2);
                    }
                    for p in junk::expand_junk_presets(&ids) {
                        println!("{p}");
                    }
                }
                None => {
                    println!("Junk presets — each one is a macro over a job's `exclude` list, nothing more:\n");
                    for p in junk::JUNK_PRESETS {
                        println!("{}{}  ({})", p.id, if p.default_on { " [on for new jobs]" } else { "" }, p.label);
                        println!("  {}", p.hint);
                        println!("  {}\n", p.patterns.join("  "));
                    }
                    println!("Apply ad hoc:  syncdash scan <root> --junk windows,macos,dev");
                    println!("Paste into a job: syncdash junk --patterns dev");
                }
            }
            Ok(0)
        }
        Cmd::Scan { root, out, no_hash, rigor, evidence, cache, force_rehash, fast, symlinks_direct, junk, exclude, progress } => {
            if !root.is_dir() {
                eprintln!("error: not a directory: {}", root.display());
                return Ok(2);
            }
            // An unknown preset id is an error, never a silent no-op: a scan that quietly excluded less
            // than asked is exactly the kind of near-miss that only shows up as a surprise in a plan
            let ids: Vec<&str> = junk.iter().map(|s| s.trim()).filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("none")).collect();
            if let Some(bad) = ids.iter().find(|id| junk::junk_preset(id).is_none()) {
                let known: Vec<&str> = junk::JUNK_PRESETS.iter().map(|p| p.id).collect();
                eprintln!("error: unknown junk preset '{bad}' (known: {}, or `none`)", known.join(", "));
                return Ok(2);
            }
            let mut excludes = junk::expand_junk_presets(&ids);
            excludes.extend(exclude.iter().cloned());
            // One ladder, shared with `Job::rigor_resolved`: preset → detail overrides → the two
            // legacy flags this subcommand still accepts. `--fast` and `--force-rehash` predate
            // `--evidence`/`--cache` and are applied after them, so an explicit flag still wins.
            let mut r = job::rigor::RigorResolved::from_preset(&rigor)
                .with_evidence(evidence.as_deref())
                .with_cache(match cache.as_deref() {
                    Some("on") => Some(true),
                    Some("off") => Some(false),
                    _ => None,
                })
                .with_no_hash(no_hash);
            if fast {
                r.sampled = true;
                r.use_cache = true;
            }
            if force_rehash {
                r.use_cache = false;
            }
            let sopt = scan::ScanOptions {
                hash: r.hash,
                sampled: r.sampled,
                use_cache: r.use_cache,
                symlinks_direct,
                filter: filter::PathFilter::build(&[], &excludes),
            };
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
        Cmd::GenJobs { root, target_root, mode, rigor, remote_host, remote_root_base, remote_exe, junk, force } => {
            let remote = remote_host.map(|h| territory::RemoteGen {
                host: h,
                root_base: remote_root_base.unwrap_or_default(),
                exe: remote_exe,
            });
            // An unknown preset id is refused rather than dropped: a job seeded with fewer rules than
            // asked for is a filter that isn't what it says it is, and it would only surface as a surprise
            let ids: Vec<String> = junk
                .iter()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("none"))
                .collect();
            if let Some(bad) = ids.iter().find(|id| junk::junk_preset(id).is_none()) {
                let known: Vec<&str> = junk::JUNK_PRESETS.iter().map(|p| p.id).collect();
                eprintln!("error: unknown junk preset '{bad}' (known: {}, or `none`)", known.join(", "));
                return Ok(2);
            }
            let n_pat = junk::expand_junk_presets(&ids).len();
            let opts = territory::GenOpts { mode, rigor, junk: ids.clone(), force, ..Default::default() };
            let outs = territory::gen_jobs(&root, &target_root, &opts, remote.as_ref())?;
            for o in &outs {
                println!("{:<44} <- {}{}", o.name, o.territory, if o.written { "" } else { "   [kept — already exists]" });
            }
            let written = outs.iter().filter(|o| o.written).count();
            let kept = outs.len() - written;
            // State the seed rather than leaving it to be discovered: these lines are the job's entire filter
            println!(
                "{written} job(s) written to {} — each seeded with junk presets [{}] = {n_pat} exclude line(s), all listed in the file",
                syncdash::foundation::dirs::jobs_dir().display(),
                if ids.is_empty() { "none".into() } else { ids.join(", ") },
            );
            if kept > 0 {
                println!("{kept} existing job(s) left untouched (their exclude lists may have been edited) — pass --force to reseed them");
            }
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
            let (path, created) = syncdash::pipeline::guard::marker::write_marker(&root, &job, &note)?;
            if created {
                println!("marked: {}", path.display());
            } else {
                let m = syncdash::pipeline::guard::marker::read_marker(&root);
                println!(
                    "already marked: {}{}",
                    path.display(),
                    m.map(|m| format!("  (job '{}', by {} )", m.job, m.host)).unwrap_or_default()
                );
            }
            println!("set `require_marker = true` in the job to have syncdash refuse to run without it");
            Ok(0)
        }
        Cmd::Caps { phrase } => match run::describe_root(&phrase) {
            Ok(sheet) => {
                println!("{sheet}");
                Ok(0)
            }
            Err(e) => {
                eprintln!("connect failed: {e}");
                Ok(1)
            }
        },
        Cmd::Cred { cmd } => {
            use syncdash::fs::vfs::cred;
            use syncdash::fs::vfs::spec::{parse, RootSpec};
            match cmd {
                CredCmd::Set { phrase, stdin } => {
                    let RootSpec::Remote(r) = parse(&phrase) else {
                        eprintln!("not a remote phrase: {phrase} (expected scheme://user@host/...)");
                        return Ok(2);
                    };
                    let Some(acc) = cred::account_for(&r) else {
                        eprintln!("the phrase names no user — spell it scheme://user@host/... so the credential has an owner");
                        return Ok(2);
                    };
                    let pw = if stdin {
                        use std::io::Read as _;
                        let mut s = String::new();
                        std::io::stdin().read_to_string(&mut s)?;
                        s
                    } else {
                        rpassword::prompt_password(format!("password for {acc}: "))?
                    };
                    let pw = pw.trim_end_matches(['\r', '\n']);
                    if pw.is_empty() {
                        eprintln!("empty password — nothing stored");
                        return Ok(2);
                    }
                    cred::set_secret(&acc, pw).map_err(std::io::Error::from)?;
                    println!("stored in the OS credential store: {acc}");
                    Ok(0)
                }
                CredCmd::Rm { phrase } => {
                    let RootSpec::Remote(r) = parse(&phrase) else {
                        eprintln!("not a remote phrase: {phrase}");
                        return Ok(2);
                    };
                    let Some(acc) = cred::account_for(&r) else {
                        eprintln!("the phrase names no user");
                        return Ok(2);
                    };
                    if cred::delete_secret(&acc).map_err(std::io::Error::from)? {
                        println!("removed: {acc}");
                    } else {
                        println!("no entry stored for {acc}");
                    }
                    Ok(0)
                }
                CredCmd::Ls => {
                    let accounts = cred::list_accounts();
                    if accounts.is_empty() {
                        println!("no stored credentials (add one with: syncdash cred set \"smb://user@host/share\")");
                    }
                    for a in accounts {
                        println!("{a}");
                    }
                    Ok(0)
                }
                CredCmd::Test { phrase } => {
                    let v = syncdash::fs::vfs::open(&phrase, &cred::default_provider()).map_err(std::io::Error::from)?;
                    match v.connect() {
                        Ok(()) => {
                            println!("connected: {}", v.display());
                            if let Some(info) = v.server_info() {
                                println!("  {info}");
                            }
                            let c = v.caps();
                            println!(
                                "  protocol {}, mtime precision {} ms, up to {} parallel stream(s)",
                                c.protocol, c.mtime_precision_ms, c.max_parallel_streams
                            );
                            Ok(0)
                        }
                        Err(e) => {
                            eprintln!("connect failed: {e}");
                            Ok(1)
                        }
                    }
                }
            }
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
