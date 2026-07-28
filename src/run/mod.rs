//! L3 orchestration: run one job, from two root phrases to a finished sync.
//!
//! The transport router lives here. A job either executes in this process — reading and writing
//! through the `Vfs` layer, whatever protocol the roots name — or it ships a package over ssh and
//! has a peer apply it against its own disk. `compare`, `preflight`, `apply` and `run_job` are the
//! only four places that choice is made; `local` and `peer` are the two sides of it.
//!
//! `local` is about *who executes*, not about where the bytes are: an `sftp://` root runs down the
//! local lane, because this process does the reading and writing.
//!
//! `roots` opens root phrases, `archive` refreshes the sync-mode archive after a successful apply.

pub mod archive;
pub mod local;
pub mod peer;
pub mod roots;

use crate::job::Job;

use crate::model::plan::{Op, Plan};
use crate::model::table::Snapshot;

use local::{apply_job_guarded_with, compare_job_detailed, preflight_job, run_local_job};
use peer::{apply_peer_job_with, compare_peer_job_detailed, preflight_peer_job, run_peer_job};
pub use roots::resolve_root;
use crate::pipeline::scan;

/// Rigor level → scan options. A monotone ladder: each tier = **more actually read this round**,
/// and the meaning of "identical ✓" climbs from "metadata measured" to "every byte measured".
/// quick    reads 0 bytes (size+mtime)
/// fast     really reads only the sampling window of changed faces (the cache covers unchanged ones)
/// standard really reads every file's sampling window this round (no cache — not memory, a measurement taken now)
/// paranoid really reads every byte of every file this round
pub fn scan_opts(job: &Job) -> scan::ScanOptions {
    let filter = crate::pipeline::filter::PathFilter::build_full(&job.include, &job.exclude, &job.deletable);
    let r = job.rigor_resolved();
    scan::ScanOptions { hash: r.hash, sampled: r.sampled, use_cache: r.use_cache, symlinks_direct: job.symlinks == "direct", filter }
}

/// Whether this job executes on a peer over ssh rather than here.
///
/// The distinction is the *transport*, not the protocol: a job whose target is `sftp://` still
/// runs here — this process does the reading and writing. A `peer://` target ships a package to
/// another machine and has it apply the plan against its own disk.
///
/// It reads the target phrase because that is where a root says where it lives. It used to read a
/// `remote_host` field instead, which meant the same question had two answers and a validation
/// rule whose only job was to stop them disagreeing.
pub fn is_peer_job(job: &Job) -> bool {
    crate::fs::vfs::spec::is_peer(&job.target)
}

/// The run-log `kind` for this job's compare or apply, so the history reads back correctly.
///
/// Says `remote-` where the rest of the code says peer, and deliberately: these strings are
/// **stored** in `runs.jsonl` and read back by `syncdash logs` and the desktop history. Renaming
/// the producer without migrating every record already on disk would split one history into two
/// vocabularies. The single producer is here, so the day that migration is written, this is the
/// only line that changes.
pub fn run_kind(job: &Job, verb: &str) -> String {
    if is_peer_job(job) { format!("remote-{verb}") } else { verb.to_string() }
}

/// Compare, whichever side does the scanning.
pub fn compare(
    name: &str,
    job: &Job,
    ctx: &crate::obs::progress::RunCtx,
    accept_caps: bool,
) -> std::io::Result<CompareOutcome> {
    if is_peer_job(job) {
        // The peer scans its own disk and sends back a table; capability consent is a property of
        // the roots this process opens, so it has nothing to apply to.
        compare_peer_job_detailed(name, job, ctx)
    } else {
        compare_job_detailed(job, ctx, accept_caps)
    }
}

/// Gate a plan before applying it.
pub fn preflight(job: &Job, plan: &Plan, ops: &[Op], acknowledged: bool) -> crate::pipeline::guard::Verdict {
    if is_peer_job(job) {
        preflight_peer_job(job, plan, ops, acknowledged)
    } else {
        preflight_job(job, plan, ops, acknowledged)
    }
}

/// Apply, whichever side does the writing.
#[allow(clippy::too_many_arguments)] // every one is a distinct decision the caller has already made
pub fn apply(
    name: &str,
    job: &Job,
    plan: &Plan,
    ops: &[Op],
    trash: Option<std::path::PathBuf>,
    verbose: bool,
    acknowledged: bool,
    accept_caps: bool,
    ctx: &crate::obs::progress::RunCtx,
) -> std::io::Result<crate::obs::progress::ApplyOutcome> {
    if is_peer_job(job) {
        apply_peer_job_with(name, job, plan, ops, verbose, acknowledged, ctx)
    } else {
        Ok(apply_job_guarded_with(job, plan, ops, trash, verbose, acknowledged, accept_caps, ctx))
    }
}

/// One job end to end: compare, then apply if asked. Returns (done, skipped, errors, conflicts).
pub fn run_job(
    name: &str,
    job: &Job,
    do_apply: bool,
    verbose: bool,
    acknowledged: bool,
    accept_caps: bool,
) -> std::io::Result<(u64, u64, u64, u64)> {
    if is_peer_job(job) {
        run_peer_job(name, job, do_apply, verbose, acknowledged)
    } else {
        run_local_job(name, job, do_apply, verbose, acknowledged, accept_caps)
    }
}

pub fn compare_job(job: &Job) -> std::io::Result<Plan> {
    compare_job_with(job, &crate::obs::progress::RunCtx::null())
}

/// One connected backend's capability sheet, rendered for `syncdash caps`.
pub fn describe_root(phrase: &str) -> std::io::Result<String> {
    use std::fmt::Write as _;
    let v = resolve_root(phrase)?;
    let c = v.caps();
    let mut out = String::new();
    let _ = writeln!(out, "root:      {}", v.display());
    let _ = writeln!(out, "identity:  {}", v.identity());
    if let Some(info) = v.server_info() {
        let _ = writeln!(out, "server:    {info}");
    }
    let _ = writeln!(out, "protocol:  {}", c.protocol);
    let _ = writeln!(out, "local lane: {}", if v.as_local().is_some() { "yes (fast path)" } else { "no (generic VFS lane)" });
    // The medium and the trash destination are printed together because one decides the other,
    // and "where do my deleted files go" is the question this sheet most often has to answer.
    let _ = writeln!(
        out,
        "medium:    {}  (preserved originals → {})",
        c.medium.as_str(),
        if c.local_trash { "this machine's trash store" } else { "<root>/.syncdash/trash/<run>/, by rename" }
    );
    let _ = writeln!(out, "mtime precision: {} ms", c.mtime_precision_ms);
    let _ = writeln!(out, "name rules: {}{}", c.name_rules.as_str(), match c.name_rules {
        crate::fs::vfs::NameRules::Windows =>
            "  (no <>:\"|?*, no trailing dot or space, no reserved device names — such paths are refused at plan time)",
        crate::fs::vfs::NameRules::Posix => "",
        crate::fs::vfs::NameRules::Unknown =>
            "  (this protocol does not reveal the server's OS — risky names are warned about, not refused)",
    });
    let cap = |s: crate::fs::vfs::Support| match s {
        crate::fs::vfs::Support::Yes => "yes",
        crate::fs::vfs::Support::No => "NO",
        crate::fs::vfs::Support::Unknown => "unknown",
    };
    let _ = writeln!(out, "set_mtime {}  fsync {}  rename_overwrite {}  ranged_read {}  write_at {}",
        cap(c.set_mtime), cap(c.fsync), cap(c.rename_overwrite), cap(c.ranged_read), cap(c.write_at));
    let _ = writeln!(out, "unix_mode {}  symlink {}  file_id {}  free_space {}  read_back {}",
        cap(c.unix_mode), cap(c.symlink), cap(c.file_id), cap(c.free_space), cap(c.read_back));
    let _ = write!(out, "parallel streams: up to {}", c.max_parallel_streams);
    Ok(out)
}

/// The full output of a comparison: the plan plus both snapshots.
/// The desktop shell needs the snapshots to compute the "evidence layer" (both sides' size/mtime, identical items); the CLI only wants the plan.
pub struct CompareOutcome {
    pub plan: Plan,
    pub source: Snapshot,
    pub target: Snapshot,
}

/// v0.9 M1: the end-to-end comparison with an event stream and cancellation. This function replaces the second
/// pipeline src-tauri had inlined just to emit events (undoing the two-copy drift).
pub fn compare_job_with(job: &Job, ctx: &crate::obs::progress::RunCtx) -> std::io::Result<Plan> {
    compare_job_detailed(job, ctx, false).map(|o| o.plan)
}

/// Execute the selected ops; on complete success in sync mode, refresh the archive (conflicted paths are dropped from it, so the conflict is reported again next time).
pub fn apply_job(job: &Job, plan: &Plan, ops: &[Op], trash: Option<std::path::PathBuf>, verbose: bool) -> (u64, u64, u64) {
    apply_job_guarded(job, plan, ops, trash, verbose, false, false)
}

/// `acknowledged` = the user passed --i-know explicitly; it only allows the "plan health check" gates through.
/// A missing marker and insufficient disk space always block (those are environment problems, not judgment calls).
pub fn apply_job_guarded(
    job: &Job,
    plan: &Plan,
    ops: &[Op],
    trash: Option<std::path::PathBuf>,
    verbose: bool,
    acknowledged: bool,
    accept_caps: bool,
) -> (u64, u64, u64) {
    apply_job_guarded_with(job, plan, ops, trash, verbose, acknowledged, accept_caps, &crate::obs::progress::RunCtx::null()).into_tuple()
}

/// v0.9 M3: the **compare stage** of a remote job — the desktop's `compare_job` routes remote jobs straight here
/// instead of silently dropping into the local pipeline (which would pull the data over UNC and rehash it: an order of magnitude slower, and semantically wrong).
pub fn compare_peer_job_with(name: &str, job: &Job, ctx: &crate::obs::progress::RunCtx) -> std::io::Result<Plan> {
    compare_peer_job_detailed(name, job, ctx).map(|o| o.plan)
}
