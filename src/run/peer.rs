//! Jobs that execute on a peer over ssh.
//!
//! The far side runs syncdash against its own disk: it scans there and sends back a table, and it
//! applies a package we build here. That is why the capability consent and the disk-space gate do
//! not apply to this lane — they are questions about roots this process opened, and it opened none.

use crate::job::{Job, SingleTargetJob};
use crate::model::plan::{Action, Op, Plan};
use crate::pipeline::{compare, scan};

use super::archive::refresh_archive_with;
use super::{scan_opts, CompareOutcome};
use crate::foundation::path::RootRelativePath;
use crate::fs::local_root::LocalRoot;
use crate::model::table::TableArtifact;

struct TemporaryPeerPackage {
    root: LocalRoot,
    relative: RootRelativePath,
    file: std::fs::File,
}

impl TemporaryPeerPackage {
    fn create() -> std::io::Result<Self> {
        let root = LocalRoot::open(std::env::temp_dir())?;
        for _ in 0..16 {
            let mut random = [0u8; 16];
            getrandom::fill(&mut random).map_err(|error| {
                std::io::Error::other(format!("random token generation failed: {error}"))
            })?;
            let mut token = String::with_capacity(random.len() * 2);
            for byte in random {
                use std::fmt::Write as _;
                write!(token, "{byte:02x}").expect("writing into a String cannot fail");
            }
            let relative = RootRelativePath::try_from(format!("syncdash-peer-{token}.tar"))
                .expect("generated peer package names satisfy the relative-path contract");
            match root.create_regular_file_new(&relative) {
                Ok(file) => {
                    return Ok(Self {
                        root,
                        relative,
                        file,
                    })
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "could not allocate an exclusive peer package file",
        ))
    }

    fn output(&self) -> std::io::Result<std::fs::File> {
        self.file.try_clone()
    }

    fn reader(&self) -> std::io::Result<std::fs::File> {
        use std::io::{Seek, SeekFrom};

        let mut reader = self.file.try_clone()?;
        reader.seek(SeekFrom::Start(0))?;
        Ok(reader)
    }

    fn len(&self) -> std::io::Result<u64> {
        Ok(self.file.metadata()?.len())
    }

    fn file_name(&self) -> &str {
        self.relative.as_str()
    }
}

impl Drop for TemporaryPeerPackage {
    fn drop(&mut self) {
        let _ = self.root.remove_open_file(&self.relative, &self.file);
    }
}

pub fn run_peer_job(
    name: &str,
    job: &SingleTargetJob,
    do_apply: bool,
    verbose: bool,
    acknowledged: bool,
) -> std::io::Result<(u64, u64, u64, u64)> {
    run_peer_job_with(
        name,
        job,
        do_apply,
        verbose,
        acknowledged,
        &crate::obs::progress::RunCtx::null(),
    )
}

/// Keep the CLI's combined Compare/Apply flow on the same progress contract as the desktop's
/// separate commands, including a terminal Summary when Compare is cancelled.
pub fn run_peer_job_with(
    name: &str,
    job: &SingleTargetJob,
    do_apply: bool,
    verbose: bool,
    acknowledged: bool,
    ctx: &crate::obs::progress::RunCtx,
) -> std::io::Result<(u64, u64, u64, u64)> {
    let plan = match super::compare_peer_job_with(name, job, ctx) {
        Ok(p) => p,
        Err(e) => {
            // Cancelled in the compare stage: the terminal state must still be visible (the desktop closes out on Summary)
            if crate::obs::progress::is_cancelled(&e) {
                emit_cancel_summary(ctx, std::time::Instant::now());
            }
            return Err(e);
        }
    };
    ctx.log(
        crate::model::event::LogLevel::Info,
        "run",
        format!(
            "[{name}] {} op(s), {} conflict(s)  (peer pipeline via ssh)",
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
    let rec = crate::obs::runlog::Recorder::start(
        crate::obs::runlog::RunSubject::for_job(name, job),
        super::apply_run_kind(job),
        ctx,
        &ops,
    );
    let out = apply_peer_job_with(name, job, &plan, &ops, verbose, acknowledged, &rec.ctx)?;
    let _ = rec.finish(&out, t0.elapsed().as_millis() as u64);
    Ok((
        out.done,
        out.skipped,
        out.errors,
        plan.header.conflict_count,
    ))
}

fn emit_cancel_summary(ctx: &crate::obs::progress::RunCtx, t0: std::time::Instant) {
    emit_apply_summary(
        ctx,
        t0,
        crate::obs::progress::ApplyOutcome {
            cancelled: true,
            ..Default::default()
        },
    );
}

fn emit_apply_summary(
    ctx: &crate::obs::progress::RunCtx,
    t0: std::time::Instant,
    out: crate::obs::progress::ApplyOutcome,
) {
    ctx.sink.emit(crate::model::event::ProgressEvent::Summary {
        ts_ms: crate::foundation::time::now_ms(),
        done: out.done,
        skipped: out.skipped,
        errors: out.errors,
        bytes_done: out.bytes_copied,
        elapsed_ms: t0.elapsed().as_millis() as u64,
        paused_ms: ctx.ctl.paused_total_ms(),
        cancelled: out.cancelled,
    });
}

/// Peer connection parameters (the product of a probe). The desktop's compare and apply are two independent IPC rounds
/// with no connection kept in between — the apply stage probes again (one ssh round trip, which doubles as a reachability preflight).
pub struct PeerLink {
    pub host: String,
    pub executable: String,
    pub peer_root: String,
    /// A local path serving the *same* tree the peer syncs — the `|mount=` option.
    ///
    /// The peer lane pushes: it packs the target-side ops and the far side applies them. The
    /// reverse (source-side) direction has nothing to push, so it writes through this mount
    /// instead. It is an option on the phrase rather than an assumption because a peer job used
    /// to depend on it silently: the mount lived in `target` alongside an unrelated
    /// `remote_root`, nothing said the two named one tree, and a missing mount skipped those ops
    /// with a warning nobody had a reason to expect.
    pub mount: Option<std::path::PathBuf>,
    pub shell: crate::transfer::peer::PeerShell,
    /// The live ssh session, held for the whole stage. The old transport handshook once per
    /// command; a compare stage runs several.
    pub session: crate::transfer::peer::PeerSession,
}

impl PeerLink {
    /// Build one syncdash command line for this peer's shell dialect.
    fn command(&self, arguments: &[String]) -> String {
        crate::transfer::peer::peer_command(self.shell, &self.executable, arguments)
    }
}

struct PeerLinkSettings {
    host: String,
    executable: String,
    peer_root: String,
    mount: Option<std::path::PathBuf>,
}

/// Restore a peer root to the absolute path the far side will resolve.
///
/// The phrase grammar strips the leading `/` — right for `sftp://` and `smb://`, where the root is
/// a segment inside a session or a share, and wrong here: a peer root is a path on the far
/// machine's own filesystem and the far syncdash resolves it against *its* working directory. Sent
/// as `Users/ben/x` it lands at `~/Users/ben/x`, which is a path that generally does not exist —
/// so the run reads an empty tree and mirror proposes deleting everything in the source.
///
/// A drive letter is already absolute (a Windows peer takes `C:\…` verbatim); everything else lost
/// a `/` on the way in and gets it back.
fn absolute_peer_root(root: &str) -> String {
    let b = root.as_bytes();
    if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
        root.to_string()
    } else {
        format!("/{root}")
    }
}

fn parse_peer_link_settings(job: &SingleTargetJob) -> std::io::Result<PeerLinkSettings> {
    use crate::fs::vfs::spec::{parse, RootSpec};
    let invalid_input =
        |message: String| std::io::Error::new(std::io::ErrorKind::InvalidInput, message);
    let RootSpec::Endpoint(peer_spec) = parse(job.target()) else {
        return Err(invalid_input(format!(
            "target '{}' is not a peer:// root",
            job.target()
        )));
    };
    if peer_spec.root.is_empty() {
        return Err(invalid_input(format!(
            "target '{}' names no path on {} — a peer root needs one (peer://{}/path/to/tree)",
            job.target(),
            peer_spec.host,
            peer_spec.host
        )));
    }
    Ok(PeerLinkSettings {
        host: peer_spec.host.clone(),
        executable: peer_spec
            .opt("exe")
            .filter(|e| !e.is_empty())
            .unwrap_or("syncdash")
            .to_string(),
        peer_root: absolute_peer_root(&peer_spec.root),
        mount: peer_spec
            .opt("mount")
            .filter(|m| !m.is_empty())
            .map(std::path::PathBuf::from),
    })
}

#[cfg(test)]
mod link_tests {
    use super::absolute_peer_root;

    /// Caught on real hardware: the far side was sent `Users/xuanbomiao/x` and resolved it against
    /// the login home. A peer root has to arrive as the absolute path it was written as.
    #[test]
    fn a_posix_peer_root_gets_its_leading_slash_back() {
        assert_eq!(absolute_peer_root("Users/ben/Code"), "/Users/ben/Code");
        assert_eq!(absolute_peer_root("srv/data"), "/srv/data");
    }

    #[test]
    fn a_windows_peer_root_is_already_absolute() {
        assert_eq!(absolute_peer_root("C:/Users/ben"), "C:/Users/ben");
        assert_eq!(absolute_peer_root(r"D:\Code"), r"D:\Code");
    }
}

#[cfg(test)]
mod temporary_package_tests {
    use super::TemporaryPeerPackage;
    use std::io::{Read, Write};

    #[test]
    fn peer_package_is_exclusive_reopen_free_and_removed_on_drop() {
        let package = TemporaryPeerPackage::create().unwrap();
        let path = package.root.display_path().join(package.file_name());
        let mut output = package.output().unwrap();
        output.write_all(b"package").unwrap();
        output.sync_all().unwrap();

        let mut reader = package.reader().unwrap();
        let mut contents = Vec::new();
        reader.read_to_end(&mut contents).unwrap();
        assert_eq!(contents, b"package");
        assert_eq!(package.len().unwrap(), b"package".len() as u64);
        assert!(path.is_file());

        drop(reader);
        drop(output);
        drop(package);
        assert!(!path.exists());
    }

    #[test]
    fn concurrent_peer_packages_never_share_a_name() {
        let first = TemporaryPeerPackage::create().unwrap();
        let second = TemporaryPeerPackage::create().unwrap();

        assert_ne!(first.file_name(), second.file_name());
    }
}

#[cfg(test)]
mod filter_tests {
    use super::build_peer_scan_arguments;
    use crate::job::Job;

    fn job_with(include: &[&str], exclude: &[&str]) -> Job {
        Job {
            mode: "mirror".into(),
            source: r"D:\src".into(),
            targets: vec!["peer://mac/Users/ben/dst".into()],
            include: include.iter().map(|s| s.to_string()).collect(),
            exclude: exclude.iter().map(|s| s.to_string()).collect(),
            ..Job::default()
        }
    }

    fn pairs<'a>(args: &'a [String], flag: &str) -> Vec<&'a str> {
        args.windows(2)
            .filter(|w| w[0] == flag)
            .map(|w| w[1].as_str())
            .collect()
    }

    #[test]
    fn every_exclude_crosses_the_link() {
        let arguments = build_peer_scan_arguments(
            &job_with(&[], &["*/big_temp/", "*/*.log"]),
            "/Users/ben/dst",
        );
        assert_eq!(
            pairs(&arguments, "--exclude"),
            vec!["*/big_temp/", "*/*.log"]
        );
        assert_eq!(pairs(&arguments, "--junk"), vec!["none"]);
    }

    /// The whole filter has to cross, not half of it.
    ///
    /// `include` is an allowlist: with one set, everything outside it is *not part of this job*.
    /// The local side applies it. If the far side never hears about it, the far side reports files
    /// the local filter hid — and because they are then "on the target and not on the source",
    /// `mirror` proposes a `Delete` for every one of them. That is the exact failure the sibling
    /// `--junk none` line three lines up exists to prevent, and it is data loss, not a cosmetic
    /// asymmetry.
    #[test]
    fn every_include_crosses_the_link_too() {
        let arguments =
            build_peer_scan_arguments(&job_with(&["*/keep/", "/docs/"], &[]), "/Users/ben/dst");
        assert_eq!(
            pairs(&arguments, "--include"),
            vec!["*/keep/", "/docs/"],
            "an allowlist that binds only the local root turns every unlisted peer file into a deletion"
        );
    }
}

/// It takes `ctx` for the sake of the schema-mismatch warning: that one has to reach the UI **during compare**.
/// Going through the macro and the global registry, no sink is installed during compare (only apply starts a Recorder),
/// so this line in particular would fall back to stderr — which in a windowed desktop build is the same as saying nothing.
pub fn probe_peer(
    name: &str,
    job: &SingleTargetJob,
    ctx: &crate::obs::progress::RunCtx,
) -> std::io::Result<PeerLink> {
    let settings = parse_peer_link_settings(job)?;
    let host = settings.host.as_str();
    let executable = settings.executable.as_str();
    let session = crate::transfer::peer::PeerSession::open(job.target())?;
    let probe = session.capture(&format!("{executable} probe"))?;
    let pv: serde_json::Value = serde_json::from_slice(&probe).map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("bad probe output: {e}"),
        )
    })?;
    if pv["schema"].as_u64() != Some(crate::model::table::TABLE_SCHEMA as u64) {
        ctx.log(
            crate::model::event::LogLevel::Warn,
            "peer",
            format!(
                "[{name}] warning: peer schema {} != local {} — rebuild the peer binary",
                pv["schema"],
                crate::model::table::TABLE_SCHEMA
            ),
        );
    }
    let peer_os = pv["os"].as_str().unwrap_or("").to_string();
    ctx.log(
        crate::model::event::LogLevel::Info,
        "peer",
        format!(
            "[{name}] peer {}: {} {}",
            host,
            peer_os,
            pv["arch"].as_str().unwrap_or("?")
        ),
    );
    Ok(PeerLink {
        host: settings.host,
        executable: settings.executable,
        peer_root: settings.peer_root,
        mount: settings.mount,
        shell: crate::transfer::peer::PeerShell::from_os(&peer_os),
        session,
    })
}

/// The `scan` the far side is told to run, as a value — so what crosses the link can be asserted
/// without one.
///
/// **Both roots must be filtered by the same rule**, and that governs everything below: the whole
/// mask crosses (`--include` as well as `--exclude`), and `--junk none` stops the peer adding its
/// own CLI default on top of rules the job already spells out in full. A rule binding one root only
/// is the shape that gets a tree proposed for deletion — with `include` dropped, the far side
/// reports files the local filter hid, and `mirror` reads those as "on the target, not on the
/// source".
///
/// The rigor knobs go over **resolved**, because a preset name is not enough: details may have
/// overridden it.
fn build_peer_scan_arguments(job: &Job, peer_root: &str) -> Vec<String> {
    // The job's own tier, not a narrowed one: this process holds no handle on the far root, so there
    // is no second backend to negotiate down to.
    let opt = scan_opts(job);
    let mut arguments: Vec<String> = vec![
        "scan".into(),
        peer_root.to_string(),
        "--evidence".into(),
        super::evidence_label(&opt).into(),
        "--cache".into(),
        (if opt.use_cache { "on" } else { "off" }).into(),
        "--junk".into(),
        "none".into(),
    ];
    for include in &job.include {
        arguments.push("--include".into());
        arguments.push(include.clone());
    }
    for exclude in &job.exclude {
        arguments.push("--exclude".into());
        arguments.push(exclude.clone());
    }
    if job.symlinks == "direct" {
        arguments.push("--symlinks-direct".into());
    }
    arguments
}

/// The same detailed variant for the peer pipeline: the peer snapshot is a complete table pulled back over ssh,
/// so the evidence layer (both sides' size/mtime, identical items) is just as computable here as for a local job.
pub fn compare_peer_job_detailed(
    name: &str,
    job: &SingleTargetJob,
    ctx: &crate::obs::progress::RunCtx,
) -> std::io::Result<CompareOutcome> {
    use crate::model::event::Phase;
    use crate::obs::progress::PhaseProgress;
    let configuration = job.configuration();
    let link = probe_peer(name, job, ctx)?;

    let scan_arguments = build_peer_scan_arguments(configuration, &link.peer_root);
    ctx.checkpoint()?;
    // The peer scans on its own disk, so this process has no totals until the table returns.
    let pp_rs = PhaseProgress::begin(
        ctx,
        Phase::ScanTarget,
        Some(format!("ssh:{} {}", link.host, link.peer_root)),
        0,
        0,
    );
    let table_bytes = link.session.capture(&link.command(&scan_arguments))?;
    let t = TableArtifact::read_snapshot(std::io::BufReader::new(&table_bytes[..]))?;
    pp_rs.finish()?;

    let mut v = crate::pipeline::guard::Verdict {
        blockers: Vec::new(),
        warnings: Vec::new(),
    };
    crate::pipeline::guard::roots::check_root(
        "source",
        configuration.source_path(),
        configuration.require_marker,
        &mut v,
    );
    for w in &v.warnings {
        ctx.log(
            crate::model::event::LogLevel::Warn,
            "compare",
            format!("[{name}] warning: {w}"),
        );
    }
    if !v.ok() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            v.blockers.join("; "),
        ));
    }
    let options = scan_opts(configuration);
    let s = scan::scan_ctx(
        configuration.source_path(),
        &options,
        ctx,
        Phase::ScanSource,
    )?;
    let archive = super::load_comparison_archive(configuration, &options, ctx)?;
    let pp_cmp = PhaseProgress::begin(
        ctx,
        Phase::Compare,
        Some(format!(
            "{} × {} entries",
            s.header.entry_count, t.header.entry_count
        )),
        0,
        0,
    );
    let copts = configuration.compare_opts();
    let plan = compare::compare(&s, &t, &configuration.mode, archive.as_ref(), false, &copts);
    pp_cmp.finish()?;
    Ok(CompareOutcome {
        plan,
        source: s,
        target: t,
        compare_options: copts,
    })
}

/// The peer plan health check can prove deletion share locally; disk space and the marker live on
/// the peer and remain explicit capability limitations.
pub fn preflight_peer_job(
    job: &SingleTargetJob,
    plan: &Plan,
    ops: &[Op],
    acknowledged: bool,
) -> crate::pipeline::guard::Verdict {
    let g = job.configuration().guards(acknowledged);
    let st = crate::pipeline::guard::stats::stat_plan(ops);
    let mut gv = crate::pipeline::guard::Verdict {
        blockers: Vec::new(),
        warnings: Vec::new(),
    };
    crate::pipeline::guard::ratio::check_delete_ratio(
        "target",
        &st.target,
        plan.header.target_entries,
        &g,
        &mut gv,
    );
    crate::pipeline::guard::ratio::check_delete_ratio(
        "source",
        &st.source,
        plan.header.source_entries,
        &g,
        &mut gv,
    );
    let needs_pull_mount = ops.iter().any(|op| {
        op.side == crate::model::plan::Side::Source
            && !matches!(
                op.action,
                crate::model::plan::Action::Conflict | crate::model::plan::Action::Note
            )
    });
    if needs_pull_mount {
        match crate::fs::vfs::spec::parse(job.target()) {
            crate::fs::vfs::spec::RootSpec::Endpoint(peer_spec) => {
                match peer_spec.opt("mount").filter(|mount| !mount.is_empty()) {
                    Some(mount) if std::path::Path::new(mount).is_dir() => {}
                    Some(mount) => gv.blockers.push(format!(
                        "source-side actions require the peer mount '{mount}', but it is not an accessible directory"
                    )),
                    None => gv.blockers.push(
                        "source-side actions require |mount=<local path serving the peer tree>; this peer job is push-only without it"
                            .into(),
                    ),
                }
            }
            _ => gv
                .blockers
                .push("the peer target phrase is no longer valid — run Compare again".into()),
        }
    }
    gv
}

/// Limitations the controlling process can prove before shipping a package to a peer.
/// Anything unobservable is represented as structured review data instead of being fabricated.
pub fn apply_capabilities(
    job: &SingleTargetJob,
    ops: &[Op],
) -> crate::pipeline::guard::caps::CapReport {
    use crate::model::plan::{Action, Side};
    use crate::pipeline::guard::caps::{CapItem, CapReport, CapSeverity};

    let writes_target = ops
        .iter()
        .any(|op| op.side == Side::Target && !matches!(op.action, Action::Conflict | Action::Note));
    if !writes_target {
        return CapReport::default();
    }
    let mut report = CapReport::default();
    let configuration = job.configuration();
    if configuration.require_marker {
        report.items.push(CapItem {
            feature: "require_marker".into(),
            side: "target".into(),
            severity: CapSeverity::Block,
            requested: "a .syncdash-root marker verified before writing".into(),
            actual: "the current peer package protocol cannot inspect the peer marker".into(),
            effect:
                "the required mount-point gate cannot be proven, so target-side writes are refused"
                    .into(),
        });
    }
    if configuration.min_free_pct > 0.0 {
        report.items.push(CapItem {
            feature: "min_free_pct".into(),
            side: "target".into(),
            severity: CapSeverity::NeedsAck,
            requested: format!(
                "at least {:.2}% free space retained before writing",
                configuration.min_free_pct * 100.0
            ),
            actual: "peer free space is not observable through the current package protocol"
                .into(),
            effect: "the peer target can run out of space; staged writes still fail per file without publishing partial content"
                .into(),
        });
    }
    report
}

#[cfg(test)]
mod apply_capability_tests {
    use super::*;
    use crate::model::plan::{Action, Op, Side};
    use crate::pipeline::guard::caps::CapSeverity;

    fn target_write() -> Op {
        Op {
            side: Side::Target,
            action: Action::Delete,
            path: "old.txt".into(),
            from: None,
            size: None,
            mtime_ms: None,
            hash: None,
            link: None,
            mode: None,
            reason: "test".into(),
        }
    }

    #[test]
    fn peer_limitations_are_structured_only_when_the_peer_side_writes() {
        let mut job = Job::default();
        job.source = "/source".into();
        job.targets = vec!["peer://host/srv/data".into()];
        let selected = job.select_target(0).unwrap();
        let report = apply_capabilities(&selected, &[target_write()]);
        assert!(report.items.iter().any(|item| {
            item.feature == "min_free_pct" && item.severity == CapSeverity::NeedsAck
        }));

        let mut source = target_write();
        source.side = Side::Source;
        assert!(apply_capabilities(&selected, &[source]).items.is_empty());
    }

    #[test]
    fn an_unobservable_required_peer_marker_is_a_hard_blocker() {
        let mut job = Job::default();
        job.source = "/source".into();
        job.targets = vec!["peer://host/srv/data".into()];
        job.require_marker = true;
        let selected = job.select_target(0).unwrap();
        let report = apply_capabilities(&selected, &[target_write()]);
        assert!(report.items.iter().any(|item| {
            item.feature == "require_marker" && item.severity == CapSeverity::Block
        }));
    }
}

/// The peer apply stage receives the reviewed subset after direction changes and inclusion decisions.
pub fn apply_peer_job_with(
    name: &str,
    job: &SingleTargetJob,
    plan: &Plan,
    ops: &[Op],
    verbose: bool,
    acknowledged: bool,
    ctx: &crate::obs::progress::RunCtx,
) -> std::io::Result<crate::obs::progress::ApplyOutcome> {
    apply_peer_job_with_classified(name, job, plan, ops, verbose, acknowledged, ctx).into_result()
}

pub fn apply_peer_job_with_classified(
    name: &str,
    job: &SingleTargetJob,
    plan: &Plan,
    ops: &[Op],
    verbose: bool,
    acknowledged: bool,
    ctx: &crate::obs::progress::RunCtx,
) -> super::ApplyExecution {
    let t0 = std::time::Instant::now();
    let mut writes_started = false;
    let mut r = apply_peer_inner(
        name,
        job,
        plan,
        ops,
        verbose,
        acknowledged,
        ctx,
        &mut writes_started,
    );
    if let Ok(out) = &mut r {
        out.cancelled |= ctx.ctl.cancelled();
    }
    let terminal = match &r {
        Ok(out) => *out,
        Err(e) => crate::obs::progress::ApplyOutcome {
            errors: u64::from(!crate::obs::progress::is_cancelled(e)),
            cancelled: crate::obs::progress::is_cancelled(e) || ctx.ctl.cancelled(),
            ..Default::default()
        },
    };
    emit_apply_summary(ctx, t0, terminal);
    classify_peer_completion(writes_started, r)
}

fn classify_peer_completion(
    writes_started: bool,
    result: std::io::Result<crate::obs::progress::ApplyOutcome>,
) -> super::ApplyExecution {
    match (writes_started, result) {
        (false, Ok(outcome)) => super::ApplyExecution::rejected(outcome),
        (true, Ok(outcome)) => super::ApplyExecution::started(outcome),
        (false, Err(error)) => super::ApplyExecution::failed_before_write(error),
        (true, Err(error)) => super::ApplyExecution::failed_after_write(error),
    }
}

#[cfg(test)]
mod apply_boundary_tests {
    use super::classify_peer_completion;
    use crate::obs::progress::ApplyOutcome;

    #[test]
    fn peer_boundary_is_explicit_not_inferred_from_outcome_or_error_text() {
        let indistinguishable = ApplyOutcome {
            done: 0,
            skipped: 1,
            errors: 1,
            bytes_copied: 0,
            cancelled: false,
        };
        let before = classify_peer_completion(false, Ok(indistinguishable));
        let after = classify_peer_completion(true, Ok(indistinguishable));
        assert!(!before.writes_started());
        assert!(after.writes_started());

        let before = classify_peer_completion(
            false,
            Err(std::io::Error::other("identical transport failure")),
        );
        let after = classify_peer_completion(
            true,
            Err(std::io::Error::other("identical transport failure")),
        );
        assert!(!before.writes_started());
        assert!(after.writes_started());
        assert_eq!(
            before.into_result().unwrap_err().to_string(),
            after.into_result().unwrap_err().to_string()
        );
    }
}

#[allow(clippy::too_many_arguments)] // reviewed inputs and the write-boundary witness remain explicit
fn apply_peer_inner(
    name: &str,
    job: &SingleTargetJob,
    plan_full: &Plan,
    sel_ops: &[Op],
    verbose: bool,
    acknowledged: bool,
    ctx: &crate::obs::progress::RunCtx,
    writes_started: &mut bool,
) -> std::io::Result<crate::obs::progress::ApplyOutcome> {
    use crate::model::event::{Phase, ProgressEvent};
    use crate::model::plan::Side;
    use crate::obs::progress::{ApplyOutcome, PhaseProgress};

    let configuration = job.configuration();
    let gv = preflight_peer_job(job, plan_full, sel_ops, acknowledged);
    if !gv.report(name) {
        for b in &gv.blockers {
            ctx.sink.emit(ProgressEvent::Error {
                phase: Phase::Apply,
                ts_ms: crate::foundation::time::now_ms(),
                path: String::new(),
                action: "preflight".into(),
                side: "target".into(),
                message: b.clone(),
            });
        }
        return Ok(ApplyOutcome {
            done: 0,
            skipped: sel_ops.len() as u64,
            errors: 1,
            bytes_copied: 0,
            cancelled: false,
        });
    }

    let link = probe_peer(name, job, ctx)?;
    let (host, peer_root, shell) = (link.host.as_str(), link.peer_root.as_str(), link.shell);
    // Packing and the pull-back only look at the finalised subset; the full plan is used only for the archive refresh (dropping conflicted paths needs all of it)
    let plan = Plan {
        header: plan_full.header.clone(),
        ops: sel_ops.to_vec(),
    };

    let mut done = 0u64;
    let mut skipped = 0u64;
    let mut errors = 0u64;
    let mut bytes_done_total = 0u64;

    let has_target_ops = plan
        .ops
        .iter()
        .any(|o| o.side == Side::Target && !matches!(o.action, Action::Conflict | Action::Note));
    if has_target_ops {
        let delta_rels: Vec<String> = plan
            .ops
            .iter()
            .filter(|o| {
                o.side == Side::Target
                    && o.action == Action::Update
                    && o.link.is_none()
                    && o.size
                        .map(|s| s >= crate::model::chunk::DELTA_MIN_SIZE)
                        .unwrap_or(false)
            })
            .map(|o| o.path.clone())
            .collect();
        let peer_chunks = if delta_rels.is_empty() {
            None
        } else {
            let mut args: Vec<String> =
                vec!["chunks".into(), "--root".into(), peer_root.to_string()];
            for r in &delta_rels {
                args.push("--file".into());
                args.push(r.clone());
            }
            match link.session.capture(&link.command(&args)) {
                Ok(bytes) => {
                    let mut m = std::collections::HashMap::new();
                    for line in String::from_utf8_lossy(&bytes).lines() {
                        let line = line.trim();
                        if line.is_empty() {
                            continue;
                        }
                        if let Ok(fc) =
                            serde_json::from_str::<crate::model::chunk::FileChunks>(line)
                        {
                            m.insert(fc.rel.as_str().to_owned(), fc);
                        }
                    }
                    ctx.log(
                        crate::model::event::LogLevel::Info,
                        "delta",
                        format!(
                            "[{name}] delta: got chunk tables for {} large file(s)",
                            m.len()
                        ),
                    );
                    Some(m)
                }
                Err(e) => {
                    ctx.log(
                        crate::model::event::LogLevel::Warn,
                        "delta",
                        format!("[{name}] delta disabled (chunk request failed: {e})"),
                    );
                    None
                }
            }
        };
        ctx.checkpoint()?;
        let pp_pack = PhaseProgress::begin(
            ctx,
            Phase::Pack,
            Some("packing target-side content".into()),
            0,
            0,
        );
        let package = TemporaryPeerPackage::create()?;
        let sum = crate::transfer::pack::pack_to_open_file(
            &plan,
            configuration.source_path(),
            package.output()?,
            peer_chunks.as_ref(),
        )?;
        pp_pack.set_totals(sum.ops, sum.bytes);
        if sum.delta_saved > 0 {
            ctx.log(
                crate::model::event::LogLevel::Info,
                "pack",
                format!(
                    "[{name}] packed {} B, delta saved {} B",
                    sum.bytes, sum.delta_saved
                ),
            );
        }
        pp_pack.complete("package");
        pp_pack.finish()?;
        let peer_package_path = if shell == crate::transfer::peer::PeerShell::PowerShell {
            // PowerShell resolves this relative path in the peer account's home directory.
            package.file_name().to_owned()
        } else {
            format!("/tmp/{}", package.file_name())
        };
        ctx.checkpoint()?;
        let tar_len = package.len()?;
        let pp_ship =
            PhaseProgress::begin(ctx, Phase::Ship, Some(format!("→ ssh:{host}")), 1, tar_len);
        let recv_cmd = link.command(&["recv".into(), peer_package_path.clone()]);
        // Bytes are counted as they leave, not assumed once the transfer returns: the old
        // transport handed the file to a child process and learned nothing until it exited, so a
        // multi-gigabyte package sat at 0% for its whole duration.
        link.session
            .send_file(&recv_cmd, package.reader()?, &mut |n| {
                pp_ship.add_bytes(n, &peer_package_path);
                // Cancel now stops the upload instead of letting it run to completion unwatched
                ctx.checkpoint()
            })?;
        pp_ship.item_done(&peer_package_path);
        pp_ship.finish()?;
        bytes_done_total += sum.bytes;

        ctx.checkpoint()?;
        let pp_ra = PhaseProgress::begin(
            ctx,
            Phase::Apply,
            Some(format!("ssh:{host} apply-pack")),
            sum.ops,
            0,
        );
        let mut ap_args: Vec<String> = vec![
            "apply-pack".into(),
            peer_package_path.clone(),
            "--apply".into(),
            "--remove-pkg".into(),
        ];
        if configuration.versioning {
            ap_args.push("--versioning".into());
        }
        if verbose {
            ap_args.push("-v".into());
        }
        // Once this command is handed to ssh, a transport error cannot prove whether the far side
        // started publishing files. Mark the boundary before invocation, not after its response.
        *writes_started = true;
        let ok = link.session.run_status(&link.command(&ap_args))?;
        if ok {
            done += sum.ops;
            pp_ra.complete(&peer_package_path);
            if pp_ra.finish().is_err() {
                return Ok(ApplyOutcome {
                    done,
                    skipped,
                    errors,
                    bytes_copied: bytes_done_total,
                    cancelled: true,
                });
            }
        } else {
            errors += 1;
            ctx.log(
                crate::model::event::LogLevel::Error,
                "peer",
                format!("[{name}] peer apply-pack reported failure"),
            );
            ctx.sink.emit(ProgressEvent::Error {
                phase: Phase::Apply,
                ts_ms: crate::foundation::time::now_ms(),
                path: peer_package_path.clone(),
                action: "apply-pack".into(),
                side: "target".into(),
                message: "peer apply-pack reported failure".into(),
            });
            pp_ra.fail();
        }
    }

    // The peer lane only pushes — it packs ops for the far
    // side to apply — so a pull has to read the peer tree through a mount of it, named by
    // `|mount=` on the phrase. No mount means the job never had a pull path; say so plainly
    // rather than reporting ops as skipped for a reason the config does not mention.
    let src_ops: Vec<Op> = plan
        .ops
        .iter()
        .filter(|o| o.side == Side::Source && !matches!(o.action, Action::Conflict | Action::Note))
        .cloned()
        .collect();
    if !src_ops.is_empty() {
        match link.mount.as_deref() {
            Some(m) if m.is_dir() => {
                *writes_started = true;
                let out = crate::pipeline::apply::apply_with(
                    &src_ops,
                    configuration.source_path(),
                    m,
                    &configuration.apply_opts(None, verbose),
                    ctx,
                );
                done += out.done;
                skipped += out.skipped;
                errors += out.errors;
                bytes_done_total += out.bytes_copied;
            }
            Some(m) => {
                skipped += src_ops.len() as u64;
                ctx.log(
                    crate::model::event::LogLevel::Warn,
                    "peer",
                    format!(
                        "[{name}] {} pull op(s) skipped: the declared mount '{}' is not reachable — check the share, or drop |mount= if this job only pushes",
                        src_ops.len(),
                        m.display()
                    ),
                );
            }
            None => {
                skipped += src_ops.len() as u64;
                ctx.log(
                    crate::model::event::LogLevel::Warn,
                    "peer",
                    format!(
                        "[{name}] {} pull op(s) skipped: '{}' declares no |mount=, and the peer lane cannot pull without one (add |mount=<path serving the same tree>)",
                        src_ops.len(),
                        job.target()
                    ),
                );
            }
        }
    }

    if errors == 0 && !ctx.ctl.cancelled() && configuration.mode == "sync" {
        // A peer job's source is a root this process owns, so opening it is local work, not a
        // second handshake. A failure here used to return silently; an archive that did not get
        // refreshed changes what the next run concludes, so it says so.
        match super::roots::resolve_root(&configuration.source) {
            // The peer lane has no local handle on the far root, so no joint-tier narrowing applies
            // and never did: its comparison runs at the job's own tier, and the archive matches it.
            Ok(sv) => {
                *writes_started = true;
                if !refresh_archive_with(
                    configuration,
                    plan_full,
                    &sv,
                    &scan_opts(configuration),
                    ctx,
                ) && !ctx.ctl.cancelled()
                {
                    errors += 1;
                }
            }
            Err(e) => {
                errors += 1;
                ctx.sink.emit(ProgressEvent::Error {
                    phase: Phase::Refresh,
                    ts_ms: crate::foundation::time::now_ms(),
                    path: configuration.source.clone(),
                    action: "archive".into(),
                    side: "source".into(),
                    message: format!("archive not refreshed — the source root would not open: {e}"),
                });
            }
        }
    }
    Ok(ApplyOutcome {
        done,
        skipped,
        errors,
        bytes_copied: bytes_done_total,
        cancelled: ctx.ctl.cancelled(),
    })
}
