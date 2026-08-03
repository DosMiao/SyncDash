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
/// Focused pipeline smoke and safety checks; test fixtures do not ship in the binary.
#[cfg(test)]
pub mod e2e;
pub mod history;
pub mod local;
pub mod peer;
pub mod roots;
pub mod watch;

use crate::job::{Job, SingleTargetJob};

use crate::model::plan::{Op, Plan};
use crate::model::table::TableArtifact;

use crate::pipeline::{compare::CompareOptions, scan};
use local::{compare_job_detailed, preflight_job, run_local_job};
use peer::{compare_peer_job_detailed, preflight_peer_job, run_peer_job};
pub use roots::resolve_root;

#[derive(Clone, Debug)]
pub struct ApplyRequirements {
    pub verdict: crate::pipeline::guard::Verdict,
    pub capabilities: crate::pipeline::guard::caps::CapReport,
}

/// The point an Apply reached relative to the first operation that may change synchronized state.
///
/// Desktop keeps a reviewed Compare result after a refusal only when the engine proves it stopped
/// at `BeforeWrite`. Counters and error messages cannot provide that proof: a first write can fail
/// with the same zero-done outcome as a capability gate, and peer transport errors can happen on
/// either side of the peer `apply-pack` invocation.
#[derive(Debug)]
pub enum ApplyExecution {
    BeforeWrite(ApplyCompletion),
    WriteStarted(ApplyCompletion),
}

#[derive(Debug)]
pub enum ApplyCompletion {
    Outcome(crate::obs::progress::ApplyOutcome),
    Error(std::io::Error),
}

impl ApplyExecution {
    pub(crate) fn rejected(outcome: crate::obs::progress::ApplyOutcome) -> Self {
        Self::BeforeWrite(ApplyCompletion::Outcome(outcome))
    }

    pub(crate) fn started(outcome: crate::obs::progress::ApplyOutcome) -> Self {
        Self::WriteStarted(ApplyCompletion::Outcome(outcome))
    }

    pub(crate) fn failed_before_write(error: std::io::Error) -> Self {
        Self::BeforeWrite(ApplyCompletion::Error(error))
    }

    pub(crate) fn failed_after_write(error: std::io::Error) -> Self {
        Self::WriteStarted(ApplyCompletion::Error(error))
    }

    pub fn writes_started(&self) -> bool {
        matches!(self, Self::WriteStarted(_))
    }

    pub fn into_result(self) -> std::io::Result<crate::obs::progress::ApplyOutcome> {
        match self {
            Self::BeforeWrite(ApplyCompletion::Outcome(outcome))
            | Self::WriteStarted(ApplyCompletion::Outcome(outcome)) => Ok(outcome),
            Self::BeforeWrite(ApplyCompletion::Error(error))
            | Self::WriteStarted(ApplyCompletion::Error(error)) => Err(error),
        }
    }
}

/// Rigor level → scan options. A monotone ladder: each tier = **more actually read this round**,
/// and the meaning of "identical ✓" climbs from "metadata measured" to "every byte measured".
/// quick    reads 0 bytes (size+mtime)
/// fast     really reads only the sampling window of changed faces (the cache covers unchanged ones)
/// standard really reads every file's sampling window this round (no cache — not memory, a measurement taken now)
/// paranoid really reads every byte of every file this round
pub fn scan_opts(job: &Job) -> scan::ScanOptions {
    let filter =
        crate::pipeline::filter::PathFilter::build_full(&job.include, &job.exclude, &job.deletable);
    let r = job.rigor_resolved();
    scan::ScanOptions {
        hash: r.hash,
        sampled: r.sampled,
        use_cache: r.use_cache,
        symlinks_direct: job.symlinks == "direct",
        filter,
    }
}

/// The scan options a comparison across **these two roots** actually runs at.
///
/// `scan_opts` answers "what did the job ask for"; this answers "what can the pair deliver", and
/// every caller that touches a real pair of backends wants the second. A sampled digest is
/// `~`-prefixed precisely so it can only ever match another sampled digest, so when either backend
/// cannot do ranged reads, *both* sides read in full — a one-sided upgrade would make an identical
/// file look different, which is the exact kind of lie this tool exists not to tell.
///
/// It is one function, rather than `scan_opts` plus a narrowing step spelled out per site, because
/// the archive is compared against exactly these digests and so has to be written at exactly this
/// tier. While the two were derived separately they drifted: the comparison upgraded to full, the
/// archive refresh rescanned the source alone and kept sampling, and every file over the sampling
/// floor became permanently unequal — surfacing as a delete-versus-edit conflict no policy can
/// resolve. Leaving the narrowing as a separate call anyone could forget is how that happened.
pub fn effective_scan_opts(
    job: &Job,
    sv: &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
    tv: &std::sync::Arc<dyn crate::fs::vfs::Vfs>,
) -> scan::ScanOptions {
    let mut opt = scan_opts(job);
    if opt.sampled && !(sv.caps().ranged_read.yes() && tv.caps().ranged_read.yes()) {
        opt.sampled = false;
    }
    opt
}

/// The `none` | `sampled` | `full` vocabulary, from the options that produced a scan.
///
/// One mapping, three readers: the snapshot header stamps it (`TableHeader.evidence`), the
/// archive is checked against it on the way back in, and the peer lane sends it to the far side as
/// `--evidence`. Spelled out separately, those drift into disagreeing about what a tier is called.
pub fn evidence_label(opt: &scan::ScanOptions) -> &'static str {
    if !opt.hash {
        "none"
    } else if opt.sampled {
        "sampled"
    } else {
        "full"
    }
}

fn load_comparison_archive(
    job: &Job,
    options: &scan::ScanOptions,
    ctx: &crate::obs::progress::RunCtx,
) -> std::io::Result<Option<TableArtifact>> {
    if job.mode != "sync" {
        return Ok(None);
    }
    let Some(path) = &job.archive else {
        return Ok(None);
    };
    let Some(archive) = archive::load_archive(path)? else {
        return Ok(None);
    };

    let expected = evidence_label(options);
    let actual = archive.header.evidence.as_str();
    if actual != expected {
        ctx.log(
            crate::model::event::LogLevel::Warn,
            "compare",
            format!(
                "archive was written with {actual} evidence but this run compares at {expected} — \
                 the two cannot be matched, so it is being ignored for this run. Sync falls back \
                 to safe mode (fills both ways, reports differences, deletes nothing). The next \
                 successful run rewrites it at {expected}."
            ),
        );
        return Ok(None);
    }
    Ok(Some(archive))
}

/// Whether this job executes on a peer over ssh rather than here.
///
/// The distinction is the *transport*, not the protocol: a job whose target is `sftp://` still
/// runs here — this process does the reading and writing. A `peer://` target ships a package to
/// another machine and has it apply the plan against its own disk.
///
/// It reads the canonical target phrases because that is where each root says where it lives. A
/// valid peer job has exactly one target; using `any` here still reports the transport honestly for
/// an invalid hand-edited job before validation explains why it cannot run.
pub fn is_peer_job(job: &Job) -> bool {
    job.targets
        .iter()
        .any(|target| crate::fs::vfs::spec::is_peer(target))
}

fn is_peer_target(job: &SingleTargetJob) -> bool {
    crate::fs::vfs::spec::is_peer(job.target())
}

pub fn compare_run_kind(job: &SingleTargetJob) -> crate::run::history::RunKind {
    if is_peer_target(job) {
        crate::run::history::RunKind::PeerCompare
    } else {
        crate::run::history::RunKind::Compare
    }
}

pub fn apply_run_kind(job: &SingleTargetJob) -> crate::run::history::RunKind {
    if is_peer_target(job) {
        crate::run::history::RunKind::PeerApply
    } else {
        crate::run::history::RunKind::Apply
    }
}

pub fn compare(
    name: &str,
    job: &SingleTargetJob,
    ctx: &crate::obs::progress::RunCtx,
) -> std::io::Result<CompareOutcome> {
    if is_peer_target(job) {
        compare_peer_job_detailed(name, job, ctx)
    } else {
        compare_job_detailed(job, ctx)
    }
}

pub fn compare_capabilities(
    job: &SingleTargetJob,
) -> std::io::Result<crate::pipeline::guard::caps::CapReport> {
    if is_peer_target(job) {
        Ok(crate::pipeline::guard::caps::CapReport::default())
    } else {
        local::compare_capabilities(job)
    }
}

/// Gate a plan before applying it.
pub fn preflight(
    job: &SingleTargetJob,
    plan: &Plan,
    ops: &[Op],
) -> std::io::Result<crate::pipeline::guard::Verdict> {
    if is_peer_target(job) {
        Ok(preflight_peer_job(job, plan, ops))
    } else {
        preflight_job(job, plan, ops)
    }
}

pub fn apply_requirements(
    job: &SingleTargetJob,
    plan: &Plan,
    ops: &[Op],
) -> std::io::Result<ApplyRequirements> {
    if is_peer_target(job) {
        Ok(ApplyRequirements {
            verdict: preflight_peer_job(job, plan, ops),
            capabilities: peer::apply_capabilities(job, ops),
        })
    } else {
        local::apply_requirements(job, plan, ops)
    }
}

/// Apply with an explicit mutation boundary. New interactive callers should use this form so a
/// refused run can preserve its reviewed Compare result without guessing from text or counters.
#[allow(clippy::too_many_arguments)] // each argument is an independently reviewed apply decision
pub fn apply_classified(
    name: &str,
    job: &SingleTargetJob,
    plan: &Plan,
    ops: &[Op],
    trash: Option<std::path::PathBuf>,
    verbose: bool,
    ctx: &crate::obs::progress::RunCtx,
) -> ApplyExecution {
    if is_peer_target(job) {
        peer::apply_peer_job_with_classified(name, job, plan, ops, verbose, ctx)
    } else {
        local::apply_job_guarded_with_classified(job, plan, ops, trash, verbose, ctx)
    }
}

/// One job end to end: compare, then apply if asked. Returns (done, skipped, errors, conflicts).
pub fn run_job(
    name: &str,
    job: &Job,
    do_apply: bool,
    verbose: bool,
) -> std::io::Result<(u64, u64, u64, u64)> {
    validate_job(job)?;
    if is_peer_job(job) {
        let target = job
            .select_target(0)
            .map_err(|reason| std::io::Error::new(std::io::ErrorKind::InvalidInput, reason))?;
        run_peer_job(name, &target, do_apply, verbose)
    } else {
        run_local_job(name, job, do_apply, verbose)
    }
}

fn validate_job(job: &Job) -> std::io::Result<()> {
    job.validate()
        .map_err(|reason| std::io::Error::new(std::io::ErrorKind::InvalidInput, reason))
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
    let _ = writeln!(
        out,
        "local lane: {}",
        if v.local_root().is_some() {
            "yes (retained root descriptor)"
        } else {
            "no (generic VFS lane)"
        }
    );
    // The medium and the trash destination are printed together because one decides the other,
    // and "where do my deleted files go" is the question this sheet most often has to answer.
    let _ = writeln!(
        out,
        "medium:    {}  (preserved originals → {})",
        c.medium.as_str(),
        if c.local_trash {
            "this machine's trash store"
        } else {
            "<root>/.syncdash/trash/<run>/, by rename"
        }
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
    let _ = writeln!(
        out,
        "set_mtime {}  fsync {}  rename_overwrite {}  ranged_read {}  write_at {}",
        cap(c.set_mtime),
        cap(c.fsync),
        cap(c.rename_overwrite),
        cap(c.ranged_read),
        cap(c.write_at)
    );
    let _ = writeln!(
        out,
        "exclusive_staged_file_publish {}  exclusive_entry_rename {}  exclusive_symlink_publish {}  durable_namespace {}",
        cap(c.exclusive_staged_file_publish),
        cap(c.exclusive_entry_rename),
        cap(c.exclusive_symlink_publish),
        cap(c.durable_namespace)
    );
    let _ = writeln!(
        out,
        "unix_mode {}  symlink {}  file_id {}  free_space {}  read_back {}",
        cap(c.unix_mode),
        cap(c.symlink),
        cap(c.file_id),
        cap(c.free_space),
        cap(c.read_back)
    );
    let _ = write!(out, "parallel streams: up to {}", c.max_parallel_streams);
    Ok(out)
}

/// The full output of a comparison: the plan, both snapshots, and the exact options that judged
/// them. The desktop shell must reuse those options when it derives or pages evidence; reconstructing
/// them later would lose capability-driven adjustments such as a protocol's timestamp precision.
pub struct CompareOutcome {
    pub plan: Plan,
    pub source: TableArtifact,
    pub target: TableArtifact,
    pub compare_options: CompareOptions,
}

/// Scan the target on its peer instead of treating the phrase as a locally mounted root.
pub fn compare_peer_job_with(
    name: &str,
    job: &SingleTargetJob,
    ctx: &crate::obs::progress::RunCtx,
) -> std::io::Result<Plan> {
    compare_peer_job_detailed(name, job, ctx).map(|o| o.plan)
}
