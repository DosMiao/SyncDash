//! The capability report: every gap between what the job asks for and what the two backends can
//! give, listed **before** any scanning starts.
//!
//! Blockers refuse the run, `NeedsAck` lines demand explicit consent, `Info` lines go to the log.
//! Nothing degrades silently — that is the whole contract.
//!
//! Primitives only: this layer must not know the Job schema. `Job::read_caps_query` and
//! `Job::write_caps_query` build the queries and hand them down.

use serde::{Deserialize, Serialize};

use crate::model::plan::{Action, Op, Side};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapSeverity {
    /// Running would make the table or the plan lie — refuse outright.
    Block,
    /// Runnable, but only with the user's explicit consent (`--accept-caps` / a ticked box).
    NeedsAck,
    /// Stated for the record; nothing to decide.
    Info,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CapItem {
    /// The job feature concerned ("evidence=sampled", "mtime window", …)
    pub feature: String,
    /// "source" | "target" | "both"
    pub side: String,
    pub severity: CapSeverity,
    /// What the job asked for
    pub requested: String,
    /// What the backend can give
    pub actual: String,
    /// What this run will therefore do — in plain words, shown verbatim
    pub effect: String,
}

impl CapItem {
    pub fn render(&self) -> String {
        format!("[{}] {}: wanted {}, backend has {} — {}", self.side, self.feature, self.requested, self.actual, self.effect)
    }
}

#[derive(Clone, Debug, Default)]
pub struct CapReport {
    pub items: Vec<CapItem>,
}

impl CapReport {
    pub fn blockers(&self) -> Vec<&CapItem> {
        self.items.iter().filter(|i| i.severity == CapSeverity::Block).collect()
    }
    pub fn needs_ack(&self) -> Vec<&CapItem> {
        self.items.iter().filter(|i| i.severity == CapSeverity::NeedsAck).collect()
    }
    pub fn infos(&self) -> Vec<&CapItem> {
        self.items.iter().filter(|i| i.severity == CapSeverity::Info).collect()
    }
}

/// What the read side of a run asks of its backends — primitives only (this layer
/// must not know the Job schema; `Job::read_caps_query` builds one).
#[derive(Clone, Copy, Debug)]
pub struct ReadCapsQuery {
    pub hash: bool,
    pub sampled: bool,
    pub escalate: bool,
    pub symlinks_direct: bool,
    pub min_free_pct: f64,
    /// The effective no-hash mtime window (already widened to the coarser backend)
    pub window_ms: i64,
    pub src_local: bool,
    pub tgt_local: bool,
}

/// The read-side capability comparison. Write-side items (fsync, verify, trash,
/// delta, chmod) join when the VFS apply lane lands.
pub fn cap_report_read(
    q: &ReadCapsQuery,
    src: &crate::fs::vfs::VfsCaps,
    tgt: &crate::fs::vfs::VfsCaps,
) -> CapReport {
    use crate::fs::vfs::Support;
    let mut r = CapReport::default();

    // symlinks="direct" needs the backend to *see* links, or the table paints a false picture
    for (side, c) in [("source", src), ("target", tgt)] {
        if q.symlinks_direct && c.symlink == Support::No {
            r.items.push(CapItem {
                feature: "symlinks=direct".into(),
                side: side.into(),
                severity: CapSeverity::Block,
                requested: "symlinks recorded in the table".into(),
                actual: "backend cannot represent symlinks".into(),
                effect: "the table would silently omit every link — refusing to produce a false picture".into(),
            });
        }
    }

    // sampled evidence needs ranged reads; without them the scan upgrades to full reads, out loud
    if q.hash && q.sampled {
        for (side, c) in [("source", src), ("target", tgt)] {
            if c.ranged_read == Support::No {
                r.items.push(CapItem {
                    feature: "evidence=sampled".into(),
                    side: side.into(),
                    severity: CapSeverity::NeedsAck,
                    requested: "3-window sampled digests".into(),
                    actual: "no ranged reads".into(),
                    effect: "sampled digests only match other sampled digests, so BOTH sides upgrade to full reads — every changed file is read whole over its link; set evidence=none to skip content reads instead".into(),
                });
            }
        }
    }

    // A widened no-hash window is a blind spot exactly when there is no content evidence
    if q.window_ms > crate::model::plan::MTIME_SLACK_MS {
        let secs = q.window_ms / 1000;
        r.items.push(CapItem {
            feature: "mtime window".into(),
            side: "both".into(),
            severity: if q.hash { CapSeverity::Info } else { CapSeverity::NeedsAck },
            requested: format!("±{}s equality window", crate::model::plan::MTIME_SLACK_MS / 1000),
            actual: format!("backend timestamps are only ±{secs}s precise"),
            effect: if q.hash {
                format!("timestamp equality widens to ±{secs}s; content evidence still decides")
            } else {
                format!("with evidence=none, edits within {secs}s of each other are INVISIBLE to this comparison")
            },
        });
    }

    if q.escalate && q.sampled && (!q.src_local || !q.tgt_local) {
        r.items.push(CapItem {
            feature: "escalate".into(),
            side: "both".into(),
            severity: CapSeverity::Info,
            requested: "full re-read on sampled-digest/mtime disagreement".into(),
            actual: "a root lives on a remote backend".into(),
            effect: "escalation is skipped this run (it arrives with the VFS write lane)".into(),
        });
    }

    if q.min_free_pct > 0.0 {
        for (side, c) in [("source", src), ("target", tgt)] {
            if c.free_space == Support::No {
                r.items.push(CapItem {
                    feature: "min_free_pct".into(),
                    side: side.into(),
                    severity: CapSeverity::Info,
                    requested: "free-space gate before writing".into(),
                    actual: "backend cannot report free space".into(),
                    effect: "the space gate is inert on this side — writes proceed without it".into(),
                });
            }
        }
    }

    r
}

/// What the write side of a run asks of its backends (`Job::write_caps_query` builds one).
#[derive(Clone, Copy, Debug)]
pub struct WriteCapsQuery {
    pub fsync: bool,
    pub verify: bool,
    pub versioning: bool,
    pub delta: bool,
    pub src_local: bool,
    pub tgt_local: bool,
}

/// The write-side capability comparison, evaluated against the ops that will actually
/// run. Joins the read-side report before the apply stage starts.
pub fn cap_report_write(
    q: &WriteCapsQuery,
    ops: &[Op],
    src: &crate::fs::vfs::VfsCaps,
    tgt: &crate::fs::vfs::VfsCaps,
) -> CapReport {
    use crate::fs::vfs::Support;
    let mut r = CapReport::default();

    for (side, side_tag, caps, local) in [
        (Side::Source, "source", src, q.src_local),
        (Side::Target, "target", tgt, q.tgt_local),
    ] {
        let side_ops: Vec<&Op> = ops
            .iter()
            .filter(|o| o.side == side && !matches!(o.action, Action::Conflict | Action::Note))
            .collect();
        if side_ops.is_empty() {
            continue;
        }

        // A plan the backend cannot execute in full is a plan the table would lie about
        if caps.unix_mode == Support::No && side_ops.iter().any(|o| o.action == Action::Chmod) {
            r.items.push(CapItem {
                feature: "chmod ops".into(),
                side: side_tag.into(),
                severity: CapSeverity::Block,
                requested: format!("{} permission change(s) from the plan", side_ops.iter().filter(|o| o.action == Action::Chmod).count()),
                actual: "backend has no unix modes".into(),
                effect: "the plan cannot be executed in full — refusing rather than silently dropping ops".into(),
            });
        }
        if caps.symlink == Support::No && side_ops.iter().any(|o| o.link.is_some()) {
            r.items.push(CapItem {
                feature: "symlink ops".into(),
                side: side_tag.into(),
                severity: CapSeverity::Block,
                requested: "symlink creation from the plan".into(),
                actual: "backend cannot create symlinks".into(),
                effect: "the plan cannot be executed in full — refusing rather than silently dropping ops".into(),
            });
        }

        // The root-lock heartbeat rides set_mtime; a backend that cannot set mtimes
        // cannot prove it is alive to the other machine — writing there is refused
        if caps.set_mtime == Support::No && !local {
            r.items.push(CapItem {
                feature: "root lock".into(),
                side: side_tag.into(),
                severity: CapSeverity::Block,
                requested: "a heartbeat other machines can observe".into(),
                actual: "backend cannot set mtimes (FTP without MFMT)".into(),
                effect: "the lock protocol cannot signal liveness — refusing to write; comparing (read-only) still works".into(),
            });
        }

        if q.fsync {
            match caps.fsync {
                Support::No => r.items.push(CapItem {
                    feature: "fsync=true".into(),
                    side: side_tag.into(),
                    severity: CapSeverity::NeedsAck,
                    requested: "fsync before rename".into(),
                    actual: "backend has no fsync".into(),
                    effect: "renamed files may not be durable across a crash on this side — continuing means accepting the server's own caching".into(),
                }),
                Support::Unknown => r.items.push(CapItem {
                    feature: "fsync=true".into(),
                    side: side_tag.into(),
                    severity: CapSeverity::NeedsAck,
                    requested: "fsync before rename".into(),
                    actual: "support unknown until tried".into(),
                    effect: "fsync is attempted per file; where the server refuses it, that file counts as failed (set fsync=false to skip the attempt)".into(),
                }),
                Support::Yes => {}
            }
        }

        if q.verify && caps.read_back == Support::No {
            r.items.push(CapItem {
                feature: "verify_writes".into(),
                side: side_tag.into(),
                severity: CapSeverity::NeedsAck,
                requested: "re-read the staged file before rename".into(),
                actual: "backend cannot read the staged file back".into(),
                effect: "verification degrades to the copy-stream hash plus a length reconciliation — no on-disk read-back".into(),
            });
        }

        let destructive = side_ops
            .iter()
            .any(|o| matches!(o.action, Action::Delete | Action::Update | Action::Copy));
        if destructive {
            // The version store writes into `<root>/.version_syncDash/`, so it only needs a real
            // path — it works on a mounted share. A protocol backend has no path at all.
            if q.versioning && !local {
                r.items.push(CapItem {
                    feature: "versioning".into(),
                    side: side_tag.into(),
                    severity: CapSeverity::NeedsAck,
                    requested: "rdelta version store on this root".into(),
                    actual: "remote backend (no local version machinery)".into(),
                    effect: "overwritten/deleted files are kept as whole files under <root>/.syncdash/trash/<run>/ instead — recover with any file browser; rdelta history does not accrue on this side".into(),
                });
            } else if !q.versioning && !caps.local_trash {
                // Deliberately NOT gated on `local`: a UNC root is a real path, which is exactly
                // why this went unnoticed — the store is on THIS machine, so preserving there
                // would have pulled every deleted file across the link first.
                r.items.push(CapItem {
                    feature: "trash".into(),
                    side: side_tag.into(),
                    severity: CapSeverity::NeedsAck,
                    requested: "deleted/overwritten files into the central trash store".into(),
                    // Reads through the template as "wanted …, backend has a network share, the
                    // central store is on this machine — …"
                    actual: format!("a {}, and the central store is on this machine", caps.medium.as_str()),
                    effect: "originals are renamed into <root>/.syncdash/trash/<run>/ on the root's own side instead — recoverable with any file browser, and nothing crosses the link".into(),
                });
            }
        }
    }

    if q.delta && !(q.src_local && q.tgt_local) {
        r.items.push(CapItem {
            feature: "delta".into(),
            side: "both".into(),
            severity: CapSeverity::NeedsAck,
            requested: "chunk-wise delta updates".into(),
            actual: "a root lives on a remote backend".into(),
            effect: "delta is disabled this run — updates rewrite files in full over the link".into(),
        });
    }

    r
}
