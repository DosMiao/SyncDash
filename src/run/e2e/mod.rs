//! The pipeline contract as executable cases: seed two roots, drift one, sync, assert the result.
//!
//! This is the twin of `fs::vfs::conformance`. That suite asks whether a *backend* honours the
//! `Vfs` contract; this one asks whether the *pipeline* honours the mode contracts in `README.md`
//! when it runs on top of one. Both take the same shape — a factory producing fresh empty roots —
//! so a live lane can hand one closure to both and get the backend and the pipeline checked
//! together. Before this existed, every test covered a prefix or a suffix of scan → compare → plan
//! → apply and none covered the chain, so no test could have caught a mode contract being violated
//! end to end.
//!
//! Two deliberate departures from the older suite:
//!
//! **Skips are loud.** `conformance` early-returns on a missing capability, which makes a backend
//! that can do nothing look like a passing one. Here a case declares its `Need`s, an unmet need
//! reports `Outcome::Skipped`, and each lane pins its skip set exactly — so a backend that quietly
//! loses a capability turns a green run red. That matches how the rest of this codebase treats
//! exclusions: counted and visible, never silent.
//!
//! **A move is proved three ways.** The final tree is byte-identical whether the tool renamed a
//! file or copied then deleted it, so no tree assertion can tell them apart. A case that expects a
//! move asserts the plan said `Move`, that `bytes_copied` was zero, and that nothing was routed
//! into trash. Any one of the three alone would pass for the wrong reason.

pub mod cases;
pub mod corpus;
pub mod tree;

mod archive_tier;
mod guards;
mod lanes;
mod unicode;

use std::sync::Arc;

use crate::fs::vfs::Vfs;
use crate::job::Job;
use crate::model::plan::{Action, Op, Side};
use crate::obs::progress::{ApplyOutcome, RunCtl, RunCtx};

use corpus::{Edit, Seed};

/// What a case needs from the two backends before its assertions mean anything.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Need {
    SetMtime,
    Symlink,
    UnixMode,
    RangedRead,
    /// Sampled-divergence escalation only runs when both roots expose a local path, so a case that
    /// depends on it is meaningless anywhere else.
    BothLocal,
    /// The case's own timestamps must stay distinguishable on this root.
    MtimePrecisionAtMost(u32),
    /// The target can hold a root lock, and can therefore be written at all.
    ///
    /// Not declared by any case — it is a precondition of *applying*, which every case does, so the
    /// driver checks it for all of them. A backend that cannot set mtimes and is not local has no
    /// way to signal liveness to another machine, and the write side refuses rather than sync
    /// without one. Such a lane still compares, so it skips loudly instead of failing: "readable,
    /// never writable" is a real and supported shape, and the lane's own test pins the refusal.
    WritableTarget,
}

impl Need {
    fn met(self, s: &Arc<dyn Vfs>, t: &Arc<dyn Vfs>) -> bool {
        let (sc, tc) = (s.caps(), t.caps());
        match self {
            Need::SetMtime => sc.set_mtime.yes() && tc.set_mtime.yes(),
            Need::Symlink => sc.symlink.yes() && tc.symlink.yes(),
            Need::UnixMode => sc.unix_mode.yes() && tc.unix_mode.yes(),
            Need::RangedRead => sc.ranged_read.yes() && tc.ranged_read.yes(),
            Need::BothLocal => s.as_local().is_some() && t.as_local().is_some(),
            Need::MtimePrecisionAtMost(ms) => {
                sc.mtime_precision_ms <= ms && tc.mtime_precision_ms <= ms
            }
            // Mirrors the root-lock gate in `guard::caps`: a local root locks by other means, a
            // remote one needs `set_mtime` for the heartbeat.
            Need::WritableTarget => tc.set_mtime.yes() || t.as_local().is_some(),
        }
    }
}

/// How many bytes the apply was allowed to move. `None` is the move proof: a rename transfers
/// nothing, and `apply` only counts bytes for `Copy`/`Update`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Bytes {
    None,
    Any,
    AtLeast(u64),
}

/// One op the plan must contain. `reason` is matched as a prefix, so a case can pin just the base
/// reason or the whole decorated string as it prefers.
#[derive(Clone, Debug)]
pub struct ExpectOp {
    /// Which root the op is executed against. Part of the assertion because in `sync` mode the
    /// direction *is* the finding — an `Update` landing on Source rather than Target is a different
    /// verdict about who changed, not a detail.
    pub side: Side,
    pub action: Action,
    pub path: &'static str,
    pub from: Option<&'static str>,
    pub reason: &'static str,
}

#[derive(Clone, Debug)]
pub struct Expect {
    /// Asserted **exhaustively** — an op the plan carries and this list does not is a failure, the
    /// same as the reverse. Without that, a plan of `[Move, Copy, Delete]` would satisfy a case that
    /// only asked for a `Move`.
    pub ops: &'static [ExpectOp],
    pub bytes: Bytes,
    /// After a successful mirror the target must equal the source. Modes that legitimately diverge
    /// (enrich never deletes, so the target keeps its extras) set this false and say what is extra.
    pub target_equals_source: bool,
    pub extra_on_target: &'static [&'static str],
    /// Originals the run must have preserved. Empty is the third leg of the move proof.
    pub preserved: &'static [&'static str],
}

#[derive(Clone, Debug)]
pub struct Case {
    pub name: &'static str,
    pub seeds: &'static [Seed],
    pub source_edits: &'static [Edit],
    pub target_edits: &'static [Edit],
    pub mode: &'static str,
    pub rigor: &'static str,
    pub needs: &'static [Need],
    pub expect: Expect,
}

pub enum Outcome {
    Ran,
    Skipped(Need),
}

pub struct Report {
    pub lane: String,
    pub ran: Vec<&'static str>,
    pub skipped: Vec<(&'static str, Need)>,
}

/// A backend under test. Must return a **fresh, empty** root on every call; the harness asks for two
/// per case and never assumes they share a filesystem.
pub type Roots<'a> = &'a mut dyn FnMut() -> Arc<dyn Vfs>;

/// Everything the run said that was not routine, kept so a failing assertion can report *why* the
/// pipeline refused rather than only that it did. A capability report that blocks a write is a
/// sentence of explanation the engine already produced; throwing it away and then asserting on the
/// error count would make every refusal look identical.
#[derive(Clone, Default)]
pub struct Transcript(Arc<std::sync::Mutex<Vec<String>>>);

impl Transcript {
    pub fn sink(&self) -> Arc<dyn crate::obs::progress::ProgressSink> {
        let buf = self.0.clone();
        Arc::new(move |ev: crate::model::event::ProgressEvent| {
            use crate::model::event::{LogLevel, ProgressEvent as E};
            let line = match ev {
                E::Error { action, message, .. } => Some(format!("error[{action}] {message}")),
                E::Log { level, scope, message, .. } => match level {
                    LogLevel::Info => None,
                    _ => Some(format!("{level:?}[{scope}] {message}")),
                },
                _ => None,
            };
            if let Some(l) = line {
                buf.lock().unwrap().push(l);
            }
        })
    }

    /// The collected lines, indented for embedding in a panic message.
    pub fn text(&self) -> String {
        let b = self.0.lock().unwrap();
        if b.is_empty() {
            "  (the run said nothing)".to_string()
        } else {
            format!("  {}", b.join("\n  "))
        }
    }
}

/// A `RunCtx` that records what the run said.
pub fn watched() -> (Transcript, RunCtx) {
    let t = Transcript::default();
    let ctx = RunCtx::new(RunCtl::new(), t.sink());
    (t, ctx)
}

/// A job with every ambient influence switched off, so a case measures the pipeline and not the
/// defaults.
///
/// `Job::default()` carries the windows+macos junk presets and the real guard thresholds; both are
/// correct for a user and wrong for a test that asserts exact op counts. The guard cases turn the
/// thresholds back on one at a time, which is what makes those cases about the guard rather than
/// about the fixture.
///
/// Roots stay blank because everything below `compare_resolved` works on open backends and never
/// reads the phrases. `targets` stays empty deliberately: `compare_resolved` sits *below* the
/// multi-target check, so a stray entry here would silently test a shape production refuses.
///
pub fn bare_job() -> Job {
    Job {
        mode: "mirror".into(),
        rigor: "standard".into(),
        source: String::new(),
        target: String::new(),
        targets: Vec::new(),
        archive: None,
        include: Vec::new(),
        exclude: Vec::new(),
        require_marker: false,
        min_free_pct: 0.0,
        max_delete_ratio: 0.0,
        fsync: false,
        parallel: Some(1),
        ..Job::default()
    }
}

/// Run every case in `cases::ALL` against one lane.
pub fn run_all(lane: &str, mk: Roots<'_>) -> Report {
    let mut rep = Report { lane: lane.to_string(), ran: Vec::new(), skipped: Vec::new() };
    for c in cases::ALL {
        match run_case(lane, c, mk) {
            Outcome::Ran => rep.ran.push(c.name),
            Outcome::Skipped(n) => rep.skipped.push((c.name, n)),
        }
    }
    rep
}

pub fn run_case(lane: &str, case: &Case, mk: Roots<'_>) -> Outcome {
    let sv = mk();
    let tv = mk();
    if !Need::WritableTarget.met(&sv, &tv) {
        return Outcome::Skipped(Need::WritableTarget);
    }
    for n in case.needs {
        if !n.met(&sv, &tv) {
            return Outcome::Skipped(*n);
        }
    }
    let what = format!("[{lane}] {}", case.name);

    corpus::seed_into(&sv, case.seeds);
    corpus::seed_into(&tv, case.seeds);
    corpus::apply_edits(&sv, case.source_edits);
    corpus::apply_edits(&tv, case.target_edits);
    corpus::prune_empty_dirs(&sv, "");
    corpus::prune_empty_dirs(&tv, "");

    let mut job = Job { mode: case.mode.into(), rigor: case.rigor.into(), ..bare_job() };
    // A `Need` is two statements in one: what the backends must support, and what the job must
    // therefore ask for. Recording symlinks and comparing mode bits are both off by default, so a
    // case needing the capability needs the setting too — deriving it here keeps the two from being
    // declared separately and drifting. Turning either on globally is not an option: `symlinks =
    // "direct"` against a backend that cannot represent links is a hard Block, by design.
    for n in case.needs {
        match n {
            Need::Symlink => job.symlinks = "direct".into(),
            Need::UnixMode => job.sync_mode = true,
            _ => {}
        }
    }
    let (said, ctx) = watched();

    let out = super::local::compare_resolved(&job, &sv, &tv, &ctx, true)
        .unwrap_or_else(|e| panic!("{what}: compare failed: {e}\n{}", said.text()));
    assert_ops(&what, &out.plan.ops, case.expect.ops);

    // Conflict and Note rows describe the world; they are not work. The orchestrator drops them
    // before applying and so does this.
    let ops: Vec<Op> = out
        .plan
        .ops
        .iter()
        .filter(|o| !matches!(o.action, Action::Conflict | Action::Note))
        .cloned()
        .collect();

    // A per-case batch directory rather than the shared store: this suite runs alongside the rest of
    // the test binary, so it must not write into the developer's real trash, and two cases must
    // never see each other's preserved originals.
    let trash = std::env::temp_dir()
        .join(format!("syncdash-e2e-trash-{}-{lane}-{}", std::process::id(), case.name));
    let _ = std::fs::remove_dir_all(&trash);

    let ap = super::local::apply_resolved(
        &job,
        &out.plan,
        &ops,
        &sv,
        &tv,
        Some(trash.clone()),
        false,
        false,
        true,
        std::time::Instant::now(),
        &ctx,
    );
    assert_eq!(ap.errors, 0, "{what}: apply reported {} error(s)\n{}", ap.errors, said.text());
    assert!(!ap.cancelled, "{what}: apply reported itself cancelled");

    match case.expect.bytes {
        Bytes::None => assert_eq!(
            ap.bytes_copied, 0,
            "{what}: expected no bytes to cross — a move that copies is not a move"
        ),
        Bytes::AtLeast(n) => assert!(
            ap.bytes_copied >= n,
            "{what}: expected at least {n} bytes copied, got {}",
            ap.bytes_copied
        ),
        Bytes::Any => {}
    }

    let preserved = preserved_of(&tv, &trash);
    let want: Vec<String> = case.expect.preserved.iter().map(|s| s.to_string()).collect();
    assert_eq!(preserved, want, "{what}: preserved originals differ");
    let _ = std::fs::remove_dir_all(&trash);

    if case.expect.target_equals_source {
        let tol = tree::Tolerance::between(&sv, &tv);
        let mut want = tree::shape_of(&sv);
        for extra in case.expect.extra_on_target {
            want.retain(|s| &s.path != extra);
        }
        let got = tree::shape_of(&tv);
        tree::assert_same(&want, &got, &tol, &format!("{what}: target after sync"));
    }

    Outcome::Ran
}

/// Every op the plan carries, matched against every op the case expects. Reports the whole
/// disagreement at once rather than the first mismatch.
fn assert_ops(what: &str, got: &[Op], want: &[ExpectOp]) {
    let mut diffs: Vec<String> = Vec::new();
    let mut taken = vec![false; got.len()];

    for w in want {
        let hit = got.iter().enumerate().position(|(i, g)| {
            !taken[i]
                && g.side == w.side
                && g.action == w.action
                && g.path == w.path
                && g.from.as_deref() == w.from
                && g.reason.starts_with(w.reason)
        });
        match hit {
            Some(i) => taken[i] = true,
            None => diffs.push(format!(
                "expected but absent: {:?} {:?} {} from={:?} reason={:?}",
                w.side, w.action, w.path, w.from, w.reason
            )),
        }
    }
    for (i, g) in got.iter().enumerate() {
        if !taken[i] {
            diffs.push(format!(
                "unexpected op: {:?} {:?} {} from={:?} reason={:?}",
                g.side, g.action, g.path, g.from, g.reason
            ));
        }
    }
    if !diffs.is_empty() {
        panic!(
            "{what}: plan does not match ({} op(s) planned, {} expected)\n  {}",
            got.len(),
            want.len(),
            diffs.join("\n  ")
        );
    }
}

/// Originals the run set aside — over **both** routes, because which one is taken is a property of
/// the backend, not of the case. A root whose medium the central trash store can rename into gets
/// the explicit batch directory; everything else gets `<root>/.syncdash/trash/<ms>/` on its own
/// side. A case asserts *what* was preserved; checking only one door would silently pass every case
/// on the other kind of root.
///
/// An empty list is what distinguishes a real rename from a copy-and-delete that happens to leave
/// the same tree behind.
fn preserved_of(v: &Arc<dyn Vfs>, trash: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();

    let base = format!("{}/trash", crate::foundation::names::APP_DIR);
    if let Ok(runs) = v.read_dir(&base) {
        for run in runs {
            collect_vfs(v, &format!("{base}/{}", run.name), "", &mut out);
        }
    }
    collect_fs(&trash.join("target"), "", &mut out);

    out.sort();
    out
}

/// The explicit trash batch directory lives on the real filesystem regardless of what the roots
/// are, so it is walked with `std::fs` rather than through a `Vfs`.
fn collect_fs(dir: &std::path::Path, rel: &str, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let child = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
        if e.path().is_dir() {
            collect_fs(&e.path(), &child, out);
        } else {
            out.push(child);
        }
    }
}

fn collect_vfs(v: &Arc<dyn Vfs>, abs: &str, rel: &str, out: &mut Vec<String>) {
    let Ok(entries) = v.read_dir(abs) else { return };
    for e in entries {
        let child_abs = format!("{abs}/{}", e.name);
        let child_rel = if rel.is_empty() { e.name.clone() } else { format!("{rel}/{}", e.name) };
        if e.meta.kind == crate::model::table::EntryKind::Dir {
            collect_vfs(v, &child_abs, &child_rel, out);
        } else {
            out.push(child_rel);
        }
    }
}

/// Compare then apply, handing back everything the run said. Does **not** assert success — the
/// guard cases are about refusals, and a refusal is a result, not a failure.
pub fn try_cycle(
    job: &Job,
    sv: &Arc<dyn Vfs>,
    tv: &Arc<dyn Vfs>,
    acknowledged: bool,
) -> (crate::model::plan::Plan, ApplyOutcome, String) {
    let (said, ctx) = watched();
    let out = super::local::compare_resolved(job, sv, tv, &ctx, true)
        .unwrap_or_else(|e| panic!("compare: {e}\n{}", said.text()));
    let ops: Vec<Op> = out
        .plan
        .ops
        .iter()
        .filter(|o| !matches!(o.action, Action::Conflict | Action::Note))
        .cloned()
        .collect();
    let ap = super::local::apply_resolved(
        job,
        &out.plan,
        &ops,
        sv,
        tv,
        None,
        false,
        acknowledged,
        true,
        std::time::Instant::now(),
        &ctx,
    );
    (out.plan, ap, said.text())
}

/// One full sync cycle that is expected to succeed.
pub fn cycle(job: &Job, sv: &Arc<dyn Vfs>, tv: &Arc<dyn Vfs>) -> crate::model::plan::Plan {
    let (plan, ap, said) = try_cycle(job, sv, tv, false);
    assert_eq!(ap.errors, 0, "apply errored\n{said}");
    plan
}
